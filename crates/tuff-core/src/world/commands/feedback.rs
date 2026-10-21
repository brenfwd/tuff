//! Feedback for commands that act on several targets, matching the game's
//! response tracker: one message names the single affected target, another
//! counts them, and an optional error covers nobody being affected. Replies
//! arrive from other actors, so they are gathered by a detached task that
//! delivers the feedback when they are all in.

use std::time::Duration;

use tokio::sync::mpsc;

use crate::world::Shared;
use crate::world::messages::{CoordinatorMessage, DirectiveReply};
use tuff_item::text::TextComponent;

use super::FeedbackTarget;

/// How long a collector waits for replies from targets that never answer.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

pub type Single = Box<dyn Fn(&DirectiveReply) -> TextComponent + Send + Sync>;
pub type Multiple = Box<dyn Fn(usize, i32) -> TextComponent + Send + Sync>;
pub type OnZero = Box<dyn Fn(&[DirectiveReply]) -> TextComponent + Send + Sync>;

pub struct Messages {
    pub single: Single,
    pub multiple: Multiple,
    /// The error when no target counts.
    pub on_zero: Option<OnZero>,
    /// Count targets whose value is zero too (the game's `ANY`).
    pub any: bool,
    pub broadcast: bool,
    /// Deliver nothing: the command sends its own feedback.
    pub silent: bool,
}

impl Messages {
    pub fn new(
        single: impl Fn(&DirectiveReply) -> TextComponent + Send + Sync + 'static,
        multiple: impl Fn(usize, i32) -> TextComponent + Send + Sync + 'static,
    ) -> Self {
        Self {
            single: Box::new(single),
            multiple: Box::new(multiple),
            on_zero: None,
            any: false,
            broadcast: true,
            silent: false,
        }
    }

    pub fn silent(mut self) -> Self {
        self.silent = true;
        self
    }

    pub fn on_zero(
        mut self,
        error: impl Fn(&[DirectiveReply]) -> TextComponent + Send + Sync + 'static,
    ) -> Self {
        self.on_zero = Some(Box::new(error));
        self
    }

    pub fn any(mut self) -> Self {
        self.any = true;
        self
    }

    pub fn quiet(mut self) -> Self {
        self.broadcast = false;
        self
    }

    /// The feedback for the gathered replies: `Ok` to show as success,
    /// `Err` to show in red.
    pub fn resolve(&self, replies: &[DirectiveReply]) -> Result<TextComponent, TextComponent> {
        let tracked: Vec<&DirectiveReply> = replies.iter().filter(|r| r.affected).collect();
        if tracked.len() == 1
            && let Some(error) = &tracked[0].error
        {
            return Err(error.clone());
        }
        if replies.len() == 1
            && let Some(error) = &replies[0].error
        {
            return Err(error.clone());
        }
        let counted: Vec<&DirectiveReply> = tracked
            .iter()
            .copied()
            .filter(|r| self.any || r.value != 0)
            .collect();
        let total: i32 = tracked.iter().map(|r| r.value).sum();
        if counted.is_empty()
            && let Some(on_zero) = &self.on_zero
        {
            return Err(on_zero(replies));
        }
        Ok(match counted.as_slice() {
            [only] => (self.single)(only),
            _ => (self.multiple)(counted.len(), total),
        })
    }
}

/// Delivers a command's success the way the game does: to the source
/// unless a player turned feedback off, and to operators when `broadcast`.
pub async fn success(
    shared: &Shared,
    source: &FeedbackTarget,
    source_name: TextComponent,
    message: TextComponent,
    broadcast: bool,
) {
    let feedback_on = shared.rule_bool("send_command_feedback");
    let from_block = source.is_command_block();
    if (source.player_id().is_none() && !from_block) || feedback_on {
        source.send(message.clone());
    }
    let console = matches!(source, FeedbackTarget::Console | FeedbackTarget::Channel(_));
    if broadcast
        && !matches!(source, FeedbackTarget::Silent)
        && (!console || shared.live.network().broadcast_console_to_ops)
        && (!from_block || shared.rule_bool("command_block_output"))
    {
        let _ = shared
            .coordinator
            .send(CoordinatorMessage::AdminBroadcast {
                source: source.clone(),
                source_name,
                message,
            })
            .await;
    }
}

pub fn fail(source: &FeedbackTarget, message: TextComponent) {
    source.send(message.color("red"));
}

/// Waits for up to `expected` replies, giving up after the reply timeout.
pub async fn gather(
    replies: &mut mpsc::UnboundedReceiver<DirectiveReply>,
    expected: usize,
) -> Vec<DirectiveReply> {
    let mut gathered = Vec::with_capacity(expected);
    let deadline = tokio::time::Instant::now() + REPLY_TIMEOUT;
    while gathered.len() < expected {
        match tokio::time::timeout_at(deadline, replies.recv()).await {
            Ok(Some(reply)) => gathered.push(reply),
            Ok(None) | Err(_) => break,
        }
    }
    gathered
}

/// Waits for every target's reply, then sends the feedback. Returns once
/// delivered, so a caller that may wait (the console) can.
pub async fn collect(
    shared: std::sync::Arc<Shared>,
    source: FeedbackTarget,
    source_name: TextComponent,
    mut replies: mpsc::UnboundedReceiver<DirectiveReply>,
    expected: usize,
    messages: Messages,
) {
    let gathered = gather(&mut replies, expected).await;
    if messages.silent {
        return;
    }
    match messages.resolve(&gathered) {
        Ok(message) => success(&shared, &source, source_name, message, messages.broadcast).await,
        Err(error) => fail(&source, error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(name: &str, value: i32, affected: bool) -> DirectiveReply {
        DirectiveReply {
            player: 1,
            name: name.into(),
            value,
            affected,
            error: None,
            attribute: None,
            post_effects: Vec::new(),
        }
    }

    fn messages() -> Messages {
        Messages::new(
            |r| TextComponent::new(format!("single {} {}", r.name, r.value)),
            |count, total| TextComponent::new(format!("multiple {count} {total}")),
        )
        .on_zero(|_| TextComponent::new("none"))
    }

    #[tuff_core::watchdog]
    #[test]
    fn resolves_single_multiple_and_zero_like_the_game() {
        let m = messages();
        assert_eq!(
            m.resolve(&[reply("A", 3, true)]).unwrap().text,
            "single A 3"
        );
        assert_eq!(
            m.resolve(&[reply("A", 3, true), reply("B", 2, true)])
                .unwrap()
                .text,
            "multiple 2 5"
        );
        assert_eq!(
            m.resolve(&[reply("A", 3, true), reply("B", 0, true)])
                .unwrap()
                .text,
            "single A 3"
        );
        assert_eq!(m.resolve(&[reply("A", 0, true)]).unwrap_err().text, "none");
        assert_eq!(m.resolve(&[reply("A", 0, false)]).unwrap_err().text, "none");
        assert_eq!(m.resolve(&[]).unwrap_err().text, "none");
        let any = messages().any();
        assert_eq!(
            any.resolve(&[reply("A", 0, true)]).unwrap().text,
            "single A 0"
        );
        let no_error = Messages::new(
            |r| TextComponent::new(r.name.clone()),
            |count, total| TextComponent::new(format!("{count}/{total}")),
        );
        assert_eq!(
            no_error.resolve(&[reply("A", 0, true)]).unwrap().text,
            "0/0"
        );
        let mut failed = reply("A", 0, false);
        failed.error = Some(TextComponent::new("nope"));
        assert_eq!(m.resolve(&[failed.clone()]).unwrap_err().text, "nope");
        assert_eq!(
            m.resolve(&[failed, reply("B", 1, true)]).unwrap().text,
            "single B 1",
            "errors only surface for a single target"
        );
    }
}
