//! The feature system: how vanilla (and third-party) behavior registers
//! itself against the inert core.
//!
//! Core ships with no behavior of its own. A `Server` is assembled from
//! a `Feature` list; the `tuff` binary's `vanilla_features::all()` is the
//! standard vanilla set.

use crate::registry::Registries;
use tuff_item::load::{LoadError, LoadErrorKind, Phase};

/// A loadable unit of server behavior. Implemented once per feature
/// crate (`vanilla-mobs`, `vanilla-worldgen`, ...); a third-party
/// feature implements the same trait with no special privileges.
pub trait Feature: Send + Sync {
    /// Stable identifier, used in conflict/error messages and as the
    /// `registered_by` attribution on every entry this feature
    /// registers. Convention: the crate name (`"vanilla-mobs"`).
    fn id(&self) -> &'static str;

    /// Called once per phase, in the order features were passed to
    /// [`load_all`]: every feature's `Types` call happens before any
    /// feature's `Content` call, and so on. `cx.phase()` says which
    /// phase this call is; implementations only need to match on the
    /// phases they use. The default does nothing.
    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        let _ = cx;
        Ok(())
    }
}

/// Passed to [`Feature::load`]. Wraps [`Registries`] with the current
/// phase and the calling feature's id, so every `register`/`replace`
/// call the feature makes is automatically attributed without the
/// feature having to pass its own id through every call.
pub struct LoadContext<'a> {
    phase: Phase,
    feature_id: &'static str,
    registries: &'a mut Registries,
}

impl<'a> LoadContext<'a> {
    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn feature_id(&self) -> &'static str {
        self.feature_id
    }

    pub fn registries(&mut self) -> &mut Registries {
        self.registries
    }

    /// Turn a plain message into a [`LoadError`] attributed to this
    /// context's feature, for the "anything else" case.
    pub fn error(&self, message: impl Into<String>) -> LoadError {
        LoadError {
            feature: self.feature_id,
            kind: LoadErrorKind::Other(message.into()),
        }
    }
}

/// Runs every phase, across every feature, in order: all of `Types`
/// before any `Content`, all of `Content` before any `Resolve`, then
/// `Registries::freeze_all` once. `features` is loaded in the order
/// given -- there is no dependency solver; a feature that
/// must run after another is simply listed after it by whoever
/// assembles the server.
pub fn load_all(
    features: &[Box<dyn Feature>],
    registries: &mut Registries,
) -> Result<(), LoadError> {
    for &phase in &[Phase::Types, Phase::Content, Phase::Resolve] {
        for feature in features {
            let mut cx = LoadContext {
                phase,
                feature_id: feature.id(),
                registries,
            };
            feature.load(&mut cx)?;
            HookOwners::attribute(feature.id(), registries);
            for key in registries.config_keys.iter_mut() {
                key.feature.get_or_insert(feature.id());
            }
        }
    }
    install_advancements(registries)?;
    registries.freeze_all();
    let content = registries.custom.resolve()?;
    for def in content.items() {
        registries.commands.add_custom_item(
            def.id.clone(),
            crate::command::item::ItemInput::from_template(&def.template()),
        );
    }
    Ok(())
}

/// Which feature registered each hook, by position in its `Registries`
/// list, so timings can attribute hook time to features. [`load_all`] fills
/// it; registries built without it leave hooks unattributed.
#[derive(Clone, Debug, Default)]
pub struct HookOwners {
    /// Every loaded feature, in load order.
    pub features: Vec<&'static str>,
    pub region_hooks: Vec<&'static str>,
    pub session_hooks: Vec<&'static str>,
    pub packet_interceptors: Vec<&'static str>,
}

/// The owner of a hook no feature is known to have registered.
pub const UNATTRIBUTED: &str = "unattributed";

impl HookOwners {
    fn attribute(feature: &'static str, registries: &mut Registries) {
        let owners = &mut registries.hook_owners;
        if !owners.features.contains(&feature) {
            owners.features.push(feature);
        }
        for (owners, len) in [
            (&mut owners.region_hooks, registries.region_hooks.len()),
            (&mut owners.session_hooks, registries.session_hooks.len()),
            (
                &mut owners.packet_interceptors,
                registries.packet_interceptors.len(),
            ),
        ] {
            owners.resize(len.max(owners.len()), feature);
        }
    }

    /// The feature that registered entry `index` of `list` (one of this
    /// struct's lists).
    pub fn owner(list: &[&'static str], index: usize) -> &'static str {
        list.get(index).copied().unwrap_or(UNATTRIBUTED)
    }
}

/// A span charging what runs inside it to the feature that registered
/// entry `index` of `owners` (one of [`HookOwners`]' lists).
#[inline(always)]
pub fn feature_span(owners: &[&'static str], index: usize) -> tuff_timings::Span {
    tuff_timings::span!(
        tuff_timings::FEATURE,
        tuff_timings::Tag::Name(HookOwners::owner(owners, index))
    )
}

/// [`feature_span`] for hooks that await.
#[inline(always)]
pub fn feature_timer(owners: &[&'static str], index: usize) -> tuff_timings::Timer {
    tuff_timings::timer!(
        tuff_timings::FEATURE,
        tuff_timings::Tag::Name(HookOwners::owner(owners, index))
    )
}

/// Builds core's advancement tree from what features registered, unless a
/// feature supplied its own.
fn install_advancements(registries: &mut Registries) -> Result<(), LoadError> {
    use crate::world::advancement::{self, Tree, trigger::Registry as Triggers};
    const OWNER: &str = "tuff-core";
    registries.player_saved.register(
        OWNER,
        "advancements",
        Box::new(advancement::SavedProgress),
    )?;
    if registries.advancements.get().is_some() {
        return Ok(());
    }
    let mut triggers = Triggers::new();
    for (trigger, handler) in registries.advancement_triggers.iter() {
        triggers.register(trigger, handler.as_ref().clone());
    }
    let mut packs = registries.advancement_packs.iter();
    let tree = match (packs.next(), registries.advancement_definitions.is_empty()) {
        (None, true) => Tree::new(&[], triggers),
        (Some((_, pack)), true) if packs.next().is_none() => Tree::new(pack.as_ref(), triggers),
        _ => {
            let base = registries
                .advancement_packs
                .iter()
                .flat_map(|(_, pack)| advancement::definitions_of(pack));
            let base: Vec<_> = base.collect();
            let mut extra: Vec<_> = registries
                .advancement_definitions
                .iter()
                .map(|(_, definition)| *definition.as_ref())
                .collect();
            loop {
                let known: std::collections::HashSet<&str> = base
                    .iter()
                    .chain(&extra)
                    .map(|definition| definition.name)
                    .collect();
                let before = extra.len();
                extra.retain(|definition| {
                    let kept = definition
                        .parent
                        .is_none_or(|parent| known.contains(parent));
                    if !kept {
                        log::warn!(
                            "Skipping advancement {}: unknown parent {}",
                            definition.name,
                            definition.parent.unwrap_or_default()
                        );
                    }
                    kept
                });
                if extra.len() == before {
                    break;
                }
            }
            let merged =
                advancement::merge(&[], base.into_iter().chain(extra)).map_err(|message| {
                    LoadError {
                        feature: OWNER,
                        kind: LoadErrorKind::Other(message),
                    }
                })?;
            Tree::new(merged, triggers)
        }
    };
    registries
        .advancements
        .fill(OWNER, std::sync::Arc::new(tree))
}
