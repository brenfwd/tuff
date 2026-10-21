//! Scanning, admitting, instantiating and enabling plugins, and grants.

use super::*;

impl Actor {
    /// Loads every module in the plugin directory, each after the plugins
    /// it depends on.
    pub(super) async fn scan(&mut self) {
        let Some(directory) = self.directory() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&directory) else {
            return;
        };
        let mut files: Vec<String> = entries
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| name.ends_with(".wasm") && !name.starts_with('.'))
            .collect();
        files.sort();
        let mut pending = Vec::new();
        for file in files {
            let path = directory.join(&file);
            match read_module(&path).await.and_then(|bytes| {
                manifest::read(&bytes)
                    .map(|manifest| (manifest, bytes))
                    .map_err(|err| format!("{}: {err}", path.display()))
            }) {
                Ok((manifest, bytes)) => pending.push((path, manifest, bytes)),
                Err(err) => log::error!("wasm plugin not loaded: {err}"),
            }
        }
        let manifests: Vec<&Manifest> = pending.iter().map(|(_, m, _)| m).collect();
        let plan = plan_load(&manifests);
        for (index, reason) in &plan.refused {
            log::error!("wasm plugin not loaded: {}: {reason}", manifests[*index].id);
        }
        let mut slots: Vec<Option<_>> = pending.into_iter().map(Some).collect();
        for index in plan.order {
            let Some((path, manifest, bytes)) = slots[index].take() else {
                continue;
            };
            match self.load(path, manifest, bytes).await {
                Ok(done) => log::info!("{done}"),
                Err(err) => log::error!("wasm plugin not loaded: {err}"),
            }
        }
    }

    pub(super) async fn load_file(&mut self, file: &str) -> Result<String, String> {
        self.turned_off()?;
        let directory = self
            .directory()
            .ok_or_else(|| "this world has no plugin directory".to_owned())?;
        let path = directory.join(module_name(file)?);
        let bytes = read_module(&path).await?;
        let manifest =
            manifest::read(&bytes).map_err(|err| format!("{}: {err}", path.display()))?;
        self.load(path, manifest, bytes).await
    }

    /// Checks `manifest` against the server and the loaded plugins;
    /// `replacing` is the plugin a reload swaps out.
    pub(super) fn admit(&self, manifest: &Manifest, replacing: Option<&str>) -> Result<(), String> {
        let id = &manifest.id;
        if manifest.api != abi::API_VERSION {
            return Err(format!(
                "{id} needs plugin API {}, this server speaks API {}",
                manifest.api,
                abi::API_VERSION
            ));
        }
        if self.shared.hook_owners.features.contains(&id.as_str()) {
            return Err(format!("{id} is the id of a built-in plugin"));
        }
        if replacing.is_none() && self.plugins.contains_key(id) {
            return Err(format!("{id} is already loaded; use reload"));
        }
        for dep in &manifest.depends {
            match self.plugins.get(&dep.id) {
                None => return Err(format!("{id} needs {dep}, which is not loaded")),
                Some(loaded) if !dep.accepts(loaded.manifest.version) => {
                    return Err(format!(
                        "{id} needs {dep}, but {} {} is loaded",
                        dep.id, loaded.manifest.version
                    ));
                }
                Some(_) => {}
            }
        }
        for soft in &manifest.soft_depends {
            if let Some(loaded) = self.plugins.get(&soft.id)
                && !soft.accepts(loaded.manifest.version)
            {
                log::warn!(
                    "wasm plugin {id} optionally needs {soft}, but {} {} is loaded; continuing without it",
                    soft.id,
                    loaded.manifest.version
                );
            }
        }
        for other in self
            .plugins
            .values()
            .filter(|p| Some(p.manifest.id.as_str()) != replacing)
        {
            let theirs = &other.manifest;
            if let Some(conflict) = manifest
                .conflicts
                .iter()
                .find(|c| c.id == theirs.id && c.accepts(theirs.version))
            {
                return Err(format!(
                    "{id} conflicts with {conflict}, and {} {} is loaded",
                    theirs.id, theirs.version
                ));
            }
            if let Some(conflict) = theirs
                .conflicts
                .iter()
                .find(|c| c.id == *id && c.accepts(manifest.version))
            {
                return Err(format!(
                    "{} conflicts with {conflict}, so {id} {} cannot load",
                    theirs.id, manifest.version
                ));
            }
        }
        if let Some(old) = replacing {
            for other in self.plugins.values() {
                for dep in other.manifest.depends.iter().filter(|dep| dep.id == old) {
                    if !dep.accepts(manifest.version) {
                        return Err(format!(
                            "{} needs {dep}, so {id} {} cannot replace it",
                            other.manifest.id, manifest.version
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn check_commands(
        &self,
        id: &str,
        registration: &Registration,
    ) -> Result<(), String> {
        let server = &self.shared.commands;
        for name in registration.command_names() {
            if server.roots().command(name).is_some() || server.executor(name).is_some() {
                return Err(format!("{id} claims /{name}, which the server already has"));
            }
            if let Some(owner) = self.commands.get(name).filter(|owner| *owner != id) {
                return Err(format!("{id} claims /{name}, which {owner} already has"));
            }
        }
        let builtin = server.roots().command("tuff").map(|tuff| tuff.node);
        for name in registration.tuff_names() {
            if builtin.is_some_and(|tuff| tuff.child(name).is_some()) {
                return Err(format!(
                    "{id} claims /tuff {name}, which the server already has"
                ));
            }
            if let Some(owner) = self.tuff_subcommands.get(name).filter(|owner| *owner != id) {
                return Err(format!(
                    "{id} claims /tuff {name}, which {owner} already has"
                ));
            }
        }
        for (name, target) in &registration.aliases {
            if server.roots().command(name).is_some() || server.executor(name).is_some() {
                return Err(format!("{id} claims /{name}, which the server already has"));
            }
            let taken = self
                .commands
                .get(name)
                .or_else(|| self.aliases.get(name).map(|(o, _)| o));
            if let Some(owner) = taken.filter(|owner| *owner != id) {
                return Err(format!("{id} claims /{name}, which {owner} already has"));
            }
            if registration.command_names().any(|command| command == name) {
                return Err(format!("{id} claims /{name} twice"));
            }
            let mut words = target.split(' ');
            let own = match words.next() {
                Some("tuff") => words
                    .next()
                    .is_some_and(|sub| registration.tuff_names().any(|s| s == sub)),
                Some(root) => registration.command_names().any(|c| c == root),
                None => false,
            };
            if !own {
                return Err(format!(
                    "{id} aliases /{name} to {target:?}, which it does not own"
                ));
            }
        }
        Ok(())
    }

    /// Compiles `bytes` and runs its registration, off the actor.
    pub(super) async fn instantiate(
        &mut self,
        manifest: &Manifest,
        bytes: Vec<u8>,
    ) -> Result<(Box<Sandbox>, Registration), String> {
        let id = manifest.id.as_str();
        let (_, initial) = watch::channel(Arc::new(self.policy_for(manifest).await));
        let engine = self.engine.clone();
        let limits = self.limits;
        let plugin = id.to_owned();
        let env = Env {
            shared: self.shared.clone(),
            runtime: tokio::runtime::Handle::current(),
        };
        let joined = tokio::task::spawn_blocking(move || {
            std::panic::catch_unwind(AssertUnwindSafe(|| {
                let mut sandbox = Sandbox::new(&engine, &bytes, &plugin, limits)?;
                sandbox.connect(env);
                sandbox.set_policy(initial);
                let registration = sandbox.register()?;
                Ok((Box::new(sandbox), registration))
            }))
            .unwrap_or_else(|_| Err(Fault("the plugin host panicked".into())))
        })
        .await
        .unwrap_or_else(|_| Err(Fault("the plugin call was cancelled".into())));
        joined.map_err(|fault: Fault| format!("{id} failed to load: {fault}"))
    }

    pub(super) async fn load(
        &mut self,
        path: PathBuf,
        manifest: Manifest,
        bytes: Vec<u8>,
    ) -> Result<String, String> {
        self.admit(&manifest, None)?;
        let (sandbox, registration) = self.instantiate(&manifest, bytes).await?;
        self.check_commands(&manifest.id, &registration)?;
        let done = format!("Loaded {} {}", manifest.id, manifest.version);
        let notes = self.capability_notes(&manifest).await;
        self.enable(path, manifest, sandbox, registration).await?;
        Ok(done + &notes)
    }

    /// The owner's stored grants, read from the config store on first use.
    pub(super) async fn grants(&mut self) -> &mut Grants {
        if self.grants.is_none() {
            let store = self.shared.config_store.clone();
            let rows = tokio::task::spawn_blocking(move || store.read_plugin_grants())
                .await
                .unwrap_or_else(|_| Err(std::io::Error::other("reading plugin grants panicked")))
                .unwrap_or_else(|err| {
                    log::error!("{err}; no plugin capabilities are granted");
                    Vec::new()
                });
            let (grants, errors) = Grants::from_rows(rows);
            for error in errors {
                log::error!("{error}; that plugin is granted nothing");
            }
            self.grants = Some(grants);
        }
        self.grants.as_mut().expect("grants were just read")
    }

    pub(super) fn policy_for<'a>(
        &'a mut self,
        manifest: &'a Manifest,
    ) -> Pin<Box<dyn Future<Output = Policy> + Send + 'a>> {
        Box::pin(self.building_policy(manifest))
    }

    pub(super) async fn building_policy(&mut self, manifest: &Manifest) -> Policy {
        let requests = manifest.capabilities.clone();
        let grants = self.grants().await;
        Policy::new(requests, grants.get(&manifest.id))
    }

    /// Requests the owner's grants leave out and grants that do nothing,
    /// logged and returned as a suffix for the load reply.
    pub(super) fn capability_notes<'a>(
        &'a mut self,
        manifest: &'a Manifest,
    ) -> Pin<Box<dyn Future<Output = String> + Send + 'a>> {
        Box::pin(self.noting(manifest))
    }

    pub(super) async fn noting(&mut self, manifest: &Manifest) -> String {
        let policy = self.policy_for(manifest).await;
        let notes = policy.report();
        for note in &notes {
            log::warn!("wasm plugin {}: {note}", manifest.id);
        }
        match notes.is_empty() {
            true => String::new(),
            false => format!(" ({})", notes.join("; ")),
        }
    }

    pub(super) fn grant_report(
        &mut self,
        id: String,
    ) -> Pin<Box<dyn Future<Output = Result<GrantReport, String>> + Send + '_>> {
        Box::pin(self.reporting(id))
    }

    pub(super) async fn reporting(&mut self, id: String) -> Result<GrantReport, String> {
        let id = id.as_str();
        let manifest = self
            .plugins
            .get(id)
            .map(|p| p.manifest.clone())
            .ok_or_else(|| format!("{id} is not loaded"))?;
        let policy = self.policy_for(&manifest).await;
        let grant = self.grants().await.get(id).cloned().unwrap_or_default();
        let mut report = crate::grants_edit::report(&manifest, &grant, &policy);
        for path in self.shadowed_routes(id).await {
            report.notes.push(format!(
                "route {path} is shadowed by a route registered later"
            ));
        }
        Ok(report)
    }

    pub(super) fn change_grant(
        &mut self,
        id: String,
        change: GrantChange,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + '_>> {
        Box::pin(self.changing(id, change))
    }

    pub(super) async fn changing(
        &mut self,
        id: String,
        change: GrantChange,
    ) -> Result<String, String> {
        let id = id.as_str();
        let manifest = self
            .plugins
            .get(id)
            .map(|p| p.manifest.clone())
            .ok_or_else(|| format!("{id} is not loaded"))?;
        let mut grant = self.grants().await.get(id).cloned().unwrap_or_default();
        let message = crate::grants_edit::apply_change(&mut grant, &manifest, change)?;
        let candidate = Policy::new(manifest.capabilities.clone(), Some(&grant));
        if let Some(conflict) = self.route_conflict(id, &candidate).await {
            return Err(format!("cannot grant the route: {conflict}"));
        }
        let body = (!grant.is_empty()).then(|| grant.render());
        let store = self.shared.config_store.clone();
        let plugin = id.to_owned();
        tokio::task::spawn_blocking(move || store.write_plugin_grant(&plugin, body.as_deref()))
            .await
            .map_err(|_| "saving the grant panicked".to_owned())?
            .map_err(|err| format!("cannot save the grant: {err}"))?;
        self.grants().await.set(id, grant);
        let policy = self.policy_for(&manifest).await;
        let notes = policy.report();
        if let Some(loaded) = self.plugins.get(id) {
            loaded.files.set_access(Access::of(&policy));
            loaded.policy.send_replace(Arc::new(policy));
        }
        self.publish();
        self.recheck_routes(id);
        self.sync_top_routes(id).await;
        log::info!("wasm plugin {id}: {message}");
        Ok(match notes.is_empty() {
            true => message,
            false => format!("{message} ({})", notes.join("; ")),
        })
    }

    /// Makes a registered plugin live, then runs its `tuff_enable`; a
    /// fault there takes everything back out. Boxed, like
    /// [`Self::disable`], to keep the actor's future type shallow enough
    /// for the compiler to prove it `Send`.
    pub(super) fn enable(
        &mut self,
        file: PathBuf,
        manifest: Manifest,
        sandbox: Box<Sandbox>,
        registration: Registration,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        Box::pin(self.enabling(file, manifest, sandbox, registration))
    }

    pub(super) async fn enabling(
        &mut self,
        file: PathBuf,
        manifest: Manifest,
        sandbox: Box<Sandbox>,
        registration: Registration,
    ) -> Result<(), String> {
        let id = manifest.id.clone();
        let mut sandbox = sandbox;
        sandbox.set_storage(self.store(&id).await?.storage.clone());
        self.generation += 1;
        self.add_panel(&id, &registration).await?;
        for name in registration.command_names() {
            self.commands.insert(name.to_owned(), id.clone());
        }
        for name in registration.tuff_names() {
            self.tuff_subcommands.insert(name.to_owned(), id.clone());
        }
        for (name, target) in &registration.aliases {
            self.aliases
                .insert(name.clone(), (id.clone(), target.clone()));
        }
        let policy = self.policy_for(&manifest).await;
        let (policy_tx, watcher) = watch::channel(Arc::new(policy));
        let net = crate::net::Net::new(
            watcher,
            self.resolver.clone(),
            &id,
            self.generation,
            self.io_queue.clone(),
        );
        net.set_reserved(Arc::new(ServerPorts(self.shared.clone())));
        sandbox.attach_background(net.shared().events.clone());
        sandbox.attach_net(net);
        sandbox.attach_index(self.index.subscribe());
        let files = self.files_for(&id, self.generation, &policy_tx.borrow());
        sandbox.set_files(files.clone());
        sandbox.set_policy(policy_tx.subscribe());
        let settings = self.add_extras(&id, &registration).await;
        self.plugins.insert(
            id.clone(),
            Loaded {
                files,
                policy: policy_tx,
                manifest,
                file,
                generation: self.generation,
                sandbox: Some(sandbox),
                registration,
                settings,
                guard: Default::default(),
            },
        );
        self.publish();
        let version = self.plugins[&id].manifest.version.to_string();
        self.announce_change(&id, version, true);
        self.sync_top_routes(&id).await;
        match self.call(&id, Sandbox::enable).await {
            Some(Ok(effects)) => {
                self.apply(&id, effects).await;
                Ok(())
            }
            Some(Err(fault)) => {
                self.remove(&id).await;
                self.unload_orphans().await;
                Err(format!("{id} failed to enable: {fault}"))
            }
            None => Err(format!("{id} was unloaded while enabling")),
        }
    }
}

pub(super) struct LoadPlan {
    pub order: Vec<usize>,
    pub refused: Vec<(usize, String)>,
}

/// Orders `manifests` for loading: hard, soft and load-before edges in one
/// graph, ties broken by id. Soft and load-before edges inside a cycle are
/// dropped with a warning; plugins in a cycle of hard edges are refused.
pub(super) fn plan_load(manifests: &[&Manifest]) -> LoadPlan {
    use std::collections::{BTreeSet, HashMap};
    let n = manifests.len();
    let mut index: HashMap<&str, usize> = HashMap::new();
    for (i, m) in manifests.iter().enumerate() {
        index.entry(m.id.as_str()).or_insert(i);
    }
    let hard: Vec<Vec<usize>> = manifests
        .iter()
        .map(|m| {
            m.depends
                .iter()
                .filter_map(|d| index.get(d.id.as_str()).copied())
                .collect()
        })
        .collect();
    let mut refused = Vec::new();
    let mut dead = vec![false; n];
    for i in 0..n {
        let mut stack = hard[i].clone();
        let mut seen = BTreeSet::new();
        let mut cyclic = false;
        while let Some(j) = stack.pop() {
            if j == i {
                cyclic = true;
                break;
            }
            if seen.insert(j) {
                stack.extend(&hard[j]);
            }
        }
        if cyclic {
            dead[i] = true;
        }
    }
    let members: Vec<&str> = (0..n)
        .filter(|&i| dead[i])
        .map(|i| manifests[i].id.as_str())
        .collect();
    for i in (0..n).filter(|&i| dead[i]) {
        refused.push((
            i,
            format!("part of a dependency cycle ({})", members.join(", ")),
        ));
    }
    let mut edges: Vec<(usize, usize, bool)> = Vec::new();
    for (i, m) in manifests.iter().enumerate().filter(|(i, _)| !dead[*i]) {
        for &d in &hard[i] {
            if !dead[d] {
                edges.push((d, i, true));
            }
        }
        for soft in &m.soft_depends {
            if let Some(&d) = index.get(soft.id.as_str()).filter(|&&d| !dead[d] && d != i) {
                edges.push((d, i, false));
            }
        }
        for x in &m.load_before {
            if let Some(&x) = index.get(x.as_str()).filter(|&&x| !dead[x] && x != i) {
                edges.push((i, x, false));
            }
        }
    }
    let key = |i: usize| (manifests[i].id.as_str(), i);
    let mut left: BTreeSet<(&str, usize)> = (0..n).filter(|&i| !dead[i]).map(key).collect();
    let mut order = Vec::new();
    while !left.is_empty() {
        let remaining: BTreeSet<usize> = left.iter().map(|k| k.1).collect();
        let ready = left
            .iter()
            .find(|k| {
                !edges
                    .iter()
                    .any(|(from, to, _)| *to == k.1 && remaining.contains(from))
            })
            .copied();
        let pick = match ready {
            Some(k) => k,
            None => {
                let cut = edges
                    .iter()
                    .filter(|(from, to, hard)| {
                        !hard && remaining.contains(from) && remaining.contains(to)
                    })
                    .min_by_key(|(from, to, _)| {
                        (manifests[*to].id.as_str(), manifests[*from].id.as_str())
                    })
                    .copied();
                match cut {
                    Some((from, to, _)) => {
                        log::warn!(
                            "wasm plugin ordering: dropping optional edge {} -> {} to break a cycle",
                            manifests[from].id,
                            manifests[to].id
                        );
                        edges.retain(|e| (e.0, e.1) != (from, to));
                        continue;
                    }
                    None => *left.iter().next().expect("non-empty"),
                }
            }
        };
        left.remove(&pick);
        order.push(pick.1);
    }
    LoadPlan { order, refused }
}

#[cfg(test)]
mod plan_tests {
    use super::*;

    fn manifest(text: &str) -> Manifest {
        manifest::parse(&format!("version=1.0.0\napi=0\n{text}")).unwrap()
    }

    fn ids(list: &[Manifest]) -> (Vec<String>, Vec<String>) {
        let refs: Vec<&Manifest> = list.iter().collect();
        let plan = plan_load(&refs);
        (
            plan.order.iter().map(|&i| list[i].id.clone()).collect(),
            plan.refused
                .iter()
                .map(|(i, _)| list[*i].id.clone())
                .collect(),
        )
    }

    #[tuff_core::watchdog]
    #[test]
    fn soft_and_load_before_order() {
        let list = [
            manifest("id=a\nsoft-depends=z, absent"),
            manifest("id=m\nload-before=a"),
            manifest("id=z"),
        ];
        assert_eq!(ids(&list).0, ["m", "z", "a"]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn deterministic_ties() {
        let mut list = [manifest("id=c"), manifest("id=a"), manifest("id=b")];
        let first = ids(&list);
        list.reverse();
        assert_eq!(first, ids(&list));
        assert_eq!(first.0, ["a", "b", "c"]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn hard_cycle_refused_with_members() {
        let list = [
            manifest("id=a\ndepends=b"),
            manifest("id=b\ndepends=a"),
            manifest("id=c\ndepends=a"),
            manifest("id=d"),
        ];
        let (order, refused) = ids(&list);
        assert_eq!(refused, ["a", "b"]);
        assert_eq!(order, ["c", "d"]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn soft_cycle_edge_dropped() {
        let list = [
            manifest("id=a\nsoft-depends=b"),
            manifest("id=b\nsoft-depends=a"),
        ];
        let (order, refused) = ids(&list);
        assert!(refused.is_empty());
        assert_eq!(order.len(), 2);
        let list = [
            manifest("id=a\nsoft-depends=b"),
            manifest("id=b\ndepends=a"),
        ];
        assert_eq!(ids(&list).0, ["a", "b"]);
    }
}
