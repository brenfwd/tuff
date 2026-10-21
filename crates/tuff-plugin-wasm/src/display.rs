//! Titles, action bars, a shared sidebar and scoreboard teams a plugin
//! queued. A plugin's sidebar objective and teams are named after its id,
//! and go away when it unloads.

use std::sync::Arc;

use tuff_core::world::Shared;
use tuff_core::world::commands::text;
use tuff_core::world::messages::{CoordinatorMessage, TitleAction};
use tuff_core::world::scoreboard::{
    Criterion, DisplaySlot, NumberFormat, Objective, RenderType, Scoreboard, Team, TeamColor,
};

use crate::sandbox::DisplayOp;

fn objective_name(plugin: &str) -> String {
    format!("{plugin}.sidebar")
}

fn team_name(plugin: &str, name: &str) -> String {
    format!("{plugin}.{name}")
}

fn line_holder(index: usize) -> String {
    format!("line{index}")
}

/// Carries out the ops a call queued, in order.
pub async fn apply(shared: &Arc<Shared>, plugin: &str, ops: Vec<DisplayOp>) {
    for op in ops {
        match op {
            DisplayOp::Title {
                player,
                title,
                subtitle,
                times: [fade_in, stay, fade_out],
            } => {
                let clamp = |ticks: u32| i32::try_from(ticks).unwrap_or(i32::MAX);
                for action in [
                    TitleAction::Times {
                        fade_in: clamp(fade_in),
                        stay: clamp(stay),
                        fade_out: clamp(fade_out),
                    },
                    TitleAction::Subtitle(text(subtitle)),
                    TitleAction::Title(text(title)),
                ] {
                    title_action(shared, player, action).await;
                }
            }
            DisplayOp::ClearTitle(player) => {
                title_action(shared, player, TitleAction::Reset).await;
            }
            DisplayOp::Payload {
                player,
                channel,
                data,
            } => {
                let _ = shared
                    .coordinator
                    .send(CoordinatorMessage::PluginDirective {
                        uuid: player,
                        directive: Box::new(tuff_core::world::messages::Directive::Payload {
                            channel,
                            data,
                        }),
                    })
                    .await;
            }
            DisplayOp::ActionBar(player, message) => {
                title_action(shared, player, TitleAction::ActionBar(text(message))).await;
            }
            DisplayOp::Sidebar(sidebar) => {
                let name = objective_name(plugin);
                shared
                    .scoreboard
                    .apply(move |board| match sidebar {
                        Some((title, lines)) => set_sidebar(board, &name, &title, &lines),
                        None => board.remove_objective(&name),
                    })
                    .await;
            }
            DisplayOp::Team {
                name,
                color,
                prefix,
                suffix,
                members,
            } => {
                let name = team_name(plugin, &name);
                shared
                    .scoreboard
                    .apply(move |board| set_team(board, &name, color, &prefix, &suffix, &members))
                    .await;
            }
            DisplayOp::TeamRemove(name) => {
                let name = team_name(plugin, &name);
                shared
                    .scoreboard
                    .apply(move |board| board.remove_team(&name))
                    .await;
            }
        }
    }
}

/// Removes the sidebar and every team a plugin made.
pub async fn remove_all(shared: &Arc<Shared>, plugin: &str) {
    let objective = objective_name(plugin);
    let prefix = format!("{plugin}.");
    shared
        .scoreboard
        .apply(move |board| {
            board.remove_objective(&objective);
            let owned: Vec<String> = board
                .teams()
                .filter(|team| team.name.starts_with(&prefix))
                .map(|team| team.name.clone())
                .collect();
            for name in owned {
                board.remove_team(&name);
            }
        })
        .await;
}

async fn title_action(shared: &Arc<Shared>, uuid: u128, action: TitleAction) {
    let _ = shared
        .coordinator
        .send(CoordinatorMessage::PluginTitle { uuid, action })
        .await;
}

fn set_sidebar(board: &mut Scoreboard, name: &str, title: &str, lines: &[String]) {
    let display_name = text(title);
    if board.objective(name).is_some() {
        board.modify_objective(name, |objective| objective.display_name = display_name);
    } else {
        board.add_objective(Objective {
            name: name.to_owned(),
            criterion: Criterion::Dummy,
            display_name,
            render_type: RenderType::Integer,
            display_auto_update: false,
            number_format: Some(NumberFormat::Blank),
        });
    }
    let stale: Vec<String> = board
        .holders_in(name)
        .into_iter()
        .filter(|holder| {
            holder
                .strip_prefix("line")
                .and_then(|index| index.parse::<usize>().ok())
                .is_none_or(|index| index >= lines.len())
        })
        .collect();
    for holder in stale {
        board.reset(&holder, name);
    }
    for (index, line) in lines.iter().enumerate() {
        let holder = line_holder(index);
        board.get_or_create(&holder, name);
        board.set(
            &tuff_core::world::scoreboard::Holder::named(holder.clone()),
            name,
            i32::try_from(lines.len() - index).unwrap_or(0),
        );
        board.set_score_display(&holder, name, Some(text(line)));
        board.set_score_format(&holder, name, Some(NumberFormat::Blank));
    }
    board.set_display(DisplaySlot::Sidebar, Some(name));
}

fn set_team(
    board: &mut Scoreboard,
    name: &str,
    color: Option<String>,
    prefix: &str,
    suffix: &str,
    members: &[String],
) {
    let color = color.as_deref().and_then(TeamColor::from_name);
    if board.team(name).is_none() {
        board.add_team(Team::new(name));
    }
    let (prefix, suffix) = (text(prefix), text(suffix));
    board.modify_team(name, |team| {
        team.color = color;
        team.prefix = prefix;
        team.suffix = suffix;
    });
    let gone: Vec<String> = board
        .team(name)
        .map(|team| {
            team.members
                .iter()
                .filter(|member| !members.contains(member))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    for member in gone {
        board.leave_team(&member);
    }
    for member in members {
        board.join_team(member, name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn sidebar_updates_in_place_and_drops_stale_lines() {
        let mut board = Scoreboard::default();
        set_sidebar(&mut board, "p.sidebar", "Title", &lines(&["a", "b", "c"]));
        assert_eq!(
            board
                .display_objective(DisplaySlot::Sidebar)
                .map(|o| o.name.as_str()),
            Some("p.sidebar")
        );
        assert_eq!(board.score("line0", "p.sidebar").unwrap().value, 3);
        assert_eq!(board.score("line2", "p.sidebar").unwrap().value, 1);
        set_sidebar(&mut board, "p.sidebar", "T2", &lines(&["x"]));
        assert!(board.score("line1", "p.sidebar").is_none());
        assert_eq!(
            board.score("line0", "p.sidebar").unwrap().display,
            Some(text("x"))
        );
        board.remove_objective("p.sidebar");
        assert!(board.display_objective(DisplaySlot::Sidebar).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn team_set_syncs_members_and_color() {
        let mut board = Scoreboard::default();
        let names = lines(&["Alex", "Steve"]);
        set_team(&mut board, "p.red", Some("red".into()), "[R] ", "", &names);
        let team = board.team("p.red").unwrap();
        assert_eq!(team.color, Some(TeamColor::Red));
        assert_eq!(team.members.len(), 2);
        set_team(&mut board, "p.red", None, "", "", &lines(&["Steve"]));
        assert_eq!(board.team("p.red").unwrap().members.len(), 1);
        assert!(board.team_of("Alex").is_none());
        assert_eq!(board.team("p.red").unwrap().color, None);
    }
}
