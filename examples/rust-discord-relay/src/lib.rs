// SPDX-License-Identifier: MIT
#![deny(deprecated)]
use std::collections::VecDeque;

use tuff_plugin_sdk::net::{self, Request, json_string};
use tuff_plugin_sdk::{files, prelude::*};

static WEBHOOK: Setting<String> = Setting::new(
    "webhook",
    "",
    "Discord webhook URL that chat, joins and leaves are posted to",
)
.secret();
const FAILED: &str = "failed.log";
const QUEUE_LIMIT: usize = 64;
const MAX_ATTEMPTS: u32 = 3;
const DEFAULT_RETRY_TICKS: u64 = 100;
const MAX_RETRY_TICKS: u64 = 20 * 60;

#[derive(Default)]
struct Relay {
    queue: VecDeque<String>,
    sending: bool,
}

plugin_state!(RELAY: Relay = Relay::default());

fn relay<R>(f: impl FnOnce(&mut Relay) -> R) -> R {
    RELAY.with(f)
}

/// Discord renders markdown and mentions; neutralize what a player can type.
fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' | '*' | '_' | '~' | '`' | '|' | '>' | '[' | ']' | '(' | ')' => {
                out.push('\\');
                out.push(c);
            }
            '@' => out.push_str("@\u{200b}"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out.chars().take(1500).collect()
}

fn body(line: &str) -> String {
    format!(
        "{{\"content\":{},\"allowed_mentions\":{{\"parse\":[]}}}}",
        json_string(line)
    )
}

fn retry_ticks(response: &net::Response) -> u64 {
    let seconds = response
        .header("retry-after")
        .and_then(|v| v.trim().parse::<f64>().ok());
    match seconds {
        Some(s) if s.is_finite() && s >= 0.0 => {
            ((s * 20.0).ceil() as u64).clamp(1, MAX_RETRY_TICKS)
        }
        _ => DEFAULT_RETRY_TICKS,
    }
}

async fn deliver(url: &str, line: &str) -> Result<(), String> {
    for _ in 0..MAX_ATTEMPTS {
        let response = Request::new("POST", url)
            .json(body(line))
            .timeout_ms(10_000)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        match response.status {
            200..=299 => return Ok(()),
            429 => sleep(retry_ticks(&response)).await,
            status => return Err(format!("discord answered {status}")),
        }
    }
    Err("rate limited".to_owned())
}

async fn drain() {
    loop {
        let url = WEBHOOK.get();
        let next = relay(|r| match r.queue.pop_front() {
            Some(line) if !url.is_empty() => Some((line, url)),
            _ => {
                r.queue.clear();
                r.sending = false;
                None
            }
        });
        let Some((line, url)) = next else { return };
        if let Err(reason) = deliver(&url, &line).await {
            log(Level::Warn, &format!("relay failed: {reason}"));
            let entry = format!("{reason}\t{line}\n");
            if let Err(e) = files::append(FAILED, entry).await {
                log(Level::Warn, &format!("cannot record the failure: {e}"));
            }
        }
    }
}

fn enqueue(line: String) {
    let start = relay(|r| {
        if WEBHOOK.get().is_empty() || r.queue.len() >= QUEUE_LIMIT {
            return false;
        }
        r.queue.push_back(line);
        !std::mem::replace(&mut r.sending, true)
    });
    if start {
        spawn(drain());
    }
}

struct DiscordRelay;

impl Plugin for DiscordRelay {
    fn register() -> Result<(), String> {
        WEBHOOK.declare()?;
        on::<ev::Chat>(|chat| {
            enqueue(format!(
                "**{}**: {}",
                sanitize(&chat.player.name),
                sanitize(&chat.message)
            ));
        })?;
        on::<ev::Join>(|player| {
            enqueue(format!("{} joined the game", sanitize(&player.name)));
        })?;
        on::<ev::Leave>(|player| {
            enqueue(format!("{} left the game", sanitize(&player.name)));
        })?;
        Ok(())
    }

    fn enable() {
        if WEBHOOK.get().is_empty() {
            log(
                Level::Warn,
                "set plugins.discord-relay.webhook to the Discord webhook URL",
            );
        } else {
            enqueue("Server relay online.".to_owned());
        }
    }

    fn disable() {
        relay(|r| *r = Relay::default());
    }
}

plugin!(DiscordRelay {
    id: "discord-relay",
    version: "1.0.0",
    description: "Relays chat, joins and leaves to a Discord webhook",
    caps: [
        http("discord.com/api/webhooks/", "Posts relayed chat to your Discord webhook"),
        files("1 MiB", "Keeps a log of failed relays"),
    ],
});
