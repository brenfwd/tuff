//! Chat and command spam: every message adds a score for its length,
//! shouting, wordiness and repeats (the sender's own recent messages and
//! the same text from other players), held in a short and a normal
//! window that both decay. Past either limit the message is dropped.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use tuff_core::protocol::play::Serverbound;

use super::{Check, Facts, Flag};

const ECHOES_KEPT: usize = 128;
const ECHO_WINDOW: Duration = Duration::from_secs(30);
const OWN_HISTORY: usize = 5;
const MAX_KEPT: usize = 256;
const SHORT_HALF_LIFE: f64 = 60.0;
const NORMAL_HALF_LIFE: f64 = 400.0;

/// The key two messages share when they read the same once normalised;
/// `None` for a message with no letters or digits.
pub fn echo_key(text: &str) -> Option<u64> {
    let normal = normalise(text);
    (!normal.is_empty()).then(|| fnv(normal.as_bytes()))
}

/// Recent message keys from every player of a world. Owned by the
/// violation board's actor, which answers each session's lookup.
#[derive(Default)]
pub struct Echoes {
    recent: VecDeque<(u64, u128, Instant)>,
}

impl Echoes {
    /// Counts other players' copies of this key in the window, then
    /// records this one.
    pub fn record(&mut self, key: u64, sender: u128, now: Instant) -> u32 {
        while self
            .recent
            .front()
            .is_some_and(|(_, _, at)| now.saturating_duration_since(*at) > ECHO_WINDOW)
        {
            self.recent.pop_front();
        }
        let copies = self
            .recent
            .iter()
            .filter(|(k, from, _)| *k == key && *from != sender)
            .count();
        if self.recent.len() == ECHOES_KEPT {
            self.recent.pop_front();
        }
        self.recent.push_back((key, sender, now));
        u32::try_from(copies).unwrap_or(u32::MAX)
    }
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Lowercase letters and digits only, runs of one character collapsed,
/// so "HIII!!" and "hi" compare equal.
fn normalise(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars().filter(|c| c.is_alphanumeric()) {
        for lower in c.to_lowercase() {
            if out.ends_with(lower) {
                continue;
            }
            if out.len() >= MAX_KEPT {
                return out;
            }
            out.push(lower);
        }
    }
    out
}

fn similar(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    long.contains(short) && short.len() * 5 >= long.len() * 4
}

/// The score weights and limits of one kind of message.
#[derive(Clone, Copy, Debug)]
struct Weights {
    check: &'static str,
    base: f64,
    length: f64,
    shout: f64,
    words: f64,
    repeat: f64,
    echo: f64,
    short_limit: f64,
    normal_limit: f64,
}

const CHAT: Weights = Weights {
    check: "chat_spam",
    base: 1.0,
    length: 1.0 / 128.0,
    shout: 1.5,
    words: 0.5,
    repeat: 2.5,
    echo: 1.0,
    short_limit: 12.0,
    normal_limit: 50.0,
};

const COMMANDS: Weights = Weights {
    check: "command_spam",
    base: 1.0,
    length: 0.0,
    shout: 0.0,
    words: 0.0,
    repeat: 0.5,
    echo: 0.0,
    short_limit: 20.0,
    normal_limit: 80.0,
};

/// Spam scores of one player's chat or commands.
pub struct Spam {
    weights: Weights,
    short: f64,
    normal: f64,
    at: u64,
    recent: Vec<String>,
}

impl Spam {
    pub fn chat() -> Spam {
        Spam::new(CHAT)
    }

    pub fn commands() -> Spam {
        Spam::new(COMMANDS)
    }

    fn new(weights: Weights) -> Spam {
        Spam {
            weights,
            short: 0.0,
            normal: 0.0,
            at: 0,
            recent: Vec::new(),
        }
    }

    fn score(&mut self, text: &str, echoes: u32) -> f64 {
        let w = self.weights;
        let chars = text.chars().count();
        let letters = text.chars().filter(|c| c.is_alphabetic()).count();
        let upper = text.chars().filter(|c| c.is_uppercase()).count();
        let words = text.split_whitespace().count();
        let normal = normalise(text);
        let repeats = self
            .recent
            .iter()
            .filter(|old| similar(old, &normal))
            .count();
        if self.recent.len() == OWN_HISTORY {
            self.recent.remove(0);
        }
        self.recent.push(normal);
        let mut score = w.base + w.length * chars.min(MAX_KEPT) as f64;
        if letters >= 8 && upper * 10 > letters * 6 {
            score += w.shout;
        }
        if words > 12 {
            score += w.words;
        }
        score += w.repeat * repeats.min(2) as f64;
        score += w.echo * f64::from(echoes.min(5));
        score
    }

    fn decay(&mut self, tick: u64) {
        let dt = tick.saturating_sub(self.at) as f64;
        self.at = tick;
        self.short *= 0.5f64.powf(dt / SHORT_HALF_LIFE);
        self.normal *= 0.5f64.powf(dt / NORMAL_HALF_LIFE);
    }
}

impl Check for Spam {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        let text = match (packet, self.weights.check) {
            (Serverbound::Chat(text), "chat_spam")
            | (Serverbound::ChatCommand(text), "command_spam") => text,
            _ => return None,
        };
        self.decay(facts.tick);
        let score = self.score(text, facts.echoes);
        self.short += score;
        self.normal += score;
        if self.short > self.weights.short_limit || self.normal > self.weights.normal_limit {
            self.short -= score;
            self.normal -= score;
            return Flag::cancel(self.weights.check, 1.0);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::super::facts;
    use super::*;

    fn run(check: &mut Spam, lines: &[&str], every: u64, chat: bool) -> usize {
        let mut f = facts();
        let mut flagged = 0;
        for line in lines {
            let packet = if chat {
                Serverbound::Chat((*line).to_owned())
            } else {
                Serverbound::ChatCommand((*line).to_owned())
            };
            flagged += usize::from(check.examine(&f, &packet).is_some());
            f.tick += every;
        }
        flagged
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_lively_conversation_is_never_flagged() {
        let talk = [
            "hey everyone",
            "anyone want to go mining?",
            "sure, meet at spawn",
            "i found diamonds!!",
            "lol nice",
            "OMG",
            "where are you",
            "near the big oak by the river, follow the torches",
            "ok coming",
            "gg",
        ];
        let lines: Vec<&str> = talk.iter().cycle().take(200).copied().collect();
        assert_eq!(run(&mut Spam::chat(), &lines, 30, true), 0);
        assert_eq!(run(&mut Spam::chat(), &talk, 10, true), 0, "a quick burst");
        let commands: Vec<&str> = std::iter::repeat_n("home", 60).collect();
        assert_eq!(run(&mut Spam::commands(), &commands, 20, false), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn repeats_shouting_and_floods_are_dropped() {
        let same: Vec<&str> =
            std::iter::repeat_n("buy cheap ranks at example dot com", 10).collect();
        assert!(run(&mut Spam::chat(), &same, 20, true) >= 5);
        let shout: Vec<&str> = std::iter::repeat_n("WHY IS NOBODY ANSWERING ME", 10).collect();
        assert!(run(&mut Spam::chat(), &shout, 10, true) > 0);
        let flood: Vec<String> = (0..40).map(|n| format!("tp {n}")).collect();
        let flood: Vec<&str> = flood.iter().map(String::as_str).collect();
        assert!(run(&mut Spam::commands(), &flood, 1, false) > 10);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_same_text_from_other_players_counts_as_an_echo() {
        let mut echoes = Echoes::default();
        let now = Instant::now();
        let key = |text| echo_key(text).unwrap();
        assert_eq!(echoes.record(key("Join my server!"), 1, now), 0);
        assert_eq!(
            echoes.record(key("join my serverrr"), 1, now),
            0,
            "own copies are not echoes"
        );
        assert_eq!(echoes.record(key("JOIN MY SERVER"), 2, now), 2);
        assert_eq!(
            echoes.record(key("join my server"), 3, now + Duration::from_secs(5)),
            3
        );
        assert_eq!(
            echoes.record(key("join my server"), 4, now + Duration::from_secs(120)),
            0,
            "old copies expire"
        );
        assert_eq!(echo_key("!!! ..."), None);
        let mut check = Spam::chat();
        let mut f = facts();
        f.echoes = 5;
        let packet = Serverbound::Chat("join my server".to_owned());
        let flagged = (0..4)
            .filter(|n| {
                f.tick = n * 40;
                check.examine(&f, &packet).is_some()
            })
            .count();
        assert!(flagged > 0);
    }
}
