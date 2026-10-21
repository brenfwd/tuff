//! The core side of the Permissions page: the permission store as the
//! page lists and changes it.

use hyper::StatusCode;
use serde_json::{Map, Value, json};

use crate::permission::{Change, Grants, SetError, View, valid_pattern};
use crate::world::Shared;
use tuff_storage::admin::{format_uuid, parse_uuid_text};
use tuff_web_ui::host::{Edit, HostError, HostFuture, PanelHost};
use tuff_web_ui::permissions::PermissionHost;

fn nodes_json(nodes: &std::collections::BTreeMap<String, bool>) -> Map<String, Value> {
    nodes
        .iter()
        .map(|(pattern, held)| (pattern.clone(), Value::Bool(*held)))
        .collect()
}

/// The page's whole state.
pub fn state_json(view: &View) -> Value {
    let grants = view.grants();
    let nodes: Vec<Value> = view
        .nodes()
        .iter()
        .map(|node| {
            json!({
                "id": node.id,
                "description": node.description,
                "default": node.default,
                "op_bypass": node.op_bypass,
                "op_level": node.op_level.id(),
            })
        })
        .collect();
    let mut players: Vec<(&u128, &crate::permission::PlayerGrants)> =
        grants.players.iter().collect();
    players.sort_by_key(|a| a.1.name.to_lowercase());
    let players: Vec<Value> = players
        .into_iter()
        .map(|(uuid, player)| {
            json!({
                "uuid": format_uuid(*uuid),
                "name": player.name,
                "roles": player.roles,
                "nodes": nodes_json(&player.nodes),
            })
        })
        .collect();
    let mut roles: Vec<(&String, &crate::permission::Role)> = grants.roles.iter().collect();
    roles.sort_by(|a, b| b.1.priority.cmp(&a.1.priority).then(a.0.cmp(b.0)));
    let roles: Vec<Value> = roles
        .into_iter()
        .map(|(name, role)| {
            let mut members: Vec<&str> = grants
                .players
                .values()
                .filter(|p| p.roles.contains(name))
                .map(|p| p.name.as_str())
                .collect();
            members.sort_unstable();
            let mut attributes = serde_json::Map::new();
            for attribute in crate::permission::Attribute::ALL {
                if let Some(value) = role.attributes.get(attribute) {
                    attributes.insert(attribute.key().into(), value.into());
                }
            }
            json!({
                "name": name,
                "priority": role.priority,
                "members": members,
                "nodes": nodes_json(&role.nodes),
                "attributes": attributes,
            })
        })
        .collect();
    json!({
        "nodes": nodes,
        "players": players,
        "roles": roles,
        "default_role": crate::permission::DEFAULT_ROLE,
    })
}

/// A change request the panel sends, before its player is resolved.
#[derive(Debug, PartialEq, Eq)]
pub enum ChangeRequest {
    Player {
        player: String,
        node: String,
        held: Option<bool>,
    },
    Role {
        role: String,
        node: String,
        held: Option<bool>,
    },
    CreateRole {
        role: String,
        priority: i32,
    },
    DeleteRole {
        role: String,
    },
    Priority {
        role: String,
        priority: i32,
    },
    Member {
        player: String,
        role: String,
        member: bool,
    },
    Attribute {
        role: String,
        attribute: crate::permission::Attribute,
        value: Option<String>,
    },
}

fn text(body: &Value, key: &str) -> Result<String, &'static str> {
    body.get(key)
        .and_then(Value::as_str)
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .ok_or(match key {
            "player" => "player is required",
            "role" => "role is required",
            _ => "node is required",
        })
}

fn held(body: &Value) -> Result<Option<bool>, &'static str> {
    match body.get("held") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(held)) => Ok(Some(*held)),
        Some(_) => Err("held is true, false or null"),
    }
}

fn priority(body: &Value) -> Result<i32, &'static str> {
    match body.get("priority") {
        None | Some(Value::Null) => Ok(0),
        Some(value) => value
            .as_i64()
            .and_then(|p| i32::try_from(p).ok())
            .ok_or("priority is a whole number"),
    }
}

/// Reads a change body.
pub fn parse_change(body: &Value) -> Result<ChangeRequest, &'static str> {
    let kind = body.get("kind").and_then(Value::as_str).unwrap_or_default();
    Ok(match kind {
        "player" => ChangeRequest::Player {
            player: text(body, "player")?,
            node: text(body, "node")?,
            held: held(body)?,
        },
        "role" => ChangeRequest::Role {
            role: text(body, "role")?,
            node: text(body, "node")?,
            held: held(body)?,
        },
        "create_role" => ChangeRequest::CreateRole {
            role: text(body, "role")?,
            priority: priority(body)?,
        },
        "delete_role" => ChangeRequest::DeleteRole {
            role: text(body, "role")?,
        },
        "priority" => ChangeRequest::Priority {
            role: text(body, "role")?,
            priority: priority(body)?,
        },
        "member" => ChangeRequest::Member {
            player: text(body, "player")?,
            role: text(body, "role")?,
            member: body
                .get("member")
                .and_then(Value::as_bool)
                .ok_or("member is true or false")?,
        },
        "attribute" => ChangeRequest::Attribute {
            role: text(body, "role")?,
            attribute: body
                .get("attribute")
                .and_then(Value::as_str)
                .and_then(crate::permission::Attribute::from_key)
                .ok_or("attribute is prefix, suffix, name_color or display_name")?,
            value: match body.get("value") {
                None | Some(Value::Null) => None,
                Some(Value::String(v)) => Some(v.clone()),
                Some(_) => return Err("value is a string or null"),
            },
        },
        _ => return Err("unknown change kind"),
    })
}

/// A player named by uuid, by a name already in the settings, or by an
/// online player's name.
async fn resolve(shared: &Shared, grants: &Grants, player: &str) -> Option<(u128, String)> {
    if let Some(uuid) = parse_uuid_text(player) {
        let name = grants
            .players
            .get(&uuid)
            .map(|p| p.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format_uuid(uuid));
        return Some((uuid, name));
    }
    if let Some((uuid, p)) = grants
        .players
        .iter()
        .find(|(_, p)| p.name.eq_ignore_ascii_case(player))
    {
        return Some((*uuid, p.name.clone()));
    }
    shared
        .online_players()
        .await
        .into_iter()
        .find(|(_, name)| name.eq_ignore_ascii_case(player))
}

fn set_error(err: &SetError) -> HostError {
    let status = match err {
        SetError::Stopped => StatusCode::SERVICE_UNAVAILABLE,
        SetError::Save(_) => StatusCode::INTERNAL_SERVER_ERROR,
        SetError::UnknownRole => StatusCode::NOT_FOUND,
        SetError::RoleExists => StatusCode::CONFLICT,
        _ => StatusCode::BAD_REQUEST,
    };
    HostError::new(status, err.to_string())
}

fn refused(status: StatusCode, message: &str) -> (&'static str, Edit<bool>) {
    (
        "",
        Edit {
            target: String::new(),
            detail: None,
            result: Err(HostError::new(status, message).quiet_if(true)),
        },
    )
}

/// Applies a change body, answering the audit action and the edit:
/// `Ok(false)` when the settings already were that way.
pub async fn change(shared: &Shared, body: &Value) -> (&'static str, Edit<bool>) {
    let request = match parse_change(body) {
        Ok(request) => request,
        Err(text) => return refused(StatusCode::BAD_REQUEST, text),
    };
    let view = shared.permissions.view();
    let player = match &request {
        ChangeRequest::Player { player, .. } | ChangeRequest::Member { player, .. } => {
            match resolve(shared, view.grants(), player).await {
                Some(found) => Some(found),
                None => {
                    return refused(
                        StatusCode::NOT_FOUND,
                        "no such player: give a uuid or an online player's name",
                    );
                }
            }
        }
        _ => None,
    };
    let (uuid, name) = player.unwrap_or_default();
    if let ChangeRequest::Player { node, .. } | ChangeRequest::Role { node, .. } = &request
        && !valid_pattern(node)
    {
        return refused(StatusCode::BAD_REQUEST, "not a permission node or pattern");
    }
    let (action, target, detail, change) = match request {
        ChangeRequest::Player { node, held, .. } => {
            if held.is_some() && !node.ends_with('*') && !view.nodes().contains(&node) {
                return refused(StatusCode::BAD_REQUEST, "unknown permission node");
            }
            (
                "permissions.player",
                name.clone(),
                format!("{node} {}", state(held)),
                Change::Player {
                    uuid,
                    name: name.clone(),
                    pattern: node,
                    held,
                },
            )
        }
        ChangeRequest::Role { role, node, held } => {
            if held.is_some() && !node.ends_with('*') && !view.nodes().contains(&node) {
                return refused(StatusCode::BAD_REQUEST, "unknown permission node");
            }
            (
                "permissions.role",
                role.clone(),
                format!("{node} {}", state(held)),
                Change::Role {
                    role,
                    pattern: node,
                    held,
                },
            )
        }
        ChangeRequest::CreateRole { role, priority } => (
            "permissions.role.create",
            role.clone(),
            format!("priority {priority}"),
            Change::CreateRole { role, priority },
        ),
        ChangeRequest::DeleteRole { role } => (
            "permissions.role.delete",
            role.clone(),
            String::new(),
            Change::DeleteRole { role },
        ),
        ChangeRequest::Priority { role, priority } => (
            "permissions.role.priority",
            role.clone(),
            format!("priority {priority}"),
            Change::Priority { role, priority },
        ),
        ChangeRequest::Attribute {
            role,
            attribute,
            value,
        } => (
            "permissions.role.attribute",
            role.clone(),
            format!(
                "{} {}",
                attribute.key(),
                value.as_deref().unwrap_or("cleared")
            ),
            Change::Attribute {
                role,
                attribute,
                value,
            },
        ),
        ChangeRequest::Member { role, member, .. } => (
            "permissions.member",
            name.clone(),
            format!("{} {role}", if member { "add to" } else { "remove from" }),
            Change::Member {
                uuid,
                name: name.clone(),
                role,
                member,
            },
        ),
    };
    let result = super::apply_permissions(shared, change)
        .await
        .map_err(|err| set_error(&err));
    (
        action,
        Edit {
            target,
            detail: Some(detail),
            result,
        },
    )
}

fn state(held: Option<bool>) -> &'static str {
    match held {
        Some(true) => "granted",
        Some(false) => "denied",
        None => "cleared",
    }
}

impl PermissionHost for Shared {
    fn state(&self) -> Value {
        state_json(&self.permissions.view())
    }

    fn change<'a>(&'a self, body: &'a Value) -> HostFuture<'a, (&'static str, Edit<bool>)> {
        Box::pin(change(self, body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::{NodeRegistry, PlayerGrants};
    use std::sync::Arc;

    #[tuff_core::watchdog]
    #[test]
    fn change_bodies_parse() {
        assert_eq!(
            parse_change(&json!({"kind":"player","player":"Alex","node":"a.b","held":true})),
            Ok(ChangeRequest::Player {
                player: "Alex".into(),
                node: "a.b".into(),
                held: Some(true)
            })
        );
        assert_eq!(
            parse_change(&json!({"kind":"role","role":"mods","node":"a.*","held":null})),
            Ok(ChangeRequest::Role {
                role: "mods".into(),
                node: "a.*".into(),
                held: None
            })
        );
        assert_eq!(
            parse_change(&json!({"kind":"create_role","role":"mods"})),
            Ok(ChangeRequest::CreateRole {
                role: "mods".into(),
                priority: 0
            })
        );
        assert_eq!(
            parse_change(&json!({"kind":"member","player":"Alex","role":"mods","member":false})),
            Ok(ChangeRequest::Member {
                player: "Alex".into(),
                role: "mods".into(),
                member: false
            })
        );
        assert_eq!(
            parse_change(
                &json!({"kind":"attribute","role":"mods","attribute":"prefix","value":"<red>M "})
            ),
            Ok(ChangeRequest::Attribute {
                role: "mods".into(),
                attribute: crate::permission::Attribute::Prefix,
                value: Some("<red>M ".into())
            })
        );
        assert_eq!(
            parse_change(
                &json!({"kind":"attribute","role":"mods","attribute":"name_color","value":null})
            ),
            Ok(ChangeRequest::Attribute {
                role: "mods".into(),
                attribute: crate::permission::Attribute::NameColor,
                value: None
            })
        );
        for bad in [
            json!({"kind":"attribute","role":"g","attribute":"colour","value":"x"}),
            json!({"kind":"attribute","role":"g","attribute":"prefix","value":3}),
            json!({}),
            json!({"kind":"player","node":"a.b"}),
            json!({"kind":"player","player":"Alex","node":"a.b","held":"yes"}),
            json!({"kind":"priority","role":"g","priority":1.5}),
            json!({"kind":"member","player":"Alex","role":"g"}),
            json!({"kind":"role","role":"g","node":"  "}),
        ] {
            assert!(parse_change(&bad).is_err(), "{bad}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn state_lists_nodes_players_and_roles() {
        let mut grants = Grants::default();
        let mut player = PlayerGrants {
            name: "Alex".into(),
            ..Default::default()
        };
        player.nodes.insert("tuff.web.access".into(), true);
        player.roles.insert("mods".into());
        grants.players.insert(0x12, player);
        grants.roles.insert(
            "mods".into(),
            crate::permission::Role {
                priority: 2,
                ..Default::default()
            },
        );
        let view = View::new(NodeRegistry::new().nodes(), Arc::new(grants));
        let state = state_json(&view);
        assert!(
            state["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["id"] == "tuff.web.permissions" && n["op_level"] == 4)
        );
        assert_eq!(state["players"][0]["name"], "Alex");
        assert_eq!(state["players"][0]["nodes"]["tuff.web.access"], true);
        assert_eq!(state["players"][0]["roles"], json!(["mods"]));
        assert_eq!(state["roles"][0]["members"], json!(["Alex"]));
        assert_eq!(state["roles"][0]["priority"], 2);
    }
}
