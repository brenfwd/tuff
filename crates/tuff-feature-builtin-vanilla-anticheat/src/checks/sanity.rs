use tuff_core::protocol::play::{Movement, Serverbound};

use super::{Check, Facts, Fix, Flag};

const NAME: &str = "sanity";
const MAX_HORIZONTAL: f64 = 3.0e7;
const MAX_VERTICAL: f64 = 2.0e7;
const MAX_YAW: f32 = 1.0e7;
const MAX_CLICK_CHANGES: usize = 128;
const MAX_SIGN_LINE: usize = 384;
const MAX_BOOK_PAGES: usize = 100;
const MAX_BOOK_PAGE: usize = 1024;
const MAX_BOOK_TITLE: usize = 32;
const MAX_ANVIL_NAME: usize = 50;
const IMMEDIATE: f64 = 1000.0;

/// Values no client produces: non-finite or out of range numbers, a pitch
/// past the poles, a cursor off the block face, oversized lists.
pub struct Sanity;

fn position_valid(position: &[f64; 3]) -> bool {
    position.iter().all(|v| v.is_finite())
        && position[0].abs() <= MAX_HORIZONTAL
        && position[2].abs() <= MAX_HORIZONTAL
        && position[1].abs() <= MAX_VERTICAL
}

fn rotation_finite(yaw: f32, pitch: f32) -> bool {
    yaw.is_finite() && pitch.is_finite() && yaw.abs() <= MAX_YAW
}

impl Check for Sanity {
    fn examine(&mut self, _facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        match packet {
            Serverbound::Move(movement) => {
                if movement
                    .position
                    .as_ref()
                    .is_some_and(|p| !position_valid(p))
                {
                    return Flag::cancel(NAME, IMMEDIATE);
                }
                if let Some((yaw, pitch)) = movement.rotation {
                    if !rotation_finite(yaw, pitch) {
                        return Flag::cancel(NAME, IMMEDIATE);
                    }
                    if pitch.abs() > 90.0 {
                        let fixed = Movement {
                            rotation: Some((yaw, pitch.clamp(-90.0, 90.0))),
                            ..*movement
                        };
                        return Some(Flag {
                            check: NAME,
                            weight: 5.0,
                            fix: Fix::Correct(Box::new(Serverbound::Move(fixed))),
                        });
                    }
                }
                None
            }
            Serverbound::MoveVehicle {
                position,
                yaw,
                pitch,
                ..
            } if !position_valid(position) || !rotation_finite(*yaw, *pitch) => {
                Flag::cancel(NAME, IMMEDIATE)
            }
            Serverbound::UseItemOn { cursor, .. }
                if cursor
                    .iter()
                    .any(|v| !v.is_finite() || *v < -1.0e-4 || *v > 1.0001) =>
            {
                Flag::cancel(NAME, 10.0)
            }
            Serverbound::Interact { location, .. }
                if location.iter().any(|v| !v.is_finite() || v.abs() > 16.0) =>
            {
                Flag::cancel(NAME, 10.0)
            }
            Serverbound::ChunkBatchReceived(rate) if !rate.is_finite() => Flag::cancel(NAME, 5.0),
            Serverbound::ContainerClick { changed, .. } if changed.len() > MAX_CLICK_CHANGES => {
                Flag::resync(NAME, 20.0)
            }
            Serverbound::EditBook { pages, title, .. }
                if pages.len() > MAX_BOOK_PAGES
                    || pages.iter().any(|p| p.chars().count() > MAX_BOOK_PAGE)
                    || title
                        .as_ref()
                        .is_some_and(|t| t.chars().count() > MAX_BOOK_TITLE) =>
            {
                Flag::cancel(NAME, 10.0)
            }
            Serverbound::RenameItem(name) if name.chars().count() > MAX_ANVIL_NAME => {
                Flag::cancel(NAME, 10.0)
            }
            Serverbound::SignUpdate { lines, .. }
                if lines.len() > 4
                    || lines
                        .iter()
                        .any(|l| l.encode_utf16().count() > MAX_SIGN_LINE) =>
            {
                Flag::cancel(NAME, 10.0)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::facts;
    use super::*;

    fn mv(position: Option<[f64; 3]>, rotation: Option<(f32, f32)>) -> Serverbound {
        Serverbound::Move(Movement {
            horizontal_collision: false,
            position,
            rotation,
            on_ground: true,
        })
    }

    #[tuff_core::watchdog]
    #[test]
    fn ordinary_moves_pass() {
        let mut check = Sanity;
        let facts = facts();
        assert_eq!(
            check.examine(&facts, &mv(Some([1.0, 64.0, 2.0]), Some((720.0, -90.0)))),
            None
        );
        assert_eq!(check.examine(&facts, &mv(None, None)), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn oversize_books_and_anvil_names_are_cancelled() {
        let mut check = Sanity;
        let facts = facts();
        let book = |pages: Vec<String>, title: Option<String>| Serverbound::EditBook {
            slot: 0,
            pages,
            title,
        };
        let ok = book(vec!["a".repeat(1024); 100], Some("t".repeat(32)));
        assert_eq!(check.examine(&facts, &ok), None);
        for bad in [
            book(vec!["a".into(); 101], None),
            book(vec!["a".repeat(1025)], None),
            book(vec![], Some("t".repeat(33))),
            Serverbound::RenameItem("n".repeat(51)),
        ] {
            assert_eq!(check.examine(&facts, &bad).unwrap().fix, Fix::Cancel);
        }
        assert_eq!(
            check.examine(&facts, &Serverbound::RenameItem("n".repeat(50))),
            None
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn non_finite_and_huge_values_are_dropped() {
        let mut check = Sanity;
        let facts = facts();
        for bad in [
            mv(Some([f64::NAN, 64.0, 0.0]), None),
            mv(Some([0.0, f64::INFINITY, 0.0]), None),
            mv(Some([3.1e7, 64.0, 0.0]), None),
            mv(None, Some((f32::NAN, 0.0))),
            mv(None, Some((1.0e9, 0.0))),
        ] {
            let flag = check.examine(&facts, &bad).unwrap();
            assert_eq!(flag.fix, Fix::Cancel);
            assert!(flag.weight >= 40.0);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_pitch_past_the_pole_is_clamped() {
        let flag = Sanity
            .examine(&facts(), &mv(None, Some((10.0, 120.0))))
            .unwrap();
        let Fix::Correct(fixed) = flag.fix else {
            panic!("expected a correction");
        };
        assert_eq!(*fixed, mv(None, Some((10.0, 90.0))));
    }

    #[tuff_core::watchdog]
    #[test]
    fn cursors_and_lists_are_bounded() {
        let mut check = Sanity;
        let facts = facts();
        let place = |cursor| Serverbound::UseItemOn {
            hand: 0,
            position: tuff_world_model::BlockPos::new(0, 0, 0),
            face: tuff_world_model::Face::Up,
            cursor,
            sequence: 1,
        };
        assert_eq!(check.examine(&facts, &place([0.5, 1.0, 0.0])), None);
        assert!(check.examine(&facts, &place([0.5, 7.0, 0.0])).is_some());
        assert!(
            check
                .examine(&facts, &place([f32::NAN, 0.0, 0.0]))
                .is_some()
        );
    }
}
