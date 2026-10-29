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
    fn class_at(&mut self, x: i32, y: i32, z: i32) -> Result<Class, Pending> {
        Ok(match self.0.get(&(x, y, z)).map(String::as_str) {
            Some("minecraft:stone") => Class::STANDABLE,
            _ => Class::NONE,
        })
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
    run_from(farmer, field, carried, (1.5, 1.5)).0
}

fn run_from(
    farmer: &mut Farmer,
    field: &mut Field,
    carried: &[Carried],
    start: (f64, f64),
) -> (u32, Vec<Pos>) {
    let mut goals = Vec::new();
    let mut me = Vec3 {
        x: start.0,
        y: 1.0,
        z: start.1,
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
            Farming::Done => return (tick, goals),
            Farming::Failed(r) => panic!("failed: {r}"),
            Farming::Goal(Goal::Leaf(LeafGoal::Near(n))) => {
                goals.push((n.pos.x, n.pos.y, n.pos.z));
                me = Vec3 {
                    x: f64::from(n.pos.x) + 0.5,
                    y: f64::from(n.pos.y),
                    z: f64::from(n.pos.z) + 0.5,
                };
            }
            Farming::Goal(_) => {}
            Farming::Dig(p) => digging = Some((p, tick + 3)),
            Farming::Act(actions) => {
                for a in actions {
                    if let Action::Place { against, face: _ } = a {
                        let crop = field.0.get(&against).cloned().unwrap_or_default();
                        match age(&crop) {
                            Some(a) if name_of(&crop) == "wheat" => {
                                field.set(against, &format!("wheat[age={}]", (a + 3).min(7)));
                            }
                            _ => field.set((against.0, against.1 + 1, against.2), "wheat[age=0]"),
                        }
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

fn bordered(age: u32) -> Field {
    let mut f = wheat_field(age);
    for x in -3..=5 {
        for z in -3..=5 {
            if !(0..3).contains(&x) || !(0..3).contains(&z) {
                f.set((x, 0, z), "stone");
            }
        }
    }
    f
}

fn bone_meal() -> Vec<Carried> {
    vec![
        (36, "minecraft:wheat_seeds".into(), 9),
        (37, "minecraft:bone_meal".into(), 9),
    ]
}

#[test]
fn never_walks_onto_the_farmland_when_a_border_exists() {
    let mut field = bordered(7);
    let mut farmer = Farmer::new(&spec(true));
    let (_, goals) = run_from(&mut farmer, &mut field, &seeds(), (-1.5, 1.5));
    assert_eq!(farmer.harvested(), 9);
    assert!(!goals.is_empty());
    for g in goals {
        let below = field.0.get(&(g.0, g.1 - 1, g.2)).map(String::as_str);
        assert_ne!(below, Some("minecraft:farmland[moisture=7]"), "goal {g:?}");
    }
}

#[test]
fn bone_meal_ripens_young_crops_and_they_are_then_harvested() {
    let mut field = bordered(2);
    let mut farmer = Farmer::new(&spec(false));
    run_from(&mut farmer, &mut field, &bone_meal(), (-1.5, 1.5));
    assert_eq!(farmer.harvested(), 9);
    for x in 0..3 {
        for z in 0..3 {
            assert_eq!(field.0[&(x, 1, z)], "minecraft:air");
        }
    }
}

#[test]
fn bone_meal_is_never_used_on_mature_crops() {
    let mut field = bordered(7);
    let mut farmer = Farmer::new(&spec(false));
    let mut me_acts = 0;
    let me = Vec3 {
        x: -1.5,
        y: 1.0,
        z: 1.5,
    };
    for _ in 0..3 {
        if let Farming::Act(a) = farmer.tick(&me, &mut field, &bone_meal())
            && a.iter().any(|a| matches!(a, Action::Place { .. }))
        {
            me_acts += 1;
        }
    }
    assert_eq!(me_acts, 0);
}

#[test]
fn young_crops_are_left_alone_without_bone_meal() {
    let mut field = bordered(2);
    let mut farmer = Farmer::new(&spec(true));
    let me = Vec3 {
        x: -1.5,
        y: 1.0,
        z: 1.5,
    };
    assert_eq!(farmer.tick(&me, &mut field, &seeds()), Farming::Wait);
    assert_eq!(farmer.tick(&me, &mut field, &seeds()), Farming::Done);
}

#[test]
fn bone_meal_is_applied_as_a_use_on_the_crop() {
    let mut field = bordered(2);
    let mut farmer = Farmer::new(&spec(true));
    let me = Vec3 {
        x: -0.5,
        y: 1.0,
        z: 0.5,
    };
    let Farming::Act(acts) = farmer.tick(&me, &mut field, &bone_meal()) else {
        panic!("expected an action");
    };
    assert_eq!(acts[0], Action::Select(1));
    assert!(matches!(acts[1], Action::Place { against: (0, 1, 0), .. }));
}

#[test]
fn bone_meal_is_not_spent_on_the_seedlings_it_just_planted() {
    let mut field = bordered(7);
    let mut farmer = Farmer::new(&spec(true));
    run_from(&mut farmer, &mut field, &bone_meal(), (-1.5, 1.5));
    for x in 0..3 {
        for z in 0..3 {
            assert_eq!(field.0[&(x, 1, z)], "minecraft:wheat[age=0]");
        }
    }
}
