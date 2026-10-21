//! The teams as gameplay reads them: who is allied, whether allies may
//! hurt or push each other, and how a member's name is decorated. The
//! scoreboard actor publishes a fresh snapshot whenever a team or its
//! members change, so ticking code reads it without waiting.

use std::collections::HashMap;

use tuff_item::text::{ClickEvent, TextComponent};

use super::{CollisionRule, Team, Visibility};

/// A score holder as teams know it: a player by name, any other entity
/// by its UUID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Member<'a> {
    Player(&'a str),
    Entity(u128),
}

#[derive(Debug, Default)]
pub struct Teams {
    teams: Vec<Team>,
    by_name: HashMap<String, usize>,
    by_uuid: HashMap<u128, usize>,
}

impl Teams {
    pub(super) fn new<'a>(teams: impl Iterator<Item = &'a Team>) -> Self {
        let mut out = Self::default();
        for team in teams {
            let index = out.teams.len();
            for holder in &team.members {
                out.by_name.insert(holder.clone(), index);
                if let Some(uuid) = tuff_storage::admin::parse_uuid_text(holder) {
                    out.by_uuid.insert(uuid, index);
                }
            }
            out.teams.push(team.clone());
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    fn index(&self, member: Member) -> Option<usize> {
        match member {
            Member::Player(name) => self.by_name.get(name).copied(),
            Member::Entity(uuid) => self.by_uuid.get(&uuid).copied(),
        }
    }

    pub fn team_of(&self, member: Member) -> Option<&Team> {
        self.index(member).map(|index| &self.teams[index])
    }

    /// Both are on the same team.
    pub fn allied(&self, a: Member, b: Member) -> bool {
        self.index(a)
            .is_some_and(|team| self.index(b) == Some(team))
    }

    /// Whether `attacker` may hurt `victim`: always, unless they are allies
    /// on a team that forbids friendly fire.
    pub fn allows_damage(&self, attacker: Member, victim: Member) -> bool {
        match self.index(attacker) {
            Some(team) if self.index(victim) == Some(team) => self.teams[team].friendly_fire,
            _ => true,
        }
    }

    /// Whether `own` pushes against `their`, by both teams' collision
    /// rules.
    pub fn pushes(&self, own: Member, their: Member) -> bool {
        if self.is_empty() {
            return true;
        }
        let own_team = self.index(own);
        let their_team = self.index(their);
        let rule = |team: Option<usize>| {
            team.map_or(CollisionRule::Always, |t| self.teams[t].collision_rule)
        };
        let (own_rule, their_rule) = (rule(own_team), rule(their_team));
        if own_rule == CollisionRule::Never || their_rule == CollisionRule::Never {
            return false;
        }
        let same = own_team.is_some() && own_team == their_team;
        let either = |wanted| own_rule == wanted || their_rule == wanted;
        !((same && either(CollisionRule::PushOwnTeam))
            || (!same && either(CollisionRule::PushOtherTeams)))
    }

    /// `name` with the member's team prefix, suffix and color around it.
    pub fn format_name(&self, member: Member, name: TextComponent) -> TextComponent {
        let Some(team) = self.team_of(member) else {
            return name;
        };
        let out = TextComponent::new("")
            .append(team.prefix.clone())
            .append(name)
            .append(team.suffix.clone());
        match team.color {
            Some(color) => out.color(color.name()),
            None => out,
        }
    }

    /// A player's display name: its team decoration, suggesting a private
    /// message when clicked, showing the player on hover and inserting the
    /// name when shift-clicked.
    pub fn player_name(&self, name: &str, uuid: u128) -> TextComponent {
        self.format_name(Member::Player(name), TextComponent::new(name))
            .click(ClickEvent::SuggestCommand(format!("/tell {name} ")))
            .hover_entity("minecraft:player", uuid, Some(TextComponent::new(name)))
            .insertion(name)
    }

    /// Any other entity's display name: `name` decorated by its team,
    /// showing the entity of type `kind` on hover and inserting its UUID
    /// when shift-clicked.
    pub fn entity_name(&self, kind: &str, uuid: u128, name: TextComponent) -> TextComponent {
        self.owned_entity_name(kind, uuid, name, None)
    }

    /// As [`Self::entity_name`], for an entity that takes the team of the
    /// online player `owner`.
    pub fn owned_entity_name(
        &self,
        kind: &str,
        uuid: u128,
        name: TextComponent,
        owner: Option<&str>,
    ) -> TextComponent {
        let member = owner.map_or(Member::Entity(uuid), Member::Player);
        self.format_name(member, name.clone())
            .hover_entity(kind, uuid, Some(name))
            .insertion(tuff_storage::admin::format_uuid(uuid))
    }

    /// Who hears a member's death message: everyone (`None`), or only the
    /// online players `hears` accepts.
    pub fn death_audience<'a>(&'a self, victim: &'a str) -> DeathAudience<'a> {
        let Some(index) = self.index(Member::Player(victim)) else {
            return DeathAudience::Everyone;
        };
        match self.teams[index].death_message_visibility {
            Visibility::Always => DeathAudience::Everyone,
            Visibility::Never => DeathAudience::Nobody,
            Visibility::HideForOtherTeams => DeathAudience::Team(self, index, victim),
            Visibility::HideForOwnTeam => DeathAudience::OthersThan(self, index),
        }
    }
}

/// Who a player's death message is broadcast to.
#[derive(Debug, Clone, Copy)]
pub enum DeathAudience<'a> {
    Everyone,
    Nobody,
    /// The victim's teammates, not the victim.
    Team(&'a Teams, usize, &'a str),
    /// Everyone not on the victim's team.
    OthersThan(&'a Teams, usize),
}

impl DeathAudience<'_> {
    pub fn hears(&self, player: &str) -> bool {
        match *self {
            Self::Everyone => true,
            Self::Nobody => false,
            Self::Team(teams, team, victim) => {
                player != victim && teams.index(Member::Player(player)) == Some(team)
            }
            Self::OthersThan(teams, team) => teams.index(Member::Player(player)) != Some(team),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn teams(setup: impl FnOnce(&mut Vec<Team>)) -> Teams {
        let mut list = vec![Team::new("red"), Team::new("blue")];
        list[0].members.insert("Alex".to_owned());
        list[0].members.insert("Steve".to_owned());
        list[1].members.insert("Bob".to_owned());
        list[1]
            .members
            .insert("00000000-0000-0001-0000-000000000002".to_owned());
        setup(&mut list);
        Teams::new(list.iter())
    }

    #[crate::watchdog]
    #[tuff_core::watchdog]
    #[test]
    fn friendly_fire_only_spares_allies() {
        let open = teams(|_| {});
        let alex = Member::Player("Alex");
        assert!(open.allows_damage(alex, Member::Player("Steve")));
        let closed = teams(|t| t[0].friendly_fire = false);
        assert!(!closed.allows_damage(alex, Member::Player("Steve")));
        assert!(!closed.allows_damage(alex, alex));
        assert!(closed.allows_damage(alex, Member::Player("Bob")));
        assert!(closed.allows_damage(Member::Player("Nobody"), alex));
        assert!(closed.allows_damage(alex, Member::Player("Nobody")));
    }

    #[crate::watchdog]
    #[tuff_core::watchdog]
    #[test]
    fn entities_belong_by_uuid() {
        let t = teams(|_| {});
        let mob = Member::Entity((1 << 64) | 2);
        assert_eq!(t.team_of(mob).map(|t| t.name.as_str()), Some("blue"));
        assert!(t.allied(mob, Member::Player("Bob")));
        assert!(!t.allied(mob, Member::Player("Alex")));
        assert!(!t.allied(Member::Entity(7), Member::Entity(8)));
    }

    #[crate::watchdog]
    #[tuff_core::watchdog]
    #[test]
    fn collision_rules_follow_both_teams() {
        let (alex, steve, bob, loner) = (
            Member::Player("Alex"),
            Member::Player("Steve"),
            Member::Player("Bob"),
            Member::Player("Loner"),
        );
        let t = teams(|_| {});
        assert!(t.pushes(alex, steve) && t.pushes(alex, bob));
        let t = teams(|t| t[0].collision_rule = CollisionRule::Never);
        assert!(!t.pushes(alex, bob) && !t.pushes(bob, alex) && !t.pushes(loner, alex));
        assert!(t.pushes(bob, loner));
        let t = teams(|t| t[0].collision_rule = CollisionRule::PushOwnTeam);
        assert!(!t.pushes(alex, steve));
        assert!(t.pushes(alex, bob) && t.pushes(loner, alex));
        let t = teams(|t| t[0].collision_rule = CollisionRule::PushOtherTeams);
        assert!(t.pushes(alex, steve));
        assert!(!t.pushes(alex, bob) && !t.pushes(bob, alex) && !t.pushes(loner, alex));
        assert!(t.pushes(bob, loner));
    }

    #[crate::watchdog]
    #[tuff_core::watchdog]
    #[test]
    fn names_take_the_team_prefix_suffix_and_color() {
        let t = teams(|t| {
            t[0].prefix = TextComponent::new("[R] ");
            t[0].suffix = TextComponent::new("!");
            t[0].color = Some(super::super::TeamColor::Red);
        });
        let name = t.player_name("Alex", 5);
        assert_eq!(name.color.as_deref(), Some("red"));
        assert_eq!(name.plain(), "[R] Alex!");
        let events = name.events().unwrap();
        assert_eq!(events.insertion.as_deref(), Some("Alex"));
        assert_eq!(
            events.click,
            Some(ClickEvent::SuggestCommand("/tell Alex ".to_owned()))
        );
        assert_eq!(
            events.hover,
            Some(tuff_item::text::Hover::Entity {
                kind: "minecraft:player".to_owned(),
                uuid: 5,
                name: Some(TextComponent::new("Alex")),
            })
        );
        let value = name.to_value();
        assert_eq!(value["hover_event"]["action"], "show_entity");
        assert_eq!(
            value["hover_event"]["uuid"],
            "00000000-0000-0000-0000-000000000005"
        );
        let plain = t.player_name("Loner", 6);
        assert_eq!(plain.plain(), "Loner");
        assert!(plain.extra.is_empty() && plain.color.is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn entity_names_take_their_team_color_and_show_the_entity() {
        let uuid = (1u128 << 64) | 2;
        let t = teams(|t| t[1].color = Some(super::super::TeamColor::Blue));
        let name = t.entity_name("minecraft:zombie", uuid, TextComponent::new("Zed"));
        assert_eq!(name.color.as_deref(), Some("blue"));
        assert_eq!(name.plain(), "Zed");
        let events = name.events().unwrap();
        assert_eq!(
            events.insertion.as_deref(),
            Some("00000000-0000-0001-0000-000000000002")
        );
        assert!(events.click.is_none());
        let value = name.to_value();
        assert_eq!(value["hover_event"]["id"], "minecraft:zombie");
        assert_eq!(value["hover_event"]["name"]["text"], "Zed");
        let canon = format!("{:?}", name.canon());
        assert!(canon.contains("show_entity") && canon.contains("Ints([0, 1, 0, 2])"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn owned_entities_take_their_owners_team() {
        let t = teams(|t| t[0].color = Some(super::super::TeamColor::Red));
        let owned = t.owned_entity_name(
            "minecraft:wolf",
            99,
            TextComponent::new("Rex"),
            Some("Alex"),
        );
        assert_eq!(owned.color.as_deref(), Some("red"));
        let own = t.owned_entity_name("minecraft:wolf", 99, TextComponent::new("Rex"), None);
        assert_eq!(own.color, None);
    }

    #[crate::watchdog]
    #[tuff_core::watchdog]
    #[test]
    fn death_messages_reach_the_audience_the_team_allows() {
        let hears = |visibility, who| {
            teams(|t| t[0].death_message_visibility = visibility)
                .death_audience("Alex")
                .hears(who)
        };
        for who in ["Alex", "Steve", "Bob", "Loner"] {
            assert!(hears(Visibility::Always, who));
            assert!(!hears(Visibility::Never, who));
        }
        assert!(!hears(Visibility::HideForOtherTeams, "Alex"));
        assert!(hears(Visibility::HideForOtherTeams, "Steve"));
        assert!(!hears(Visibility::HideForOtherTeams, "Bob"));
        assert!(!hears(Visibility::HideForOwnTeam, "Alex"));
        assert!(!hears(Visibility::HideForOwnTeam, "Steve"));
        assert!(hears(Visibility::HideForOwnTeam, "Bob"));
        assert!(hears(Visibility::HideForOwnTeam, "Loner"));
        assert!(teams(|_| {}).death_audience("Loner").hears("Alex"));
    }
}
