use tuff_vanilla_data::packet::play::Clientbound as Id;

use crate::protocol::Encode;
use crate::protocol::codec::WriteExt;
use crate::world::scoreboard::{NumberFormat, RenderType, Team};
use tuff_item::text::TextComponent;
use tuff_wire::PacketWriter;

fn write_number_format(w: &mut PacketWriter, format: Option<&NumberFormat>) {
    w.write_bool(format.is_some());
    match format {
        None => {}
        Some(NumberFormat::Blank) => w.write_var_int(0),
        Some(NumberFormat::Styled(style)) => {
            w.write_var_int(1);
            w.write_nbt(&tuff_nbt::Tag::Compound(style.clone()))
                .expect("styles are parsed within the depth limit");
        }
        Some(NumberFormat::Fixed(text)) => {
            w.write_var_int(2);
            w.write_text(text);
        }
    }
}

/// Update Objectives: `method` 0 adds, 1 removes, 2 changes.
pub struct SetObjective<'a> {
    pub name: &'a str,
    pub method: u8,
    pub display_name: &'a TextComponent,
    pub render_type: RenderType,
    pub number_format: Option<&'a NumberFormat>,
}

impl Encode for SetObjective<'_> {
    const ID: i32 = Id::SetObjective.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_string(self.name);
        w.write_byte(self.method as i8);
        if self.method != 1 {
            w.write_text(self.display_name);
            w.write_var_int(self.render_type as i32);
            write_number_format(w, self.number_format);
        }
    }
}

/// Display Objective; an empty name clears the slot.
pub struct SetDisplayObjective<'a> {
    pub slot: i32,
    pub name: &'a str,
}

impl Encode for SetDisplayObjective<'_> {
    const ID: i32 = Id::SetDisplayObjective.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.slot);
        w.write_string(self.name);
    }
}

/// Update Score.
pub struct SetScore<'a> {
    pub holder: &'a str,
    pub objective: &'a str,
    pub value: i32,
    pub display: Option<&'a TextComponent>,
    pub number_format: Option<&'a NumberFormat>,
}

impl Encode for SetScore<'_> {
    const ID: i32 = Id::SetScore.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_string(self.holder);
        w.write_string(self.objective);
        w.write_var_int(self.value);
        w.write_bool(self.display.is_some());
        if let Some(display) = self.display {
            w.write_text(display);
        }
        write_number_format(w, self.number_format);
    }
}

/// Reset Score: one objective, or every objective when `None`.
pub struct ResetScore<'a> {
    pub holder: &'a str,
    pub objective: Option<&'a str>,
}

impl Encode for ResetScore<'_> {
    const ID: i32 = Id::ResetScore.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_string(self.holder);
        w.write_bool(self.objective.is_some());
        if let Some(objective) = self.objective {
            w.write_string(objective);
        }
    }
}

/// What an Update Teams packet does.
pub enum TeamAction<'a> {
    Add(&'a Team),
    Remove,
    Change(&'a Team),
    Join(&'a [String]),
    Leave(&'a [String]),
}

pub struct SetPlayerTeam<'a> {
    pub name: &'a str,
    pub action: TeamAction<'a>,
}

fn write_team_parameters(w: &mut PacketWriter, team: &Team) {
    w.write_text(&team.display_name);
    w.write_text(&team.prefix);
    w.write_text(&team.suffix);
    w.write_var_int(team.name_tag_visibility as i32);
    w.write_var_int(team.collision_rule as i32);
    w.write_bool(team.color.is_some());
    if let Some(color) = team.color {
        w.write_var_int(color as i32);
    }
    w.write_byte(team.options() as i8);
}

fn write_names<'a>(w: &mut PacketWriter, names: impl ExactSizeIterator<Item = &'a String>) {
    w.write_var_int(names.len() as i32);
    for name in names {
        w.write_string(name);
    }
}

impl Encode for SetPlayerTeam<'_> {
    const ID: i32 = Id::SetPlayerTeam.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_string(self.name);
        match &self.action {
            TeamAction::Add(team) => {
                w.write_byte(0);
                write_team_parameters(w, team);
                write_names(w, team.members.iter());
            }
            TeamAction::Remove => w.write_byte(1),
            TeamAction::Change(team) => {
                w.write_byte(2);
                write_team_parameters(w, team);
            }
            TeamAction::Join(names) => {
                w.write_byte(3);
                write_names(w, names.iter());
            }
            TeamAction::Leave(names) => {
                w.write_byte(4);
                write_names(w, names.iter());
            }
        }
    }
}
