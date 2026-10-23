//! `/tuff web login|logout|logout-all`: one-time login codes for the web
//! admin panel and ending its sessions. `/tuff web token create <name>
//! <level> [expiry]|list|revoke <id or name>` manages API tokens for
//! scripts (`tuff.web.panel.admin`, like the panel's own token page).

use std::time::{SystemTime, UNIX_EPOCH};

use tuff_core::command::args::Value;
use tuff_core::command::{CommandError, CommandNode, CommandNodeKind};
use tuff_core::world::commands::{Context, SourceKind, text};
use tuff_item::text::{ClickEvent, TextComponent};
use tuff_storage::admin::PermissionLevel;
use tuff_web_ui as web;
use tuff_web_ui::Principal;
use tuff_web_ui::auth::{TokenInfo, valid_token_name};

use crate::tuff::{any_child, guarded, literal, string_argument};

const ACCESS: &str = r#"{"type":"tuff:permission","node":"tuff.web.access"}"#;
const LOGIN: CommandNode = guarded(literal("login", true, &[]), ACCESS);
const LOGOUT: CommandNode = guarded(literal("logout", true, &[]), ACCESS);
const LOGOUT_ALL: CommandNode = guarded(
    literal("logout-all", true, &[]),
    r#"{"type":"tuff:permission","node":"tuff.web.logout-all"}"#,
);
const WORD: &str = r#"{"type":"word"}"#;
const EXPIRY: CommandNode = string_argument("expiry", WORD, true, &[]);
const LEVEL: CommandNode = CommandNode {
    name: "level",
    kind: CommandNodeKind::Argument {
        parser: "brigadier:integer",
        properties: Some(r#"{"min":0,"max":4}"#),
    },
    executable: true,
    redirect: None,
    permissions: None,
    children: &[EXPIRY],
};
const TOKEN: CommandNode = guarded(
    literal(
        "token",
        false,
        &[
            literal(
                "create",
                false,
                &[string_argument("name", WORD, false, &[LEVEL])],
            ),
            literal("list", true, &[]),
            literal("revoke", false, &[string_argument("id", WORD, true, &[])]),
        ],
    ),
    r#"{"type":"tuff:permission","node":"tuff.web.panel.admin"}"#,
);
pub const WEB: CommandNode = any_child(literal("web", false, &[LOGIN, LOGOUT, LOGOUT_ALL, TOKEN]));

/// The longest `[expiry]` accepted: ten years.
const MAX_EXPIRY_SECS: u64 = 10 * 365 * 24 * 3600;

/// Seconds in an `[expiry]` like `90m`, `12h`, `30d`, `2w` or `1y`;
/// `Some(None)` for `never`.
pub fn parse_expiry(text: &str) -> Option<Option<u64>> {
    if text.eq_ignore_ascii_case("never") {
        return Some(None);
    }
    let split = text.len().checked_sub(1)?;
    let (count, unit) = text.split_at(split);
    let unit: u64 = match unit {
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        "y" => 365 * 86_400,
        _ => return None,
    };
    let count: u64 = count.parse().ok().filter(|&c| c > 0)?;
    let secs = count.checked_mul(unit)?;
    (secs <= MAX_EXPIRY_SECS).then_some(Some(secs))
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// A span of milliseconds as its largest whole unit, like `3d` or `5m`.
fn span(millis: u64) -> String {
    let secs = millis / 1000;
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

fn describe(token: &TokenInfo, now: u64) -> String {
    let used = match token.last_used {
        Some(at) => format!("used {} ago", span(now.saturating_sub(at))),
        None => "never used".to_owned(),
    };
    let expires = match token.expires {
        Some(at) if at <= now => "expired".to_owned(),
        Some(at) => format!("expires in {}", span(at - now)),
        None => "no expiry".to_owned(),
    };
    format!(
        "{} {} (level {}, by {}, created {} ago, {used}, {expires})",
        token.id,
        token.name,
        token.level.id(),
        token.by,
        span(now.saturating_sub(token.created)),
    )
}

fn fail(message: impl Into<String>) -> CommandError {
    CommandError::text(text(message.into()))
}

async fn token(ctx: &mut Context<'_>, principal: Principal) -> Result<i32, CommandError> {
    let shared = ctx.shared().clone();
    let auth = &shared.web.auth;
    let audit = |action: &str, target: &str| {
        web::audit::Entry::new(principal.name(), action).target(target)
    };
    match ctx.literals().get(3).copied() {
        Some("create") => {
            let Some(Value::Str(name)) = ctx.arg("name").cloned() else {
                return Err(CommandError::simple("command.unknown.command"));
            };
            if !valid_token_name(&name) {
                return Err(fail("A token name is 1 to 64 letters, digits, _, - or ."));
            }
            let level = match ctx.arg("level") {
                Some(Value::Int(level)) => PermissionLevel::from_id(i64::from(*level)),
                _ => None,
            }
            .ok_or_else(|| fail("The level must be 0 to 4"))?;
            if level > ctx.source.permission {
                return Err(fail("A token cannot outrank you"));
            }
            let secs = match ctx.arg("expiry") {
                Some(Value::Str(expiry)) => parse_expiry(expiry).ok_or_else(|| {
                    fail(format!(
                        "Unknown expiry {expiry}: use never, or a count with m, h, d, w or y \
                         (like 30d), up to 10 years"
                    ))
                })?,
                _ => None,
            };
            let expires = secs.map(|s| now_millis() + s * 1000);
            let Some(made) = auth
                .create_token(name.clone(), level, expires, principal.name())
                .await
            else {
                return Err(fail(
                    "Could not create the token: too many exist or the web panel is not running",
                ));
            };
            shared
                .web
                .audit
                .record(audit("token.create", &name).detail(format!("level {}", level.id())));
            let lasts = match secs {
                Some(s) => format!("expires in {}", span(s * 1000)),
                None => "never expires".to_owned(),
            };
            ctx.reply(
                text(format!(
                    "API token {name} (level {}, {lasts}): ",
                    level.id()
                ))
                .append(TextComponent::copy_on_click(made.secret))
                .append(text(
                    " -- send it as Authorization: Bearer <token>; it is shown only once",
                )),
            );
            Ok(1)
        }
        Some("list") => {
            let tokens = auth.tokens().await;
            if tokens.is_empty() {
                ctx.reply(text("No API tokens"));
                return Ok(0);
            }
            let now = now_millis();
            ctx.reply(text(format!("{} API token(s):", tokens.len())));
            for token in &tokens {
                ctx.reply(text(describe(token, now)));
            }
            Ok(tokens.len() as i32)
        }
        Some("revoke") => {
            let Some(Value::Str(wanted)) = ctx.arg("id").cloned() else {
                return Err(CommandError::simple("command.unknown.command"));
            };
            let tokens = auth.tokens().await;
            let named: Vec<&TokenInfo> = tokens.iter().filter(|t| t.name == wanted).collect();
            let id = match tokens.iter().find(|t| t.id == wanted) {
                Some(token) => token.id.clone(),
                None => match named.as_slice() {
                    [token] => token.id.clone(),
                    [] => return Err(fail(format!("No API token {wanted}"))),
                    _ => {
                        return Err(fail(format!(
                            "{} tokens are named {wanted}; revoke one by id",
                            named.len()
                        )));
                    }
                },
            };
            if !auth.revoke_token(&id).await {
                return Err(fail(format!("No API token {wanted}")));
            }
            shared.web.audit.record(audit("token.revoke", &id));
            ctx.reply(text(format!(
                "Revoked API token {id} and ended the sessions it opened"
            )));
            Ok(1)
        }
        _ => Err(CommandError::simple("command.unknown.command")),
    }
}

/// Who `/tuff web` acts for: the panel user a command came from, the
/// player running it, or the console.
pub(crate) fn principal(ctx: &Context<'_>) -> Principal {
    if let Some(principal) = web::panel_principal() {
        return principal;
    }
    match &ctx.source.kind {
        SourceKind::Player { uuid, .. } => Principal::Operator {
            uuid: *uuid,
            name: ctx.source.name.clone(),
        },
        SourceKind::Console(_) => Principal::Console,
    }
}

pub async fn run(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let shared = ctx.shared().clone();
    let principal = principal(ctx);
    if matches!(principal, Principal::Operator { .. })
        && !shared.online_mode
        && !shared
            .config
            .get_bool(web::TRUST_OFFLINE_NAMES)
            .unwrap_or(false)
        && matches!(ctx.literals().get(2).copied(), Some("login" | "token"))
    {
        return Err(CommandError::text(text(format!(
            "Offline-mode names are not authenticated, so the web panel is closed to players; use the console, or set {} true to accept the risk",
            web::TRUST_OFFLINE_NAMES
        ))));
    }
    match ctx.literals().get(2).copied() {
        Some("token") => {
            if !ctx.permits(tuff_web_ui::auth::WEB_PANEL_ADMIN) {
                return Err(CommandError::simple("command.unknown.command"));
            }
            token(ctx, principal).await
        }
        Some("login") => {
            if !web::enabled(&*shared) {
                return Err(CommandError::text(text(format!(
                    "The web panel is off; enable it with /tuff config {} true",
                    web::ENABLED
                ))));
            }
            let Some(code) = shared.web.auth.new_code(principal).await else {
                return Err(CommandError::text(text("The web panel is not running")));
            };
            let url = web::public_url(&*shared).await;
            ctx.reply(
                text("Web panel login code: ")
                    .append(TextComponent::copy_on_click(code))
                    .append(" (single use, expires in 2 minutes). Enter it at ")
                    .append(
                        TextComponent::click(text(&url), ClickEvent::OpenUrl(url)).color("aqua"),
                    )
                    .append("."),
            );
            Ok(1)
        }
        Some("logout") => {
            let ended = shared.web.auth.logout_principal(principal.clone()).await;
            shared.web.audit.record(
                web::audit::Entry::new(principal.name(), "logout-own").detail(ended.to_string()),
            );
            ctx.reply(text(format!("Ended {ended} web panel session(s)")));
            Ok(ended as i32)
        }
        Some("logout-all") => {
            let ended = shared.web.auth.logout_all().await;
            shared.web.audit.record(
                web::audit::Entry::new(principal.name(), "logout-all").detail(ended.to_string()),
            );
            ctx.reply(text(format!("Ended all {ended} web panel session(s)")));
            Ok(ended as i32)
        }
        _ => Err(CommandError::simple("command.unknown.command")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn expiries_parse_with_units_and_a_cap() {
        assert_eq!(parse_expiry("never"), Some(None));
        assert_eq!(parse_expiry("90m"), Some(Some(5400)));
        assert_eq!(parse_expiry("30d"), Some(Some(30 * 86_400)));
        assert_eq!(parse_expiry("10y"), Some(Some(MAX_EXPIRY_SECS)));
        for bad in [
            "",
            "d",
            "0d",
            "11y",
            "5s",
            "-1d",
            "1.5h",
            "99999999999999999999y",
        ] {
            assert_eq!(parse_expiry(bad), None, "{bad}");
        }
    }
}
