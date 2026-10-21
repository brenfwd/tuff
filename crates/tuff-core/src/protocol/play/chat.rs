use tuff_vanilla_data::packet::play::Clientbound as Id;

use crate::protocol::codec::WriteExt;
use crate::protocol::{Encode, Packet};
use tuff_item::item::ItemView;
use tuff_item::text::TextComponent;
use tuff_wire::PacketWriter;

type Progress = [(tuff_vanilla_data::Frozen<str>, Option<i64>)];

pub mod command_node {
    pub const ROOT: u8 = 0;
    pub const LITERAL: u8 = 1;
    pub const ARGUMENT: u8 = 2;
    pub const EXECUTABLE: u8 = 0x04;
    pub const REDIRECT: u8 = 0x08;
    pub const SUGGESTIONS: u8 = 0x10;
    pub const RESTRICTED: u8 = 0x20;
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandNodeData {
    /// Node type and flags from [`command_node`].
    pub flags: u8,
    pub children: Vec<i32>,
    pub redirect: Option<i32>,
    pub name: Option<&'static str>,
    /// Argument parser ID and its encoded properties.
    pub parser: Option<(i32, Vec<u8>)>,
    /// Suggestion provider of an argument, e.g. `minecraft:ask_server`.
    pub suggestions: Option<&'static str>,
}

pub struct Commands<'a> {
    pub nodes: &'a [CommandNodeData],
    pub root: i32,
}

impl Encode for Commands<'_> {
    const ID: i32 = Id::Commands.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.nodes.len() as i32);
        for node in self.nodes {
            let redirect = if node.redirect.is_some() {
                command_node::REDIRECT
            } else {
                0
            };
            let suggestions = if node.suggestions.is_some() {
                command_node::SUGGESTIONS
            } else {
                0
            };
            w.write_unsigned_byte(node.flags | redirect | suggestions);
            w.write_var_int(node.children.len() as i32);
            for child in &node.children {
                w.write_var_int(*child);
            }
            if let Some(redirect) = node.redirect {
                w.write_var_int(redirect);
            }
            if let Some(name) = node.name {
                w.write_string(name);
            }
            if let Some((parser, properties)) = &node.parser {
                w.write_var_int(*parser);
                w.write_bytes(properties);
            }
            if let Some(suggestions) = node.suggestions {
                w.write_string(suggestions);
            }
        }
        w.write_var_int(self.root);
    }
}

#[derive(Packet)]
#[packet(id = Id::Disconnect, encode_only)]
pub struct Disconnect<'a>(pub &'a TextComponent);

/// One advancement as Update Advancements adds it.
pub struct AdvancementEntry<'a> {
    pub id: &'a str,
    pub parent: Option<&'a str>,
    pub display: Option<AdvancementDisplay<'a>>,
    pub requirements: &'a [Box<[tuff_vanilla_data::Frozen<str>]>],
    pub sends_telemetry_event: bool,
    pub position: [f32; 2],
}

pub struct AdvancementDisplay<'a> {
    pub title: &'a TextComponent,
    pub description: &'a TextComponent,
    pub icon: ItemView<'a>,
    pub frame: u8,
    pub background: Option<&'a str>,
    pub show_toast: bool,
    pub hidden: bool,
}

/// Update Advancements: advancements added to and removed from the
/// client's tree, and progress (criterion name, time met in epoch
/// milliseconds) per advancement.
pub struct UpdateAdvancements<'a> {
    pub reset: bool,
    pub added: Vec<AdvancementEntry<'a>>,
    pub removed: Vec<&'a str>,
    pub progress: Vec<(&'a str, &'a Progress)>,
    pub show_advancements: bool,
}

impl Encode for UpdateAdvancements<'_> {
    const ID: i32 = Id::UpdateAdvancements.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_bool(self.reset);
        w.write_var_int(self.added.len() as i32);
        for entry in &self.added {
            w.write_string(entry.id);
            w.write_bool(entry.parent.is_some());
            if let Some(parent) = entry.parent {
                w.write_string(parent);
            }
            w.write_bool(entry.display.is_some());
            if let Some(display) = &entry.display {
                w.write_text(display.title);
                w.write_text(display.description);
                w.write_item_template(display.icon);
                w.write_var_int(i32::from(display.frame));
                let mut flags = 0;
                if display.background.is_some() {
                    flags |= 1;
                }
                if display.show_toast {
                    flags |= 2;
                }
                if display.hidden {
                    flags |= 4;
                }
                w.write_int(flags);
                if let Some(background) = display.background {
                    w.write_string(background);
                }
            }
            w.write_var_int(entry.requirements.len() as i32);
            for group in entry.requirements {
                w.write_var_int(group.len() as i32);
                for criterion in group.iter() {
                    w.write_string(criterion);
                }
            }
            w.write_bool(entry.sends_telemetry_event);
            w.write_float(entry.position[0]);
            w.write_float(entry.position[1]);
        }
        w.write_var_int(self.removed.len() as i32);
        for id in &self.removed {
            w.write_string(id);
        }
        w.write_var_int(self.progress.len() as i32);
        for (id, criteria) in &self.progress {
            w.write_string(id);
            w.write_var_int(criteria.len() as i32);
            for (name, time) in criteria.iter() {
                w.write_string(name);
                w.write_bool(time.is_some());
                if let Some(time) = time {
                    w.write_long(*time);
                }
            }
        }
        w.write_bool(self.show_advancements);
    }
}

/// Select Advancements Tab: the tab the client should show, if any.
pub struct SelectAdvancementsTab<'a>(pub Option<&'a str>);

impl Encode for SelectAdvancementsTab<'_> {
    const ID: i32 = Id::SelectAdvancementsTab.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_bool(self.0.is_some());
        if let Some(tab) = self.0 {
            w.write_string(tab);
        }
    }
}

#[derive(Packet)]
#[packet(id = Id::SystemChat, encode_only)]
pub struct SystemChat<'a> {
    pub content: &'a TextComponent,
    pub overlay: bool,
}

/// Command Suggestions Response.
pub struct CommandSuggestions<'a> {
    pub id: i32,
    pub start: i32,
    pub length: i32,
    pub matches: &'a [String],
    /// Per match, in order; may be shorter than `matches`.
    pub tooltips: &'a [Option<TextComponent>],
}

impl Encode for CommandSuggestions<'_> {
    const ID: i32 = Id::CommandSuggestions.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.id);
        w.write_var_int(self.start);
        w.write_var_int(self.length);
        w.write_var_int(self.matches.len() as i32);
        for (i, suggestion) in self.matches.iter().enumerate() {
            w.write_string(suggestion);
            match self.tooltips.get(i).and_then(Option::as_ref) {
                Some(tooltip) => {
                    w.write_bool(true);
                    w.write_text(tooltip);
                }
                None => w.write_bool(false),
            }
        }
    }
}

/// Disguised Chat Message: an unsigned chat line formatted client-side with
/// the given `minecraft:chat_type` registry entry.
pub struct DisguisedChat<'a> {
    pub message: &'a TextComponent,
    pub chat_type: i32,
    pub sender: &'a TextComponent,
    pub target: Option<&'a TextComponent>,
}

impl Encode for DisguisedChat<'_> {
    const ID: i32 = Id::DisguisedChat.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_text(self.message);
        w.write_var_int(self.chat_type + 1);
        w.write_text(self.sender);
        w.write_bool(self.target.is_some());
        if let Some(target) = self.target {
            w.write_text(target);
        }
    }
}

/// Server Data: the MOTD and optional icon, sent once on join.
pub struct ServerData<'a> {
    pub motd: &'a TextComponent,
    pub icon: Option<&'a [u8]>,
}

impl Encode for ServerData<'_> {
    const ID: i32 = Id::ServerData.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_text(self.motd);
        w.write_bool(self.icon.is_some());
        if let Some(icon) = self.icon {
            w.write_var_int(icon.len() as i32);
            w.write_bytes(icon);
        }
    }
}
