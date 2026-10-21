//! Where the advancement screen draws each displayed node of a tree: a
//! tidy tree drawing in linear time, depth along x and siblings stacked
//! along y. Nodes without a display are skipped, their displayed
//! descendants hanging from the nearest displayed ancestor. The build
//! script lays out the compiled-in advancements the same way.

use super::Id;

const NONE: usize = usize::MAX;

/// A displayed node, linked into first-child/sibling lists.
#[derive(Clone, Copy)]
struct Slot {
    item: Id,
    depth: i32,
    up: usize,
    head: usize,
    tail: usize,
    before: usize,
    after: usize,
    rank: u32,
    pos: f32,
    offset: f32,
    drift: f32,
    push: f32,
    lead: usize,
    link: usize,
}

/// One of the four contours walked while two neighbouring subtrees are
/// pressed together, with the offsets summed along it.
#[derive(Clone, Copy)]
struct Walker {
    at: usize,
    sum: f32,
}

/// The positions of the displayed nodes below (and including) `root`,
/// which must be displayed.
pub fn layout<'a>(
    root: Id,
    children: &impl Fn(Id) -> &'a [Id],
    displayed: &impl Fn(Id) -> bool,
) -> Vec<(Id, [f32; 2])> {
    let mut sheet = Sheet(Vec::new());
    sheet.open(root, NONE, 0);
    for &child in children(root) {
        sheet.gather(0, child, children, displayed);
    }
    for v in (0..sheet.0.len()).rev() {
        sheet.arrange_children(v);
    }
    if sheet.0[0].head != NONE {
        sheet.0[0].pos = sheet.centre(0);
    }
    sheet.settle()
}

/// Every displayed node in preorder.
struct Sheet(Vec<Slot>);

impl Sheet {
    fn open(&mut self, item: Id, up: usize, depth: i32) -> usize {
        let at = self.0.len();
        let before = if up == NONE { NONE } else { self.0[up].tail };
        self.0.push(Slot {
            item,
            depth,
            up,
            head: NONE,
            tail: NONE,
            before,
            after: NONE,
            rank: if before == NONE {
                1
            } else {
                self.0[before].rank + 1
            },
            pos: 0.0,
            offset: 0.0,
            drift: 0.0,
            push: 0.0,
            lead: at,
            link: NONE,
        });
        if up != NONE {
            if before == NONE {
                self.0[up].head = at;
            } else {
                self.0[before].after = at;
            }
            self.0[up].tail = at;
        }
        at
    }

    fn gather<'a>(
        &mut self,
        host: usize,
        item: Id,
        children: &impl Fn(Id) -> &'a [Id],
        displayed: &impl Fn(Id) -> bool,
    ) {
        let host = if displayed(item) {
            let depth = self.0[host].depth + 1;
            self.open(item, host, depth)
        } else {
            host
        };
        for &child in children(item) {
            self.gather(host, child, children, displayed);
        }
    }

    fn centre(&self, v: usize) -> f32 {
        let slot = &self.0[v];
        (self.0[slot.head].pos + self.0[slot.tail].pos) / 2.0
    }

    fn seat(&mut self, v: usize) {
        let before = self.0[v].before;
        let after_before = if before == NONE {
            0.0
        } else {
            self.0[before].pos + 1.0
        };
        if self.0[v].head == NONE {
            self.0[v].pos = after_before;
        } else if before == NONE {
            self.0[v].pos = self.centre(v);
        } else {
            let centre = self.centre(v);
            let slot = &mut self.0[v];
            slot.pos = after_before;
            slot.offset = after_before - centre;
        }
    }

    /// Places `v`'s children side by side; their own subtrees are already
    /// arranged.
    fn arrange_children(&mut self, v: usize) {
        let mut fallback = NONE;
        let mut child = self.0[v].head;
        while child != NONE {
            self.seat(child);
            fallback = self.press(child, if fallback == NONE { child } else { fallback });
            child = self.0[child].after;
        }
        let (mut carried, mut rate) = (0.0f32, 0.0f32);
        let mut child = self.0[v].tail;
        while child != NONE {
            let slot = &mut self.0[child];
            slot.pos += carried;
            slot.offset += carried;
            rate += slot.drift;
            carried += slot.push + rate;
            child = slot.before;
        }
    }

    fn down_left(&self, v: usize) -> usize {
        let slot = &self.0[v];
        if slot.link != NONE {
            slot.link
        } else {
            slot.head
        }
    }

    fn down_right(&self, v: usize) -> usize {
        let slot = &self.0[v];
        if slot.link != NONE {
            slot.link
        } else {
            slot.tail
        }
    }

    fn walker(&self, at: usize) -> Walker {
        Walker {
            at,
            sum: self.0[at].offset,
        }
    }

    /// Pushes subtree `v` right until it clears its left siblings, and
    /// returns the sibling to blame for the next overlap.
    fn press(&mut self, v: usize, fallback: usize) -> usize {
        let before = self.0[v].before;
        if before == NONE {
            return fallback;
        }
        let up = self.0[v].up;
        let mut near_right = self.walker(v);
        let mut far_right = self.walker(v);
        let mut near_left = self.walker(before);
        let mut far_left = self.walker(self.0[up].head);
        loop {
            let steps = [
                self.down_right(near_left.at),
                self.down_left(near_right.at),
                self.down_left(far_left.at),
                self.down_right(far_right.at),
            ];
            if steps.contains(&NONE) {
                break;
            }
            [near_left.at, near_right.at, far_left.at, far_right.at] = steps;
            self.0[far_right.at].lead = v;
            let gap = (self.0[near_left.at].pos + near_left.sum)
                - (self.0[near_right.at].pos + near_right.sum)
                + 1.0;
            if gap > 0.0 {
                let blamed = self.0[near_left.at].lead;
                let blamed = if self.0[blamed].up == up {
                    blamed
                } else {
                    fallback
                };
                self.shove(blamed, v, gap);
                near_right.sum += gap;
                far_right.sum += gap;
            }
            near_left.sum += self.0[near_left.at].offset;
            near_right.sum += self.0[near_right.at].offset;
            far_left.sum += self.0[far_left.at].offset;
            far_right.sum += self.0[far_right.at].offset;
        }
        let left_more = self.down_right(near_left.at);
        if left_more != NONE && self.down_right(far_right.at) == NONE {
            let slot = &mut self.0[far_right.at];
            slot.link = left_more;
            slot.offset += near_left.sum - far_right.sum;
            return fallback;
        }
        let right_more = self.down_left(near_right.at);
        if right_more != NONE && self.down_left(far_left.at) == NONE {
            let slot = &mut self.0[far_left.at];
            slot.link = right_more;
            slot.offset += near_right.sum - far_left.sum;
        }
        v
    }

    fn shove(&mut self, from: usize, to: usize, gap: f32) {
        let span = self.0[to].rank as f32 - self.0[from].rank as f32;
        if span != 0.0 {
            self.0[to].drift -= gap / span;
            self.0[from].drift += gap / span;
        }
        let slot = &mut self.0[to];
        slot.push += gap;
        slot.pos += gap;
        slot.offset += gap;
    }

    /// Adds each node's ancestors' offsets to its position and lifts the
    /// drawing so nothing sits above zero. `drift` is reused to carry the
    /// summed offsets down the tree.
    fn settle(mut self) -> Vec<(Id, [f32; 2])> {
        let mut lowest = self.0[0].pos;
        for v in 0..self.0.len() {
            let up = self.0[v].up;
            let carry = if up == NONE { 0.0 } else { self.0[up].drift };
            let slot = &mut self.0[v];
            slot.pos += carry;
            lowest = lowest.min(slot.pos);
            slot.drift = carry + slot.offset;
        }
        let lift = if lowest < 0.0 { -lowest } else { 0.0 };
        self.0
            .iter()
            .map(|slot| {
                let y = if lift > 0.0 {
                    slot.pos + lift
                } else {
                    slot.pos
                };
                (slot.item, [slot.depth as f32, y])
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    #[ignore = "timing run: cargo test --release -p tuff-core --lib layout_timing -- --ignored --nocapture"]
    fn layout_timing() {
        let start = std::time::Instant::now();
        for _ in 0..50 {
            std::hint::black_box(random_forest_digest());
        }
        println!("layout_timing {:?}", start.elapsed());
    }

    fn random_forest_digest() -> u64 {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |bound: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % bound
        };
        let mut digest = 0xcbf2_9ce4_8422_2325u64;
        for _ in 0..400 {
            let count = 1 + next(80) as usize;
            let mut kids: Vec<Vec<Id>> = vec![Vec::new(); count];
            let mut shown = vec![true; count];
            for (node, visible) in shown.iter_mut().enumerate().skip(1) {
                let parent = if next(3) == 0 {
                    node - 1
                } else {
                    next(node as u64) as usize
                };
                kids[parent].push(node as Id);
                *visible = next(5) != 0;
            }
            for (id, [x, y]) in layout(0, &|id: Id| &kids[id as usize], &|id: Id| {
                shown[id as usize]
            }) {
                for word in [id as u64, x.to_bits() as u64, y.to_bits() as u64] {
                    digest = (digest ^ word).wrapping_mul(0x0100_0000_01b3);
                }
            }
        }
        digest
    }

    #[tuff_core::watchdog]
    #[test]
    fn random_forests_lay_out_like_the_reference_drawing() {
        assert_eq!(random_forest_digest(), 1_103_424_758_685_612_194);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_hidden_link_hangs_its_children_from_the_shown_ancestor() {
        let kids: [&[Id]; 5] = [&[1, 2], &[3, 4], &[], &[], &[]];
        let shown = [true, false, true, true, true];
        let placed = layout(0, &|id: Id| kids[id as usize], &|id: Id| shown[id as usize]);
        assert_eq!(
            placed,
            vec![
                (0, [0.0, 1.0]),
                (3, [1.0, 0.0]),
                (4, [1.0, 1.0]),
                (2, [1.0, 2.0])
            ]
        );
    }
}
