//! Game mode feedback, shared by `/gamemode` and the game mode switcher.

use crate::protocol::play::GameMode;
use tuff_item::text::TextComponent;

pub fn mode_name(mode: GameMode) -> TextComponent {
    TextComponent::translate(format!("gameMode.{}", mode.name()), [])
}

/// Feedback to a player who changed their own game mode.
pub fn set_self(mode: GameMode) -> TextComponent {
    TextComponent::translate("commands.gamemode.success.self", [mode_name(mode)])
}

/// Feedback to a player who changed someone else's game mode.
pub fn set_other(target: &str, mode: GameMode) -> TextComponent {
    TextComponent::translate(
        "commands.gamemode.success.other",
        [TextComponent::new(target.to_owned()), mode_name(mode)],
    )
}

/// Told to a player whose game mode someone else changed.
pub fn changed(mode: GameMode) -> TextComponent {
    TextComponent::translate("gameMode.changed", [mode_name(mode)])
}
