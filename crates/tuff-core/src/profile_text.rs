//! Chat text built from a resolved player profile.

use tuff_item::text::{ClickEvent, TextComponent};

use crate::auth::GameProfile;

fn snbt_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        if matches!(c, '\\' | '"') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// A resolved profile in the compound form the profile component takes.
pub fn profile_snbt(profile: &GameProfile) -> String {
    let id = profile.id;
    let mut out = format!(
        "{{id:[I;{},{},{},{}],name:{}",
        (id >> 96) as u32 as i32,
        (id >> 64) as u32 as i32,
        (id >> 32) as u32 as i32,
        id as u32 as i32,
        snbt_string(&profile.name)
    );
    if !profile.properties.is_empty() {
        out.push_str(",properties:[");
        for (i, property) in profile.properties.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{name:{},value:{}",
                snbt_string(&property.name),
                snbt_string(&property.value)
            ));
            if let Some(signature) = &property.signature {
                out.push_str(&format!(",signature:{}", snbt_string(signature)));
            }
            out.push('}');
        }
        out.push(']');
    }
    out.push('}');
    out
}

/// The chat line a creative operator gets for ctrl-picking a player: the
/// profile with copy, give-head and summon-mannequin actions.
pub fn fetched_profile_message(profile: &GameProfile) -> TextComponent {
    profile_report(
        "commands.fetchprofile.entity.success",
        TextComponent::new(profile.name.clone()),
        profile,
    )
}

/// The translation `message` with `argument` and the profile actions.
pub fn profile_report(
    message: &str,
    argument: TextComponent,
    profile: &GameProfile,
) -> TextComponent {
    let snbt = profile_snbt(profile);
    let action = |key: &str, event: ClickEvent| TextComponent::translate(key, []).click(event);
    let actions = [
        action(
            "commands.fetchprofile.copy_component",
            ClickEvent::CopyToClipboard(snbt.clone()),
        ),
        action(
            "commands.fetchprofile.give_item",
            ClickEvent::RunCommand(format!("give @s minecraft:player_head[profile={snbt}]")),
        ),
        action(
            "commands.fetchprofile.summon_mannequin",
            ClickEvent::RunCommand(format!(
                "summon minecraft:mannequin ~ ~ ~ {{profile:{snbt}}}"
            )),
        ),
    ];
    let mut clickable = TextComponent::new("");
    for (i, part) in actions.into_iter().enumerate() {
        if i > 0 {
            clickable.extra.push(TextComponent::new(" "));
        }
        clickable.extra.push(TextComponent::translate(
            "chat.square_brackets",
            [part.color("green")],
        ));
    }
    TextComponent::translate(message, [argument, clickable])
}

#[cfg(test)]
#[path = "profile_text_tests.rs"]
mod tests;
