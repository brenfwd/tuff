//! What the player's play feeds into scoreboard objectives: statistics,
//! deaths and kills, and the vitals the read-only criteria mirror. Nothing
//! goes to the scoreboard unless some objective counts it.

use crate::world::scoreboard::{Criterion, Holder};
use tuff_vanilla_data::registry::Attribute;

use super::PlayerSession;

const VITALS: [Criterion; 6] = [
    Criterion::Health,
    Criterion::Food,
    Criterion::Air,
    Criterion::Armor,
    Criterion::Xp,
    Criterion::Level,
];

impl PlayerSession {
    fn score_holder(&self) -> Holder {
        Holder {
            name: self.profile.name.clone(),
            display: Some(tuff_item::text::TextComponent::new(
                self.profile.name.clone(),
            )),
        }
    }

    fn vitals(&self) -> [i32; 6] {
        [
            (self.body.health.value + self.body.absorption).ceil() as i32,
            self.body.food.level,
            self.body.air,
            self.attribute(Attribute::Armor).floor() as i32,
            self.body.experience.total,
            self.body.experience.level,
        ]
    }

    /// Once a tick: statistics awarded since last time, and any vital that
    /// changed.
    pub(super) async fn tick_scores(&mut self) {
        let scoreboard = self.shared.scoreboard.clone();
        let stats = if scoreboard.tracks_stats() {
            let taken = self
                .progress
                .stats
                .scored
                .replace(Vec::new())
                .unwrap_or_default();
            taken
                .into_iter()
                .filter_map(|(stat, change)| {
                    let criterion = Criterion::Stat(stat);
                    scoreboard.tracks(&criterion).then_some((criterion, change))
                })
                .collect()
        } else {
            self.progress.stats.scored = None;
            Vec::new()
        };
        let current = self.vitals();
        let mut vitals = Vec::new();
        for (index, value) in current.into_iter().enumerate() {
            if self.published.last_vitals[index] != Some(value) {
                self.published.last_vitals[index] = Some(value);
                if scoreboard.tracks(&VITALS[index]) {
                    vitals.push((VITALS[index].clone(), value));
                }
            }
        }
        if stats.is_empty() && vitals.is_empty() {
            return;
        }
        let holder = self.score_holder();
        scoreboard
            .apply(move |board| {
                for (criterion, change) in stats {
                    match change {
                        Some(amount) => board.add_for(&criterion, &holder, amount),
                        None => board.set_for(&criterion, &holder, 0),
                    }
                }
                for (criterion, value) in vitals {
                    board.set_for(&criterion, &holder, value);
                }
            })
            .await;
    }

    pub(super) async fn score_death(&self) {
        let scoreboard = &self.shared.scoreboard;
        if scoreboard.tracks(&Criterion::DeathCount) {
            let holder = self.score_holder();
            scoreboard
                .apply(move |board| board.add_for(&Criterion::DeathCount, &holder, 1))
                .await;
        }
    }

    pub(super) async fn score_kill(&self, victim: &str, victim_is_player: bool) {
        let scoreboard = &self.shared.scoreboard;
        if !scoreboard.tracks(&Criterion::TotalKillCount)
            && !scoreboard.tracks(&Criterion::PlayerKillCount)
            && !scoreboard.tracks_team_kills()
        {
            return;
        }
        let killer = self.score_holder();
        let victim = Holder::named(victim);
        scoreboard
            .apply(move |board| board.award_kill(&killer, &victim, victim_is_player))
            .await;
    }
}
