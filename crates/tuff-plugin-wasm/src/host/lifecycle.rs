//! Removing, storing, flushing, unloading and reloading plugins.

use super::*;

impl Actor {
    /// Takes plugin `id` and everything it registered out of the host.
    pub(super) async fn remove(&mut self, id: &str) -> Option<Loaded> {
        let loaded = self.plugins.remove(id)?;
        self.drop_extras(id, &loaded.registration).await;
        self.commands.retain(|_, owner| owner != id);
        self.tuff_subcommands.retain(|_, owner| owner != id);
        self.aliases.retain(|_, (owner, _)| owner != id);
        self.timers.retain(|_, timer| timer.plugin != id);
        self.drop_traffic(id);
        self.publish();
        self.announce_change(id, loaded.manifest.version.to_string(), false);
        self.drop_routes(id).await;
        crate::display::remove_all(&self.shared, id).await;
        crate::dimensions::discard_all(&self.shared, id);
        crate::fake::remove(&self.shared, id, None).await;
        self.flush(id).await;
        Some(loaded)
    }

    /// A plugin's file store with the access `policy` gives it now.
    pub(super) fn files_for(&self, id: &str, generation: u64, policy: &Policy) -> Arc<Files> {
        let root = self.directory().map_or_else(
            || PathBuf::from("plugin-files-unavailable"),
            |dir| dir.join(id).join("files"),
        );
        let limits = policy.limits();
        let bounds = crate::files::Bounds {
            entries: limits.files,
            read: limits.message_bytes,
            pending: limits.pending_events,
        };
        let (plugin, generation, tx) = (id.to_owned(), generation, self.file_done.0.clone());
        let files = Files::new(root, bounds, move |done| {
            let _ = tx.send(FileDone {
                plugin: plugin.clone(),
                generation,
                done,
            });
        });
        files.set_access(match self.directory() {
            Some(_) => Access::of(policy),
            None => Access::NotGranted,
        });
        Arc::new(files)
    }

    pub(super) async fn file_finished(&mut self, finished: FileDone) {
        let FileDone {
            plugin,
            generation,
            done: (operation, outcome),
        } = finished;
        if self
            .plugins
            .get(&plugin)
            .is_none_or(|loaded| loaded.generation != generation)
        {
            return;
        }
        let event =
            IoEvent::File(crate::wit::bindings::capability_types::FileDone { operation, outcome });
        match self.call(&plugin, move |sandbox| sandbox.io(&event)).await {
            Some(Ok(effects)) => {
                self.apply(&plugin, effects).await;
            }
            Some(Err(fault)) => self.fail(&plugin, "a file result", &fault).await,
            None => {}
        }
        self.deliver().await;
    }

    pub(super) fn store_path(&self, id: &str) -> Option<PathBuf> {
        Some(storage::path(&self.directory()?, id))
    }

    pub(super) async fn read_store(&self, id: &str) -> Result<Storage, String> {
        let Some(path) = self.store_path(id) else {
            return Ok(Storage::default());
        };
        let quota = self.limits.storage;
        tokio::task::spawn_blocking(move || storage::read(&path, quota))
            .await
            .unwrap_or_else(|_| Err("reading the plugin store was cancelled".into()))
    }

    /// Plugin `id`'s store, read from its file the first time.
    pub(super) async fn store(&mut self, id: &str) -> Result<&Stored, String> {
        if !self.stores.contains_key(id) {
            let storage = self
                .read_store(id)
                .await
                .map_err(|err| format!("{id}'s stored data cannot be read: {err}"))?;
            self.stores.insert(
                id.to_owned(),
                Stored {
                    storage,
                    dirty: false,
                },
            );
        }
        Ok(&self.stores[id])
    }

    /// Writes plugin `id`'s store if it changed.
    pub(super) async fn flush(&mut self, id: &str) {
        let path = self.store_path(id);
        let Some(stored) = self.stores.get_mut(id).filter(|stored| stored.dirty) else {
            return;
        };
        stored.dirty = false;
        let Some(path) = path else {
            return;
        };
        let storage = stored.storage.clone();
        let written = tokio::task::spawn_blocking(move || storage::write(&path, &storage)).await;
        if !matches!(written, Ok(Ok(()))) {
            log::error!("wasm plugin {id}'s stored data could not be written");
            if let Some(stored) = self.stores.get_mut(id) {
                stored.dirty = true;
            }
        }
    }

    pub(super) async fn flush_all(&mut self) {
        let ids: Vec<String> = self.stores.keys().cloned().collect();
        for id in ids {
            self.flush(&id).await;
        }
    }

    pub(super) async fn purge(&mut self, id: &str) -> Result<String, String> {
        let had = match self.stores.remove(id) {
            Some(stored) => !stored.storage.is_empty(),
            None => !self.read_store(id).await.unwrap_or_default().is_empty(),
        };
        if let Some(path) = self.store_path(id) {
            let removed =
                tokio::task::spawn_blocking(move || storage::write(&path, &Storage::default()))
                    .await;
            if !matches!(removed, Ok(Ok(()))) {
                return Err(format!("{id}'s stored data could not be deleted"));
            }
        }
        if let Some(directory) = self.directory() {
            let root = directory.join(id).join("files");
            let removed = tokio::task::spawn_blocking(move || crate::files::purge(&root)).await;
            if !matches!(removed, Ok(Ok(()))) {
                return Err(format!("{id}'s files could not be deleted"));
            }
        }
        if let Some(plugin) = self.plugins.get(id) {
            plugin.files.rescan();
        }
        if let Some(sandbox) = self
            .plugins
            .get_mut(id)
            .and_then(|plugin| plugin.sandbox.as_mut())
        {
            sandbox.set_storage(Storage::default());
            self.stores.insert(
                id.to_owned(),
                Stored {
                    storage: Storage::default(),
                    dirty: false,
                },
            );
        }
        Ok(match had {
            true => format!("Deleted the data {id} stored"),
            false => format!("{id} has no stored data"),
        })
    }

    /// Plugin `id` faulted: it is dropped without running more of its
    /// code, and so is anything left needing it.
    pub(super) async fn fail(&mut self, id: &str, during: &str, fault: &Fault) {
        log::error!("wasm plugin {id} faulted during {during} and was unloaded: {fault}");
        self.remove(id).await;
        self.unload_orphans().await;
    }

    pub(super) fn dependents(&self, id: &str) -> Vec<String> {
        self.plugins
            .values()
            .filter(|plugin| plugin.manifest.depends.iter().any(|dep| dep.id == id))
            .map(|plugin| plugin.manifest.id.clone())
            .collect()
    }

    /// Unloads plugins whose dependencies are gone.
    pub(super) async fn unload_orphans(&mut self) {
        while let Some(orphan) = self
            .plugins
            .values()
            .find(|plugin| {
                plugin
                    .manifest
                    .depends
                    .iter()
                    .any(|dep| !self.plugins.contains_key(&dep.id))
            })
            .map(|plugin| plugin.manifest.id.clone())
        {
            log::warn!("wasm plugin {orphan} unloaded: a plugin it needs is gone");
            self.disable(&orphan).await;
        }
    }

    /// Runs plugin `id`'s `tuff_disable` and removes it.
    pub(super) fn disable<'a>(
        &'a mut self,
        id: &'a str,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(self.disabling(id))
    }

    pub(super) async fn disabling(&mut self, id: &str) {
        match self.call(id, Sandbox::disable).await {
            Some(Ok(effects)) => {
                self.apply(id, effects).await;
            }
            Some(Err(fault)) => {
                log::error!("wasm plugin {id} faulted while disabling: {fault}")
            }
            None => {}
        }
        self.remove(id).await;
    }

    pub(super) async fn unload(&mut self, id: &str) -> Result<String, String> {
        if !self.plugins.contains_key(id) {
            return Err(format!("{id} is not loaded"));
        }
        let dependents = self.dependents(id);
        if !dependents.is_empty() {
            return Err(format!(
                "{} depend on {id}; unload them first",
                dependents.join(", ")
            ));
        }
        self.disable(id).await;
        Ok(format!("Unloaded {id}"))
    }

    pub(super) async fn unload_all(&mut self) {
        while let Some(leaf) = self
            .plugins
            .keys()
            .rev()
            .find(|id| self.dependents(id).is_empty())
            .cloned()
        {
            self.disable(&leaf).await;
        }
        let rest: Vec<String> = self.plugins.keys().cloned().collect();
        for id in rest {
            self.remove(&id).await;
        }
        self.flush_all().await;
    }

    pub(super) async fn reload(&mut self, id: &str) -> Result<String, String> {
        self.turned_off()?;
        let Some(old) = self.plugins.get(id) else {
            return Err(format!("{id} is not loaded"));
        };
        let (path, old_version) = (old.file.clone(), old.manifest.version);
        let bytes = read_module(&path).await?;
        let manifest =
            manifest::read(&bytes).map_err(|err| format!("{}: {err}", path.display()))?;
        if manifest.id != id {
            return Err(format!(
                "{} now declares id {}, not {id}",
                path.display(),
                manifest.id
            ));
        }
        self.admit(&manifest, Some(id))?;
        let (sandbox, registration) = self.instantiate(&manifest, bytes).await?;
        self.check_commands(id, &registration)?;
        let new_version = manifest.version;
        let old = self.call(id, Sandbox::disable).await;
        match old {
            Some(Ok(effects)) => {
                self.apply(id, effects).await;
            }
            Some(Err(fault)) => log::error!("wasm plugin {id} faulted while disabling: {fault}"),
            None => {}
        }
        self.remove(id).await;
        let notes = self.capability_notes(&manifest).await;
        self.enable(path, manifest, sandbox, registration).await?;
        Ok(format!(
            "Reloaded {id} {old_version} -> {new_version}{notes}"
        ))
    }
}
