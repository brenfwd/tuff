//! The optional audit log (`tuff.features.audit-log.enabled`): who broke and placed
//! which blocks, what went into and out of containers, what they said and
//! ran, when they came and went, kept in
//! a SQLite file beside the world whatever backend the world uses, with
//! `/tuff audit` lookups, rollbacks, restores and purges.
//!
//! Nothing here runs unless the feature is loaded. Hooks only queue
//! records; a writer thread batches them into the file, and a reader
//! thread answers lookups beside it.

#![cfg_attr(
    test,
    expect(
        clippy::disallowed_methods,
        reason = "test code is exempt from the door gate"
    )
)]

pub mod actors;
pub mod command;
pub mod container;
pub mod entity;
pub mod filter;
pub mod hooks;
pub mod panel;
pub mod record;
pub mod rollback;
pub mod service;
pub mod store;

use std::path::PathBuf;
use std::sync::Arc;

use tuff_core::config::{Key, Value};
use tuff_core::feature::{Feature, LoadContext};
use tuff_core::permission::Node;
use tuff_core::world::commands::registry::TuffSubcommand;
use tuff_core::world::session::hooks::Interceptor;
use tuff_item::load::{LoadError, Phase};

pub use service::AuditLog;

pub const ID: &str = "audit-log";

/// The feature. `database` overrides where the file goes (tests); `None`
/// uses `tuff.features.audit-log.database`, then a file beside the world.
#[derive(Default)]
pub struct AuditLogFeature {
    pub database: Option<PathBuf>,
}

impl AuditLogFeature {
    pub fn at(database: impl Into<PathBuf>) -> AuditLogFeature {
        AuditLogFeature {
            database: Some(database.into()),
        }
    }
}

fn config_keys() -> Vec<Key> {
    let mut keys = vec![
        Key::new(
            service::DATABASE_KEY,
            Value::String(String::new()),
            "Where the audit log's SQLite file is; empty puts <world>.audit.sqlite beside the world. Read at startup",
        ),
        Key::new(
            service::RETENTION_KEY,
            Value::String(service::DEFAULT_RETENTION.to_owned()),
            "How long audit log rows are kept, like 90d or 2w; 0 keeps them for ever",
        )
        .validated(|value| match value {
            Value::String(text) if text.trim().is_empty() || text.trim() == "0" => Ok(()),
            Value::String(text) => filter::parse_duration(text.trim()).map(|_| ()),
            _ => Err("must be a duration like 90d".to_owned()),
        }),
    ];
    for group in record::Group::ALL {
        keys.push(
            Key::new(
                &service::group_retention_key(group),
                Value::String(String::new()),
                &format!(
                    "How long audit log {} rows are kept, like 30d; empty follows the general setting, 0 keeps them for ever",
                    group.name()
                ),
            )
            .validated(|value| match value {
                Value::String(text) if text.trim().is_empty() || text.trim() == "0" => Ok(()),
                Value::String(text) => filter::parse_duration(text.trim()).map(|_| ()),
                _ => Err("must be a duration like 30d".to_owned()),
            }),
        );
        keys.push(Key::new(
            &service::group_key(group),
            Value::Bool(true),
            &format!("Whether the audit log records {} actions", group.name()),
        ));
    }
    keys
}

fn permission_nodes() -> [Node; 6] {
    [
        Node::new(
            panel::READ_NODE,
            "Read audit log lookups in the admin panel API",
        ),
        Node::new(
            command::LOOKUP_NODE,
            "Search the audit log with /tuff audit lookup",
        ),
        Node::new(
            command::ROLLBACK_NODE,
            "Roll back logged block changes with /tuff audit rollback, and cancel one",
        ),
        Node::new(
            command::RESTORE_NODE,
            "Restore rolled back changes with /tuff audit restore",
        ),
        Node::new(
            command::PURGE_NODE,
            "Delete old audit log rows with /tuff audit purge",
        ),
        Node::new(
            command::STATUS_NODE,
            "Show the audit log's file, size and running job with /tuff audit status",
        ),
    ]
}

impl Feature for AuditLogFeature {
    fn id(&self) -> &'static str {
        ID
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() != Phase::Content {
            return Ok(());
        }
        let registries = cx.registries();
        registries.config_keys.extend(config_keys());
        for node in permission_nodes() {
            registries.permissions.register(ID, node)?;
        }
        registries.services.push(Arc::new(service::AuditLogFactory {
            database: self.database.clone(),
        }));
        registries.region_hooks.push(Arc::new(hooks::BlockHooks));
        registries
            .region_hooks
            .push(Arc::new(hooks::ContainerHooks));
        registries.region_hooks.push(Arc::new(hooks::ItemHooks));
        registries.session_hooks.push(Arc::new(hooks::Sessions));
        registries.packet_interceptors.push(Interceptor {
            order: hooks::CHAT_ORDER,
            hook: Arc::new(hooks::ChatLog),
        });
        registries.packet_interceptors.push(Interceptor {
            order: hooks::INSPECT_ORDER,
            hook: Arc::new(hooks::Inspector),
        });
        panel::register(registries)?;
        registries.commands.add_tuff_subcommand(TuffSubcommand {
            node: &command::AUDIT,
            providers: command::PROVIDERS,
            executor: Arc::new(command::AuditCommand),
        });
        Ok(())
    }
}
