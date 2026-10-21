use std::collections::VecDeque;

/// Facts the harness extracts from client output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientEvent {
    Connecting(String),
    Disconnected(String),
    ProtocolError(String),
    Chat(String),
}

pub fn parse_line(line: &str) -> Option<ClientEvent> {
    if let Some((_, text)) = line.split_once("[CHAT] ") {
        return Some(ClientEvent::Chat(text.trim_end().to_owned()));
    }
    if let Some((_, addr)) = line.split_once("Connecting to ") {
        return Some(ClientEvent::Connecting(addr.trim_end().to_owned()));
    }
    for marker in [
        "Client disconnected with reason: ",
        "Disconnected from server: ",
    ] {
        if let Some((_, reason)) = line.split_once(marker) {
            return Some(ClientEvent::Disconnected(reason.trim_end().to_owned()));
        }
    }
    const ERRORS: [&str; 3] = [
        "Error receiving packet",
        "Network Protocol Error",
        "Failed to decode packet",
    ];
    ERRORS
        .iter()
        .any(|m| line.contains(m))
        .then(|| ClientEvent::ProtocolError(line.trim_end().to_owned()))
}

pub const TAIL_LINES: usize = 50;

/// Bounded ring of the most recent client output lines.
#[derive(Debug, Default)]
pub struct LogTail(VecDeque<String>);

impl LogTail {
    pub fn push(&mut self, line: String) {
        if self.0.len() == TAIL_LINES {
            self.0.pop_front();
        }
        self.0.push_back(line);
    }

    pub fn render(&self) -> String {
        self.0
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
