use std::collections::HashMap;
use std::rc::Rc;

use super::*;
use crate::world::{Class, Pending};

#[derive(Default)]
struct Field(HashMap<Pos, String>);

impl Field {
    fn set(&mut self, p: Pos, s: &str) {
        self.0.insert(p, format!("minecraft:{s}"));
    }
}

impl Blocks for Field {
    fn class_at(&mut self, _: i32, _: i32, _: i32) -> Result<Class, Pending> {
        Ok(Class::NONE)
    }

    fn state_at(&mut self, x: i32, y: i32, z: i32) -> Result<Option<Rc<str>>, Pending> {
        Ok(self.0.get(&(x, y, z)).map(|s| Rc::from(s.as_str())))
    }
}

fn spec(replant: bool) -> FarmSpec {
    FarmSpec {
        min: BlockPos { x: 0, y: 1, z: 0 },
        max: BlockPos { x: 2, y: 1, z: 2 },
        replant,
    }
}

fn wheat_field(age: u32) -> Field {
    let mut f = Field::default();
    for x in 0..3 {
        for z in 0..3 {
            f.set((x, 0, z), "farmland[moisture=7]");
            f.set((x, 1, z), &format!("wheat[age={age}]"));
        }
    }
    f
}

fn seeds() -> Vec<Carried> {
    vec![(36, "minecraft:wheat_seeds".into(), 9)]
}

/// Drives the farmer, applying digs and plantings to the field the way
/// the server would and teleporting the bot to whatever it walks to.
fn run(farmer: &mut Farmer, field: &mut Field, carried: &[Carried]) -> u32 {
    let mut me = Vec3 {
        x: 1.5,
        y: 1.0,
        z: 1.5,
    };
    let mut digging: Option<(Pos, u32)> = None;
    for tick in 0..2000 {
        if let Some((p, t)) = digging
            && tick >= t
        {
            field.0.insert(p, "minecraft:air".into());
            digging = None;
        }
        match farmer.tick(&me, field, carried) {
            Farming::Done => return tick,
            Farming::Failed(r) => panic!("failed: {r}"),
            Farming::Goal(Goal::Leaf(LeafGoal::Near(n))) => {
                me = Vec3 {
                    x: f64::from(n.pos.x) + 0.5,
                    y: 1.0,
                    z: f64::from(n.pos.z) + 0.5,
                };
            }
            Farming::Goal(_) => {}
            Farming::Dig(p) => digging = Some((p, tick + 3)),
            Farming::Act(actions) => {
                for a in actions {
                    if let Action::Place { against, face: _ } = a {
                        field.set((against.0, against.1 + 1, against.2), "wheat[age=0]");
                    }
                }
            }
            Farming::Wait => {}
        }
    }
    panic!("never finished");
}

#[test]
fn only_mature_crops_count() {
    assert!(is_mature("minecraft:wheat[age=7]", None));
    assert!(!is_mature("minecraft:wheat[age=6]", None));
    assert!(is_mature("minecraft:beetroots[age=3]", None));
    assert!(!is_mature("minecraft:beetroots[age=2]", None));
    assert!(is_mature("minecraft:nether_wart[age=3]", None));
    assert!(is_mature("minecraft:melon", None));
    assert!(!is_mature("minecraft:melon_stem[age=7]", None));
    assert!(is_mature(
        "minecraft:sugar_cane[age=0]",
        Some("minecraft:sugar_cane[age=0]")
    ));
    assert!(!is_mature(
        "minecraft:sugar_cane[age=0]",
        Some("minecraft:sand")
    ));
    assert!(!is_mature("minecraft:stone", None));
}

#[test]
fn harvests_and_replants_a_three_by_three_wheat_field() {
    let mut field = wheat_field(7);
    let mut farmer = Farmer::new(&spec(true));
    run(&mut farmer, &mut field, &seeds());
    assert_eq!(farmer.harvested(), 9);
    for x in 0..3 {
        for z in 0..3 {
            assert_eq!(field.0[&(x, 1, z)], "minecraft:wheat[age=0]");
        }
    }
}

#[test]
fn leaves_immature_crops_and_skips_replanting_without_seeds() {
    let mut field = wheat_field(5);
    field.set((1, 1, 1), "wheat[age=7]");
    let mut farmer = Farmer::new(&spec(true));
    run(&mut farmer, &mut field, &[]);
    assert_eq!(farmer.harvested(), 1);
    assert_eq!(field.0[&(1, 1, 1)], "minecraft:air");
    assert_eq!(field.0[&(0, 1, 0)], "minecraft:wheat[age=5]");
}

#[test]
fn does_not_replant_when_told_not_to_and_cuts_cane_above_its_base() {
    let mut field = wheat_field(7);
    field.set((0, 0, 0), "sand");
    field.set((0, 1, 0), "sugar_cane[age=0]");
    field.set((0, 2, 0), "sugar_cane[age=0]");
    let mut s = spec(false);
    s.max.y = 2;
    let mut farmer = Farmer::new(&s);
    run(&mut farmer, &mut field, &seeds());
    assert_eq!(farmer.harvested(), 9);
    assert_eq!(field.0[&(1, 1, 1)], "minecraft:air");
    assert_eq!(field.0[&(0, 1, 0)], "minecraft:sugar_cane[age=0]");
    assert_eq!(field.0[&(0, 2, 0)], "minecraft:air");
}
