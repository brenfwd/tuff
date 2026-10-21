//! `/datapack` and `/reload`: listing, enabling and disabling the world's
//! data packs, creating an empty one, and reloading them.

use tuff_core::command::CommandError;
use tuff_core::command::args::Value;
use tuff_core::datapack::{Change, DIRECTORY, PackInfo, PackMeta, Status};
use tuff_item::text::TextComponent;

use super::{Context, Executor, executor_future, text};

fn string_arg(ctx: &Context<'_>, name: &str) -> Option<String> {
    match ctx.arg(name) {
        Some(Value::Str(value)) => Some(value.clone()),
        _ => None,
    }
}

async fn status(ctx: &Context<'_>, rescan: bool) -> Result<Status, CommandError> {
    ctx.shared()
        .datapacks
        .status(rescan)
        .await
        .ok_or_else(|| CommandError::simple("commands.reload.failure"))
}

fn missing_features(pack: &PackInfo) -> Vec<&str> {
    pack.missing_features()
}

/// The pack named `id`, which must be enabled (or not, when `enabling`).
fn pick<'s>(status: &'s Status, id: &str, enabling: bool) -> Result<&'s PackInfo, CommandError> {
    let pack = status
        .pack(id)
        .ok_or_else(|| CommandError::dynamic("commands.datapack.unknown", [id.to_owned()]))?;
    let enabled = status.is_selected(id);
    if enabling && enabled {
        return Err(CommandError::dynamic(
            "commands.datapack.enable.failed",
            [id.to_owned()],
        ));
    }
    if !enabling && !enabled {
        return Err(CommandError::dynamic(
            "commands.datapack.disable.failed",
            [id.to_owned()],
        ));
    }
    if !enabling && pack.provides_features() {
        return Err(CommandError::dynamic(
            "commands.datapack.disable.failed.feature",
            [id.to_owned()],
        ));
    }
    let missing = missing_features(pack);
    if enabling && !missing.is_empty() {
        return Err(CommandError::dynamic(
            "commands.datapack.enable.failed.no_flags",
            [id.to_owned(), missing.join(", ")],
        ));
    }
    Ok(pack)
}

fn link_list<'a>(packs: impl IntoIterator<Item = &'a PackInfo>, enabled: bool) -> TextComponent {
    let mut list = TextComponent::new("");
    for (index, pack) in packs.into_iter().enumerate() {
        if index > 0 {
            list = list.append(", ");
        }
        list = list.append(pack.chat_link(enabled));
    }
    list
}

async fn reload(ctx: &Context<'_>, change: Change) {
    if ctx.shared().datapacks.reload(change).await.is_err() {
        ctx.fail(TextComponent::translate("commands.reload.failure", []));
    }
}

async fn list_enabled(ctx: &Context<'_>) -> Result<i32, CommandError> {
    let status = status(ctx, true).await?;
    let enabled: Vec<&PackInfo> = status
        .selected
        .iter()
        .filter_map(|id| status.pack(id))
        .collect();
    let message = if enabled.is_empty() {
        TextComponent::translate("commands.datapack.list.enabled.none", [])
    } else {
        TextComponent::translate(
            "commands.datapack.list.enabled.success",
            [
                text(enabled.len()),
                link_list(enabled.iter().copied(), true),
            ],
        )
    };
    ctx.success(message, false).await;
    Ok(enabled.len() as i32)
}

async fn list_available(ctx: &Context<'_>) -> Result<i32, CommandError> {
    let status = status(ctx, true).await?;
    let available: Vec<&PackInfo> = status
        .available
        .iter()
        .filter(|p| !status.is_selected(&p.id) && missing_features(p).is_empty())
        .collect();
    let message = if available.is_empty() {
        TextComponent::translate("commands.datapack.list.available.none", [])
    } else {
        TextComponent::translate(
            "commands.datapack.list.available.success",
            [
                text(available.len()),
                link_list(available.iter().copied(), false),
            ],
        )
    };
    ctx.success(message, false).await;
    Ok(available.len() as i32)
}

fn portable(name: &str) -> bool {
    if name.ends_with('.') {
        return false;
    }
    let upper = name.to_ascii_uppercase();
    let stem = upper.split('.').next().unwrap_or("");
    let reserved = matches!(stem, "CON" | "PRN" | "AUX" | "NUL" | "COM" | "CLOCK$")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    !reserved
}

async fn create(ctx: &Context<'_>) -> Result<i32, CommandError> {
    let id = string_arg(ctx, "id").unwrap_or_default();
    let description = match ctx.arg("description") {
        Some(Value::Component(description)) => description.clone(),
        _ => TextComponent::new(""),
    };
    let valid_segment = !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.' | '_'));
    if !valid_segment {
        return Err(CommandError::dynamic(
            "commands.datapack.create.invalid_name",
            [id],
        ));
    }
    if !portable(&id) {
        return Err(CommandError::dynamic(
            "commands.datapack.create.invalid_full_name",
            [id],
        ));
    }
    let backend = ctx.shared().backend.clone();
    let prefix = format!("{DIRECTORY}{id}/");
    let file = format!("{DIRECTORY}{id}");
    let json = PackMeta::new_pack_json(&description);
    let created = tokio::task::spawn_blocking(move || {
        let exists = !backend.list_extra_files_under(&prefix)?.is_empty()
            || backend.list_extra_files_under(DIRECTORY)?.contains(&file);
        if exists {
            return Ok(false);
        }
        backend.write_extra_file(&format!("{prefix}pack.mcmeta"), json.as_bytes())?;
        Ok::<_, std::io::Error>(true)
    })
    .await;
    match created {
        Ok(Ok(true)) => {
            ctx.success(
                TextComponent::translate("commands.datapack.create.success", [text(&id)]),
                true,
            )
            .await;
            Ok(1)
        }
        Ok(Ok(false)) => Err(CommandError::dynamic(
            "commands.datapack.create.already_exists",
            [id],
        )),
        Ok(Err(err)) => {
            tuff_core::log::warn!("Failed to create pack {id}: {err}");
            Err(CommandError::dynamic(
                "commands.datapack.create.io_failure",
                [id],
            ))
        }
        Err(_) => Err(CommandError::dynamic(
            "commands.datapack.create.io_failure",
            [id],
        )),
    }
}

pub struct DataPackCommand;

impl Executor for DataPackCommand {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            if ctx.has("list") {
                if ctx.has("available") {
                    return list_available(ctx).await;
                }
                if ctx.has("enabled") {
                    return list_enabled(ctx).await;
                }
                return Ok(list_enabled(ctx).await? + list_available(ctx).await?);
            }
            if ctx.has("create") {
                return create(ctx).await;
            }
            let name = string_arg(ctx, "name").unwrap_or_default();
            let status = status(ctx, false).await?;
            let mut selected = status.selected.clone();
            let enabling = ctx.has("enable");
            let pack = pick(&status, &name, enabling)?.clone();
            if enabling {
                let index = if ctx.has("first") {
                    0
                } else if ctx.has("before") || ctx.has("after") {
                    let existing = string_arg(ctx, "existing").unwrap_or_default();
                    pick(&status, &existing, false)?;
                    let at = selected.iter().position(|id| *id == existing).unwrap_or(0);
                    if ctx.has("after") { at + 1 } else { at }
                } else {
                    selected.len()
                };
                selected.insert(index, pack.id.clone());
                ctx.success(
                    TextComponent::translate(
                        "commands.datapack.modify.enable",
                        [pack.chat_link(true)],
                    ),
                    true,
                )
                .await;
            } else {
                selected.retain(|id| *id != pack.id);
                ctx.success(
                    TextComponent::translate(
                        "commands.datapack.modify.disable",
                        [pack.chat_link(true)],
                    ),
                    true,
                )
                .await;
            }
            let count = selected.len() as i32;
            reload(ctx, Change::Select(selected)).await;
            Ok(count)
        })
    }

    fn suggest(
        &self,
        path: &[tuff_core::command::ParsedNode],
        partial: &str,
        input: &super::SuggestInput,
    ) -> Option<Vec<String>> {
        let packs = input.datapacks.known();
        let enabling = path.iter().any(|n| n.node.name == "enable")
            && !path
                .iter()
                .any(|n| matches!(n.node.name, "before" | "after"));
        let ids: Vec<String> = packs
            .available
            .iter()
            .filter(|p| packs.is_selected(&p.id) != enabling && missing_features(p).is_empty())
            .map(|p| tuff_core::datapack::quote_if_needed(&p.id))
            .filter(|id| id.starts_with(partial))
            .collect();
        Some(ids)
    }
}

pub struct Reload;

impl Executor for Reload {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            ctx.success(
                TextComponent::translate("commands.reload.success", []),
                true,
            )
            .await;
            reload(ctx, Change::Discover).await;
            Ok(0)
        })
    }
}
