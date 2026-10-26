//! Duplication contraptions as data, built the same way on a real vanilla
//! server (`vanilla_dupers.rs`, opt-in) and on tuff (`block_dupers.rs`).
//! Offsets are from the piston base at `ORIGIN`.

pub type Part = (i32, i32, i32, &'static str);

pub const ORIGIN: (i32, i32, i32) = (84, 200, 86);

pub struct Design {
    pub name: &'static str,
    /// Placed in order, one block at a time with its updates.
    pub build: &'static [Part],
    /// Placed after the contraption settled; a part `@fall:<block>` drops a
    /// falling block entity there; a part `(0, 0, 0, "@step:N")`
    /// runs N ticks.
    pub trigger: &'static [Part],
    /// Ticks run after the trigger.
    pub steps: u32,
    /// Ids counted (blocks standing, item entities, falling blocks).
    /// Also count the kinds on the End's spawn platform (key `end:<id>`).
    pub end: bool,
    pub kinds: &'static [&'static str],
    /// Counts that vanilla 26.3 reaches after the run, by id; `primed_tnt`
    /// is the primed TNT entities. Verified by `vanilla_dupers.rs`.
    pub vanilla: &'static [(&'static str, u32)],
    /// When set, standing blocks of each kind are also counted per slab
    /// along the axis, under `<id>@<offset>` (offset from `ORIGIN`).
    pub slabs: Option<Slabs>,
    /// The counted box (offsets from `ORIGIN`).
    pub area: Area,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Slabs {
    X,
    Z,
}

pub type Area = ((i32, i32, i32), (i32, i32, i32));

pub const COUNT_BOX: Area = ((-3, -4, -6), (9, 6, 6));

const PUSH: &[Part] = &[
    (-3, -1, 0, "minecraft:redstone_block"),
    (0, 0, 0, "@step:8"),
];

const PUSH_PULL: &[Part] = &[
    (0, -1, 0, "minecraft:redstone_block"),
    (0, 0, 0, "@step:6"),
    (0, -1, 0, "minecraft:air"),
    (0, 0, 0, "@step:6"),
];

macro_rules! pot_layout {
    ($top:expr) => {
        &[
            (-3, 0, 0, "minecraft:sticky_piston[facing=east]"),
            (-2, 0, 0, "minecraft:stone"),
            (-1, 0, 0, "minecraft:slime_block"),
            (0, 0, 0, "minecraft:flower_pot"),
            (-1, 1, 0, "minecraft:slime_block"),
            (1, 0, 0, "minecraft:stone"),
            (-1, 0, 1, "minecraft:stone"),
            (0, 0, 1, "minecraft:stone"),
            (0, 1, 1, "minecraft:tube_coral_fan"),
            (0, 1, 0, $top),
        ]
    };
}

macro_rules! pot_design {
    ($name:expr, $top:expr, $kind:expr, $kept:expr) => {
        Design {
            name: $name,
            build: pot_layout!($top),
            trigger: PUSH,
            steps: 0,
            end: false,
            kinds: &[
                $kind,
                "minecraft:flower_pot",
                "minecraft:slime_block",
                "minecraft:stone",
            ],
            vanilla: &[
                ("minecraft:flower_pot", 1),
                ($kind, $kept),
                ("minecraft:slime_block", 2),
                ("minecraft:stone", 4),
            ],
            slabs: None,
            area: COUNT_BOX,
        }
    };
}

pub const DESIGNS: &[Design] = &[
    pot_design!(
        "pot_powered_rail",
        "minecraft:powered_rail",
        "minecraft:powered_rail",
        2
    ),
    pot_design!(
        "pot_detector_rail",
        "minecraft:detector_rail",
        "minecraft:detector_rail",
        2
    ),
    pot_design!(
        "pot_activator_rail",
        "minecraft:activator_rail",
        "minecraft:activator_rail",
        2
    ),
    pot_design!(
        "pot_repeater",
        "minecraft:repeater[facing=north]",
        "minecraft:repeater",
        1
    ),
    pot_design!(
        "pot_comparator",
        "minecraft:comparator[facing=north]",
        "minecraft:comparator",
        1
    ),
    pot_design!("pot_lantern", "minecraft:lantern", "minecraft:lantern", 1),
    Design {
        name: "fan_dust",
        build: &[
            (-3, 0, 0, "minecraft:sticky_piston[facing=east]"),
            (-2, 0, 0, "minecraft:stone"),
            (-1, 0, 0, "minecraft:stone"),
            (0, 0, 0, "minecraft:slime_block"),
            (-1, 1, 0, "minecraft:tube_coral_fan"),
            (0, 1, 0, "minecraft:redstone_wire"),
        ],
        trigger: PUSH,
        steps: 0,
        end: false,
        kinds: &[
            "minecraft:redstone_wire",
            "minecraft:redstone",
            "minecraft:slime_block",
            "minecraft:stone",
        ],
        vanilla: &[
            ("minecraft:redstone", 1),
            ("minecraft:slime_block", 1),
            ("minecraft:stone", 2),
        ],
        slabs: None,
        area: COUNT_BOX,
    },
    Design {
        name: "dust_on_slime",
        build: &[
            (0, 0, 0, "minecraft:sticky_piston[facing=east]"),
            (1, 0, 0, "minecraft:slime_block"),
            (1, 1, 0, "minecraft:redstone_wire"),
        ],
        trigger: PUSH_PULL,
        steps: 0,
        end: false,
        kinds: &[
            "minecraft:redstone_wire",
            "minecraft:redstone",
            "minecraft:slime_block",
        ],
        vanilla: &[("minecraft:redstone", 1), ("minecraft:slime_block", 1)],
        slabs: None,
        area: COUNT_BOX,
    },
    Design {
        name: "fan_rail",
        build: &[
            (-3, 0, 0, "minecraft:sticky_piston[facing=east]"),
            (-2, 0, 0, "minecraft:stone"),
            (-1, 0, 0, "minecraft:stone"),
            (0, 0, 0, "minecraft:slime_block"),
            (-1, 1, 0, "minecraft:tube_coral_fan"),
            (0, 1, 0, "minecraft:rail"),
        ],
        trigger: PUSH,
        steps: 0,
        end: false,
        kinds: &["minecraft:rail", "minecraft:slime_block", "minecraft:stone"],
        vanilla: &[
            ("minecraft:rail", 2),
            ("minecraft:slime_block", 1),
            ("minecraft:stone", 2),
        ],
        slabs: None,
        area: COUNT_BOX,
    },
    Design {
        name: "fan_tnt",
        build: &[
            (-3, 0, 0, "minecraft:sticky_piston[facing=east]"),
            (-2, 0, 0, "minecraft:stone"),
            (-1, 0, 0, "minecraft:stone"),
            (0, 0, 0, "minecraft:slime_block"),
            (-1, 1, 0, "minecraft:tube_coral_fan"),
            (0, 1, 0, "minecraft:tnt"),
            (0, 2, 0, "minecraft:redstone_block"),
        ],
        trigger: PUSH,
        steps: 0,
        end: false,
        kinds: &["minecraft:tnt", "minecraft:slime_block", "minecraft:stone"],
        vanilla: &[
            ("minecraft:slime_block", 1),
            ("minecraft:stone", 2),
            ("minecraft:tnt", 1),
            ("primed_tnt", 1),
        ],
        slabs: None,
        area: COUNT_BOX,
    },
    Design {
        name: "fan_carpet",
        build: &[
            (-3, 0, 0, "minecraft:sticky_piston[facing=east]"),
            (-2, 0, 0, "minecraft:stone"),
            (-1, 0, 0, "minecraft:stone"),
            (0, 0, 0, "minecraft:slime_block"),
            (-1, 1, 0, "minecraft:tube_coral_fan"),
            (0, 1, 0, "minecraft:white_carpet"),
        ],
        trigger: PUSH,
        steps: 0,
        end: false,
        kinds: &[
            "minecraft:white_carpet",
            "minecraft:slime_block",
            "minecraft:stone",
        ],
        vanilla: &[
            ("minecraft:slime_block", 1),
            ("minecraft:stone", 2),
            ("minecraft:white_carpet", 1),
        ],
        slabs: None,
        area: COUNT_BOX,
    },
    Design {
        name: "carpet_pot",
        build: &[
            (-3, 0, 0, "minecraft:sticky_piston[facing=east]"),
            (-2, 0, 0, "minecraft:stone"),
            (-1, 0, 0, "minecraft:slime_block"),
            (0, 0, 0, "minecraft:flower_pot"),
            (-1, 1, 0, "minecraft:slime_block"),
            (1, 0, 0, "minecraft:stone"),
            (-1, 0, 1, "minecraft:stone"),
            (0, 0, 1, "minecraft:stone"),
            (0, 1, 1, "minecraft:tube_coral_fan"),
            (0, 1, 0, "minecraft:white_carpet"),
        ],
        trigger: PUSH,
        steps: 0,
        end: false,
        kinds: &[
            "minecraft:white_carpet",
            "minecraft:flower_pot",
            "minecraft:slime_block",
            "minecraft:stone",
        ],
        vanilla: &[
            ("minecraft:flower_pot", 1),
            ("minecraft:slime_block", 2),
            ("minecraft:stone", 4),
            ("minecraft:white_carpet", 2),
        ],
        slabs: None,
        area: COUNT_BOX,
    },
    Design {
        name: "rail_pot",
        build: &[
            (-3, 0, 0, "minecraft:sticky_piston[facing=east]"),
            (-2, 0, 0, "minecraft:stone"),
            (-1, 0, 0, "minecraft:slime_block"),
            (0, 0, 0, "minecraft:flower_pot"),
            (-1, 1, 0, "minecraft:slime_block"),
            (1, 0, 0, "minecraft:stone"),
            (-1, 0, 1, "minecraft:stone"),
            (0, 0, 1, "minecraft:stone"),
            (0, 1, 1, "minecraft:tube_coral_fan"),
            (0, 1, 0, "minecraft:rail"),
        ],
        trigger: PUSH,
        steps: 0,
        end: false,
        kinds: &[
            "minecraft:rail",
            "minecraft:flower_pot",
            "minecraft:slime_block",
            "minecraft:stone",
        ],
        vanilla: &[
            ("minecraft:flower_pot", 1),
            ("minecraft:rail", 2),
            ("minecraft:slime_block", 2),
            ("minecraft:stone", 4),
        ],
        slabs: None,
        area: COUNT_BOX,
    },
    Design {
        name: "gravity_inside_portal",
        build: &[
            (0, -1, 0, "minecraft:stone"),
            (0, 0, 0, "minecraft:end_portal"),
        ],
        trigger: &[(0, 0, 0, "@fall:minecraft:sand"), (0, 0, 0, "@step:40")],
        steps: 0,
        end: true,
        kinds: &["minecraft:sand"],
        vanilla: &[("minecraft:sand", 1), ("end:minecraft:sand", 1)],
        slabs: None,
        area: COUNT_BOX,
    },
    Design {
        name: "gravity_fall_into_portal",
        build: &[
            (0, -1, 0, "minecraft:stone"),
            (0, 0, 0, "minecraft:end_portal"),
        ],
        trigger: &[(0, 3, 0, "@fall:minecraft:sand"), (0, 0, 0, "@step:40")],
        steps: 0,
        end: true,
        kinds: &["minecraft:sand"],
        vanilla: &[("end:minecraft:sand", 1)],
        slabs: None,
        area: COUNT_BOX,
    },
    Design {
        name: "fan_tripwire_hook",
        build: &[
            (-3, 0, 0, "minecraft:sticky_piston[facing=east]"),
            (-2, 0, 0, "minecraft:stone"),
            (-1, 0, 0, "minecraft:stone"),
            (0, 0, 0, "minecraft:slime_block"),
            (-1, 1, 0, "minecraft:tube_coral_fan"),
            (0, 1, -1, "minecraft:stone"),
            (0, 1, 0, "minecraft:tripwire_hook[facing=south]"),
        ],
        trigger: PUSH,
        steps: 0,
        end: false,
        kinds: &[
            "minecraft:tripwire_hook",
            "minecraft:slime_block",
            "minecraft:stone",
        ],
        vanilla: &[
            ("minecraft:tripwire_hook", 1),
            ("minecraft:slime_block", 1),
            ("minecraft:stone", 3),
        ],
        slabs: None,
        area: COUNT_BOX,
    },
    Design {
        name: "rail_on_slime",
        build: &[
            (0, 0, 0, "minecraft:sticky_piston[facing=east]"),
            (1, 0, 0, "minecraft:slime_block"),
            (1, 1, 0, "minecraft:rail"),
        ],
        trigger: PUSH_PULL,
        steps: 0,
        end: false,
        kinds: &["minecraft:rail", "minecraft:slime_block"],
        vanilla: &[("minecraft:rail", 1), ("minecraft:slime_block", 1)],
        slabs: None,
        area: COUNT_BOX,
    },
    Design {
        name: "carpet_on_slime",
        build: &[
            (0, 0, 0, "minecraft:sticky_piston[facing=east]"),
            (1, 0, 0, "minecraft:slime_block"),
            (1, 1, 0, "minecraft:white_carpet"),
        ],
        trigger: PUSH_PULL,
        steps: 0,
        end: false,
        kinds: &["minecraft:white_carpet", "minecraft:slime_block"],
        vanilla: &[("minecraft:slime_block", 1), ("minecraft:white_carpet", 1)],
        slabs: None,
        area: COUNT_BOX,
    },
];
