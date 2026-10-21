// SPDX-License-Identifier: MIT
//! Declarative command and chat filters, declared while registering.

use crate::{host, types};

pub use types::LineAction;

/// Cancels (`LineAction::Cancel`) or rewrites a command line whose leading
/// words are `prefix`, before any command runs.
pub fn intercept_command(prefix: &str, action: LineAction) -> Result<(), types::RegisterError> {
    host::intercept_command(&types::CommandFilter {
        prefix: prefix.to_owned(),
        action,
    })
}

/// Cancels or rewrites chat messages that start with `prefix` and contain
/// `contains` (each optional) before they are sent.
pub fn intercept_chat(
    prefix: Option<&str>,
    contains: Option<&str>,
    action: LineAction,
) -> Result<(), types::RegisterError> {
    host::intercept_chat(&types::ChatFilter {
        prefix: prefix.map(str::to_owned),
        contains: contains.map(str::to_owned),
        action,
    })
}
