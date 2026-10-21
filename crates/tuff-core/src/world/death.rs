use tuff_item::text::TextComponent;
use tuff_vanilla_data::damage_type::{DamageType, DeathMessageType};

/// The death message of `victim`, killed by `killer` (if any) with `kind`.
/// `held` is the killer's custom-named weapon; `credit` is the recent
/// player attacker used when nothing is the direct cause.
pub fn death_message(
    kind: &DamageType,
    victim: TextComponent,
    killer: Option<TextComponent>,
    held: Option<TextComponent>,
    credit: Option<TextComponent>,
) -> TextComponent {
    let kind = crate::world::effective_damage_type(kind);
    let key = format!("death.attack.{}", kind.message_id);
    match (kind.death_message_type, killer) {
        (Some(DeathMessageType::IntentionalGameDesign), _) => {
            let link = TextComponent::new("[")
                .append(TextComponent::translate(format!("{key}.link"), []))
                .append(TextComponent::new("]"));
            TextComponent::translate(format!("{key}.message"), [victim, link])
        }
        (_, Some(killer)) => match held {
            Some(held) => TextComponent::translate(format!("{key}.item"), [victim, killer, held]),
            None => TextComponent::translate(key, [victim, killer]),
        },
        (_, None) => match credit {
            Some(credit) => TextComponent::translate(format!("{key}.player"), [victim, credit]),
            None => TextComponent::translate(key, [victim]),
        },
    }
}

/// Where a fall began, for the death message of someone who died of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallLocation {
    Generic,
    Ladder,
    Vines,
    WeepingVines,
    TwistingVines,
    Scaffolding,
    OtherClimbable,
    Water,
}

impl FallLocation {
    pub fn id(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::Ladder => "ladder",
            Self::Vines => "vines",
            Self::WeepingVines => "weeping_vines",
            Self::TwistingVines => "twisting_vines",
            Self::Scaffolding => "scaffolding",
            Self::OtherClimbable => "other_climbable",
            Self::Water => "water",
        }
    }

    pub fn language_key(self) -> String {
        format!("death.fell.accident.{}", self.id())
    }

    /// The location a climbable block stands for.
    pub fn of_block(block: tuff_vanilla_data::Block) -> Self {
        use tuff_vanilla_data::Block;
        match block {
            Block::Ladder => Self::Ladder,
            Block::Vine => Self::Vines,
            Block::WeepingVines | Block::WeepingVinesPlant => Self::WeepingVines,
            Block::TwistingVines | Block::TwistingVinesPlant => Self::TwistingVines,
            Block::Scaffolding => Self::Scaffolding,
            block
                if tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::block::key::TRAPDOORS,
                    block,
                ) =>
            {
                Self::Ladder
            }
            _ => Self::OtherClimbable,
        }
    }
}

/// Who dealt a hit, as a death message names them.
#[derive(Clone, Debug, PartialEq)]
pub struct Attacker {
    pub uuid: u128,
    pub name: TextComponent,
    /// The custom-named item in their hand.
    pub held: Option<TextComponent>,
}

/// One damage event of the combat log.
#[derive(Clone, Debug)]
pub struct CombatEntry {
    pub kind: &'static DamageType,
    pub damage: f32,
    pub location: Option<FallLocation>,
    pub fall_distance: f32,
    pub attacker: Option<Attacker>,
}

fn is_fall(kind: &DamageType) -> bool {
    crate::world::damage_type_in_tag(kind, "minecraft:is_fall")
}

fn always_significant(kind: &DamageType) -> bool {
    crate::world::damage_type_in_tag(kind, "minecraft:always_most_significant_fall")
}

/// The entry that knocked the victim off, if the fall was long or hard
/// enough to be blamed.
fn most_significant_fall(entries: &[CombatEntry]) -> Option<&CombatEntry> {
    let mut result = None;
    let mut alternative: Option<&CombatEntry> = None;
    let mut alt_damage = 0.0f32;
    let mut best_fall = 0.0f32;
    for (i, entry) in entries.iter().enumerate() {
        let fake = always_significant(entry.kind);
        let distance = if fake { f32::MAX } else { entry.fall_distance };
        if (is_fall(entry.kind) || fake)
            && distance > 0.0
            && (result.is_none() || distance > best_fall)
        {
            result = Some(if i > 0 { &entries[i - 1] } else { entry });
            best_fall = distance;
        }
        if entry.location.is_some() && (alternative.is_none() || entry.damage > alt_damage) {
            alternative = Some(entry);
            alt_damage = entry.damage;
        }
    }
    if best_fall > 5.0 && result.is_some() {
        return result;
    }
    if alt_damage > 5.0 && alternative.is_some() {
        return alternative;
    }
    None
}

fn assisted(
    victim: TextComponent,
    who: &Attacker,
    with_item: &str,
    without_item: &str,
) -> TextComponent {
    match &who.held {
        Some(held) => TextComponent::translate(with_item, [victim, who.name.clone(), held.clone()]),
        None => TextComponent::translate(without_item, [victim, who.name.clone()]),
    }
}

/// The `death.fell.*` message for a death whose killing blow (the last
/// entry) is of the fall-variants kind, when the log blames a fall. `killer`
/// is who dealt the killing blow.
pub fn fall_death_message(
    entries: &[CombatEntry],
    victim: TextComponent,
    killer: Option<&Attacker>,
) -> Option<TextComponent> {
    let knock_off = most_significant_fall(entries)?;
    if is_fall(knock_off.kind) || always_significant(knock_off.kind) {
        let location = knock_off.location.unwrap_or(FallLocation::Generic);
        return Some(TextComponent::translate(location.language_key(), [victim]));
    }
    let attacker = knock_off.attacker.as_ref();
    if let Some(attacker) = attacker
        && killer.is_none_or(|killer| killer.uuid != attacker.uuid)
    {
        return Some(assisted(
            victim,
            attacker,
            "death.fell.assist.item",
            "death.fell.assist",
        ));
    }
    Some(match killer {
        Some(killer) => assisted(
            victim,
            killer,
            "death.fell.finish.item",
            "death.fell.finish",
        ),
        None => TextComponent::translate("death.fell.killer", [victim]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn builds_plain_and_killed_messages() {
        let kind = &tuff_vanilla_data::damage_type::GENERIC;
        let plain = death_message(kind, TextComponent::new("Rex"), None, None, None);
        assert_eq!(plain.plain(), "Rex died");
        let killed = death_message(
            kind,
            TextComponent::new("Rex"),
            Some(TextComponent::new("Zed")),
            None,
            None,
        );
        assert_eq!(killed.plain(), "Rex died");
    }

    fn entry(
        kind: &'static DamageType,
        distance: f32,
        location: Option<FallLocation>,
    ) -> CombatEntry {
        CombatEntry {
            kind,
            damage: 4.0,
            location,
            fall_distance: distance,
            attacker: None,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_long_fall_from_a_ladder_names_the_ladder() {
        let entries = [entry(
            &tuff_vanilla_data::damage_type::FALL,
            8.0,
            Some(FallLocation::Ladder),
        )];
        let message = fall_death_message(&entries, TextComponent::new("Rex"), None).unwrap();
        assert!(format!("{message:?}").contains("death.fell.accident.ladder"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_short_fall_is_not_blamed() {
        let entries = [entry(&tuff_vanilla_data::damage_type::FALL, 3.0, None)];
        assert!(fall_death_message(&entries, TextComponent::new("Rex"), None).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_push_before_the_fall_is_an_assist_or_a_finish() {
        let hit = CombatEntry {
            attacker: Some(Attacker {
                uuid: 1,
                name: TextComponent::new("Zed"),
                held: None,
            }),
            ..entry(&tuff_vanilla_data::damage_type::PLAYER_ATTACK, 0.0, None)
        };
        let fall = entry(&tuff_vanilla_data::damage_type::FALL, 9.0, None);
        let entries = [hit, fall];
        let none = fall_death_message(&entries, TextComponent::new("Rex"), None).unwrap();
        assert!(format!("{none:?}").contains("death.fell.assist"));
        let other = Attacker {
            uuid: 2,
            name: TextComponent::new("Yan"),
            held: None,
        };
        let finish = fall_death_message(&entries, TextComponent::new("Rex"), Some(&other)).unwrap();
        assert!(format!("{finish:?}").contains("death.fell.assist"));
        let same = Attacker { uuid: 1, ..other };
        let finish = fall_death_message(&entries, TextComponent::new("Rex"), Some(&same)).unwrap();
        assert!(format!("{finish:?}").contains("death.fell.finish"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn named_weapon_and_kill_credit_pick_the_variant_keys() {
        let kind = &tuff_vanilla_data::damage_type::FALL;
        let key = |message: TextComponent| format!("{message:?}");
        let item = death_message(
            kind,
            TextComponent::new("Rex"),
            Some(TextComponent::new("Zed")),
            Some(TextComponent::new("[Bane]")),
            None,
        );
        assert!(key(item).contains("death.attack.fall.item"));
        let credit = death_message(
            kind,
            TextComponent::new("Rex"),
            None,
            None,
            Some(TextComponent::new("Zed")),
        );
        assert!(key(credit).contains("death.attack.fall.player"));
    }
}
