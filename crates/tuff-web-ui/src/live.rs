//! The panel entries that come and go at runtime (WASM plugins register
//! them while loading and lose them on unload), merged with the frozen
//! entries from load time.
//!
//! One actor owns the live entries per owner; every change publishes a
//! fresh merged [`Installed`] snapshot through a watch channel, which is
//! what the routes read. Commands are buffered (a handful per plugin
//! load) and answered over oneshots, so a conflict is reported to the
//! registering plugin.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot, watch};

use super::panel::{Installed, MetricCard, PanelApi, PanelPage, valid_id, valid_scope};

/// What one owner adds to the panel.
#[derive(Clone, Debug, Default)]
pub struct Entries {
    pub pages: Vec<PanelPage>,
    pub apis: Vec<PanelApi>,
    pub cards: Vec<MetricCard>,
}

enum Command {
    SetBase(Installed),
    Install {
        owner: String,
        entries: Entries,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Remove {
        owner: String,
        done: oneshot::Sender<()>,
    },
}

/// A cheap reader of the current merged snapshot.
#[derive(Clone)]
pub struct PanelView(watch::Receiver<Arc<Installed>>);

impl PanelView {
    pub fn get(&self) -> Arc<Installed> {
        self.0.borrow().clone()
    }
}

#[derive(Clone)]
pub struct LivePanel {
    commands: mpsc::Sender<Command>,
    view: watch::Receiver<Arc<Installed>>,
}

impl LivePanel {
    pub fn spawn() -> Self {
        let (commands, rx) = mpsc::channel(64);
        let (publish, view) = watch::channel(Arc::new(Installed::default()));
        tokio::spawn(run(rx, publish));
        Self { commands, view }
    }

    /// Answers nothing: installs fail, the view stays empty.
    pub fn detached() -> Self {
        let (commands, _) = mpsc::channel(1);
        let (_, view) = watch::channel(Arc::new(Installed::default()));
        Self { commands, view }
    }

    pub fn view(&self) -> PanelView {
        PanelView(self.view.clone())
    }

    pub async fn set_base(&self, base: Installed) {
        let _ = self.commands.send(Command::SetBase(base)).await;
    }

    /// Adds `owner`'s entries; nothing is added if any key is taken.
    pub async fn install(&self, owner: &str, entries: Entries) -> Result<(), String> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Command::Install {
                owner: owner.to_owned(),
                entries,
                reply,
            })
            .await
            .map_err(|_| "the panel is not running".to_owned())?;
        answer
            .await
            .map_err(|_| "the panel is not running".to_owned())?
    }

    /// Takes `owner`'s entries out; the view no longer has them once
    /// this returns.
    pub async fn remove(&self, owner: &str) {
        let (done, removed) = oneshot::channel();
        let command = Command::Remove {
            owner: owner.to_owned(),
            done,
        };
        if self.commands.send(command).await.is_ok() {
            let _ = removed.await;
        }
    }
}

#[derive(Default)]
struct State {
    base: Installed,
    live: BTreeMap<String, Entries>,
}

impl State {
    fn merged(&self) -> Installed {
        let mut all = self.base.clone();
        for entries in self.live.values() {
            for page in &entries.pages {
                all.pages.insert(page.id.clone(), Arc::new(page.clone()));
            }
            for api in &entries.apis {
                all.apis.insert(api.id.clone(), Arc::new(api.clone()));
            }
            for card in &entries.cards {
                all.cards
                    .insert(card.series.clone(), Arc::new(card.clone()));
            }
        }
        all
    }

    fn check(&self, owner: &str, entries: &Entries) -> Result<(), String> {
        let current = self.merged();
        let mine = self.live.get(owner);
        let owned = |f: &dyn Fn(&Entries) -> bool| mine.is_some_and(f);
        for page in &entries.pages {
            page.validate()?;
            if current.pages.contains_key(&page.id)
                && !owned(&|e| e.pages.iter().any(|p| p.id == page.id))
            {
                return Err(format!("panel page {:?} is already registered", page.id));
            }
        }
        for api in &entries.apis {
            valid_id(&api.id)?;
            valid_scope(api.scope)?;
            if current.apis.contains_key(&api.id)
                && !owned(&|e| e.apis.iter().any(|a| a.id == api.id))
            {
                return Err(format!("panel api {:?} is already registered", api.id));
            }
        }
        for card in &entries.cards {
            card.validate()?;
            if current.cards.contains_key(&card.series)
                && !owned(&|e| e.cards.iter().any(|c| c.series == card.series))
            {
                return Err(format!(
                    "panel card for series {:?} is already registered",
                    card.series
                ));
            }
        }
        Ok(())
    }
}

async fn run(mut rx: mpsc::Receiver<Command>, publish: watch::Sender<Arc<Installed>>) {
    let mut state = State::default();
    while let Some(command) = rx.recv().await {
        let mut answer = None;
        let mut removed = None;
        match command {
            Command::SetBase(base) => state.base = base,
            Command::Install {
                owner,
                entries,
                reply,
            } => {
                let outcome = state.check(&owner, &entries);
                if outcome.is_ok() {
                    state.live.insert(owner, entries);
                }
                answer = Some((reply, outcome));
            }
            Command::Remove { owner, done } => {
                state.live.remove(&owner);
                removed = Some(done);
            }
        }
        publish.send_replace(Arc::new(state.merged()));
        if let Some((reply, outcome)) = answer {
            let _ = reply.send(outcome);
        }
        if let Some(done) = removed {
            let _ = done.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Scope;
    use crate::panel::Asset;

    fn page(id: &str) -> PanelPage {
        PanelPage::new(
            id,
            "Title",
            Scope::Plugin("x.y"),
            Asset::module("main.js", "export function mount() {}"),
        )
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn live_entries_install_conflict_and_remove() {
        let live = LivePanel::spawn();
        let view = live.view();
        let entries = |id: &str| Entries {
            pages: vec![page(id)],
            ..Entries::default()
        };
        live.install("a", entries("one")).await.unwrap();
        assert!(view.get().pages.contains_key("one"));
        assert!(live.install("b", entries("one")).await.is_err());
        live.install("a", entries("one")).await.unwrap();
        live.install("b", entries("two")).await.unwrap();
        assert_eq!(view.get().pages.len(), 2);
        live.remove("a").await;
        assert_eq!(view.get().pages.len(), 1);
        live.install("b", Entries::default()).await.unwrap();
        assert!(view.get().pages.is_empty());
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn live_entries_keep_the_frozen_base() {
        let live = LivePanel::spawn();
        let mut base = Installed::default();
        base.pages.insert("base".into(), Arc::new(page("base")));
        live.set_base(base).await;
        let entries = Entries {
            pages: vec![page("base")],
            ..Entries::default()
        };
        assert!(live.install("a", entries).await.is_err());
        assert!(live.install("a", Entries::default()).await.is_ok());
        assert!(live.view().get().pages.contains_key("base"));
        assert!(
            LivePanel::detached()
                .install("a", Entries::default())
                .await
                .is_err()
        );
    }
}
