//! What a replay viewer sees of its controls: the command tree, the
//! clickable chat reply and the server list entry.

use tuff_core::protocol::play::{CommandNodeData, command_node};
use tuff_core::protocol::{PROTOCOL_VERSION, VERSION_NAME};
use tuff_item::text::{ClickEvent, TextComponent};

const DOUBLE_PARSER: i32 = 2;
const STRING_PARSER: i32 = 5;
const ASK_SERVER: &str = "minecraft:ask_server";
const TELEPORT_WORDS: [&str; 2] = ["tp", "teleport"];
const SKIP_SECONDS: i32 = 10;

const WORDS: [&str; 8] = [
    "play", "pause", "restart", "status", "hotbar", "lead", "follow", "unfollow",
];
const ARGUMENT_WORDS: [&str; 3] = ["speed", "seek", "skip"];

/// The `/replay` command tree: root, `replay`, then one literal per
/// subcommand, the numeric ones followed by a double argument.
pub fn command_tree() -> Vec<CommandNodeData> {
    let mut nodes = vec![
        CommandNodeData {
            flags: command_node::ROOT,
            children: vec![1],
            redirect: None,
            name: None,
            parser: None,
            suggestions: None,
        },
        CommandNodeData {
            flags: command_node::LITERAL | command_node::EXECUTABLE,
            children: Vec::new(),
            redirect: None,
            name: Some("replay"),
            parser: None,
            suggestions: None,
        },
    ];
    for word in WORDS {
        let index = nodes.len() as i32;
        nodes[1].children.push(index);
        nodes.push(CommandNodeData {
            flags: command_node::LITERAL | command_node::EXECUTABLE,
            children: Vec::new(),
            redirect: None,
            name: Some(word),
            parser: None,
            suggestions: None,
        });
    }
    for word in ARGUMENT_WORDS {
        let index = nodes.len() as i32;
        nodes[1].children.push(index);
        nodes.push(CommandNodeData {
            flags: command_node::LITERAL,
            children: vec![index + 1],
            redirect: None,
            name: Some(word),
            parser: None,
            suggestions: None,
        });
        nodes.push(CommandNodeData {
            flags: command_node::ARGUMENT | command_node::EXECUTABLE,
            children: Vec::new(),
            redirect: None,
            name: Some(match word {
                "speed" => "multiplier",
                _ => "seconds",
            }),
            parser: Some((DOUBLE_PARSER, vec![0])),
            suggestions: None,
        });
    }
    for word in TELEPORT_WORDS {
        let index = nodes.len() as i32;
        nodes[0].children.push(index);
        nodes.push(CommandNodeData {
            flags: command_node::LITERAL,
            children: vec![index + 1, index + 2],
            redirect: None,
            name: Some(word),
            parser: None,
            suggestions: None,
        });
        nodes.push(CommandNodeData {
            flags: command_node::ARGUMENT | command_node::EXECUTABLE,
            children: Vec::new(),
            redirect: None,
            name: Some("target"),
            parser: Some((STRING_PARSER, vec![0])),
            suggestions: Some(ASK_SERVER),
        });
        for (i, axis) in ["x", "y", "z"].into_iter().enumerate() {
            let at = nodes.len() as i32;
            let last = i == 2;
            nodes.push(CommandNodeData {
                flags: command_node::ARGUMENT | if last { command_node::EXECUTABLE } else { 0 },
                children: if last { Vec::new() } else { vec![at + 1] },
                redirect: None,
                name: Some(axis),
                parser: Some((DOUBLE_PARSER, vec![0])),
                suggestions: None,
            });
        }
    }
    nodes
}

/// The argument being typed after `/tp ` or `/teleport `, as the start of
/// that word in `text` and the partial word, when it is the first argument.
pub fn teleport_partial(text: &str) -> Option<(usize, &str)> {
    let rest = text.strip_prefix('/')?;
    let (command, partial) = rest.split_once(' ')?;
    if !TELEPORT_WORDS.contains(&command) || partial.contains(' ') {
        return None;
    }
    Some((command.len() + 2, partial))
}

/// A reply shown above the hotbar.
pub fn action_bar(text: String) -> bytes::Bytes {
    crate::transform::encode(&tuff_core::protocol::play::SystemChat {
        content: &TextComponent::new(text),
        overlay: true,
    })
}

fn button(label: &str, command: String, hint: &str) -> TextComponent {
    TextComponent::new(format!("[{label}]"))
        .color("aqua")
        .click(ClickEvent::RunCommand(command))
        .hover_text(TextComponent::new(hint))
}

/// The status line followed by clickable playback buttons.
pub fn chat_controls(status: String) -> TextComponent {
    let mut line = TextComponent::new(status);
    let buttons = [
        button("play", "/replay play".into(), "Resume playback"),
        button("pause", "/replay pause".into(), "Pause playback"),
        button(
            &format!("-{SKIP_SECONDS}s"),
            format!("/replay skip -{SKIP_SECONDS}"),
            "Skip back",
        ),
        button(
            &format!("+{SKIP_SECONDS}s"),
            format!("/replay skip {SKIP_SECONDS}"),
            "Skip forward",
        ),
        button("restart", "/replay restart".into(), "Back to the start"),
    ];
    for b in buttons {
        line.extra.push(TextComponent::new(" "));
        line.extra.push(b);
    }
    line
}

fn escape(text: &str) -> String {
    text.chars()
        .flat_map(|c| match c {
            '"' | '\\' => vec!['\\', c],
            c if c.is_control() => Vec::new(),
            c => vec![c],
        })
        .collect()
}

/// Status Response JSON for the server list.
pub fn status_json(player: &str, viewers: usize) -> String {
    format!(
        r#"{{"version":{{"name":"{VERSION_NAME}","protocol":{PROTOCOL_VERSION}}},"players":{{"max":0,"online":{viewers}}},"description":{{"text":"Replay of {}"}},"enforcesSecureChat":false}}"#,
        escape(player)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn tree_indices_are_in_range_and_arguments_follow_literals() {
        let nodes = command_tree();
        for node in &nodes {
            assert!(node.children.iter().all(|c| (*c as usize) < nodes.len()));
        }
        assert_eq!(nodes[1].children.len(), WORDS.len() + ARGUMENT_WORDS.len());
        assert_eq!(nodes[0].children.len(), 1 + TELEPORT_WORDS.len());
        let seek = nodes.iter().position(|n| n.name == Some("seek")).unwrap();
        assert_eq!(
            nodes[nodes[seek].children[0] as usize].name,
            Some("seconds")
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn teleport_partial_finds_the_first_argument_only() {
        assert_eq!(teleport_partial("/tp Ste"), Some((4, "Ste")));
        assert_eq!(teleport_partial("/teleport "), Some((10, "")));
        assert_eq!(teleport_partial("/tp 1 2"), None);
        assert_eq!(teleport_partial("/replay s"), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn controls_carry_run_commands() {
        let line = chat_controls("Paused".into());
        assert!(line.extra.iter().any(|c| matches!(
            c.events().and_then(|e| e.click.clone()),
            Some(ClickEvent::RunCommand(cmd)) if cmd == "/replay play"
        )));
    }

    #[tuff_core::watchdog]
    #[test]
    fn status_json_escapes_the_player_name() {
        assert!(status_json("a\"b", 2).contains(r#"Replay of a\"b"#));
    }
}
