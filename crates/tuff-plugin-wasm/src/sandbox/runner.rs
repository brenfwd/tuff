//! The sandbox: one plugin instance, its engine and its entry points.

use super::*;

/// The engine every sandbox shares: fuel metering on, recursion bounded.
pub fn engine() -> Engine {
    crate::wit::runtime::engine(abi::MAX_RECURSION)
}

pub(super) fn guest_error(what: &str, error: String) -> Fault {
    Fault(format!("{what} failed: {error}"))
}

pub struct Sandbox {
    runtime: Runtime<State>,
    module: Vec<u8>,
}

impl Sandbox {
    /// Compiles and instantiates `module` for plugin `plugin`. Runs the
    /// module's start function (metered) but no plugin callback.
    pub fn new(
        engine: &Engine,
        module: &[u8],
        plugin: &str,
        limits: Limits,
    ) -> Result<Self, Fault> {
        let runtime = Runtime::new(
            engine,
            crate::wit::schema(),
            module,
            State::new(plugin, limits),
            limits,
            limits.bounds(),
        )?;
        if !runtime.exports(bindings::GUEST_REGISTER) {
            return Err(Fault(format!(
                "module exports no {}",
                bindings::EXPORTS[bindings::GUEST_REGISTER]
            )));
        }
        Ok(Self {
            runtime,
            module: module.to_vec(),
        })
    }

    pub(super) fn state(&mut self) -> &mut State {
        self.runtime.host_mut()
    }

    pub(super) fn begin(&mut self, caller: Option<Player>) {
        let state = self.state();
        state.effects = Effects::default();
        state.caller = caller;
        state.reads = 0;
    }

    pub(super) fn finish(&mut self) -> Effects {
        let state = self.state();
        state.caller = None;
        state.effects.violations = state.stashes.take_violations();
        std::mem::take(&mut state.effects)
    }

    pub(super) fn require(&self, index: usize) -> Result<(), Fault> {
        match self.runtime.exports(index) {
            true => Ok(()),
            false => Err(Fault(format!(
                "module exports no {}",
                bindings::EXPORTS[index]
            ))),
        }
    }

    /// Runs `register`, the only time commands and events may be claimed.
    pub fn register(&mut self) -> Result<Registration, Fault> {
        self.begin(None);
        self.state().phase = Phase::Register;
        let result = bindings::guest_register(&mut self.runtime);
        self.state().phase = Phase::Running;
        self.finish();
        result?.map_err(|e| guest_error("register", e))?;
        let registration = self.runtime.host().registration.clone();
        if !registration.commands.is_empty()
            || !registration.tuff_subcommands.is_empty()
            || !registration.aliases.is_empty()
        {
            self.require(bindings::GUEST_RUN_COMMAND)?;
        }
        if !registration.events.is_empty() {
            self.require(bindings::GUEST_ON_EVENT)?;
        }
        Ok(registration)
    }

    pub(super) fn optional(
        &mut self,
        index: usize,
        caller: Option<Player>,
        call: impl FnOnce(&mut Runtime<State>) -> Result<(), Fault>,
    ) -> Result<Effects, Fault> {
        if !self.runtime.exports(index) {
            return Ok(Effects::default());
        }
        self.begin(caller);
        let result = call(&mut self.runtime);
        let effects = self.finish();
        result?;
        Ok(effects)
    }

    /// Runs `enable`, if exported.
    pub fn enable(&mut self) -> Result<Effects, Fault> {
        self.optional(bindings::GUEST_ENABLE, None, bindings::guest_enable)
    }

    /// Runs `disable`, if exported.
    pub fn disable(&mut self) -> Result<Effects, Fault> {
        self.optional(bindings::GUEST_DISABLE, None, bindings::guest_disable)
    }

    /// Runs one of the plugin's commands for `player`: `line` is the whole
    /// command without its slash, after alias rewriting. A line its tree
    /// does not accept fails without running guest code.
    pub fn command(
        &mut self,
        player: Player,
        line: &str,
    ) -> Result<(Result<i32, String>, Effects), Fault> {
        let state = self.runtime.host();
        let Some((tree, input)) = state.registration.tree_for(line) else {
            return Ok((Err(format!("Unknown command {line}")), Effects::default()));
        };
        let permitted = |node: &str| permitted(state, &player, node);
        let dimension = player.position.as_ref().map(|p| p.dimension.clone());
        let online = || online_players(state, dimension.as_deref());
        let cx = tree::Context {
            permitted: &permitted,
            origin: player.position.as_ref().map(|p| p.at),
            sender: Some(player.to_wit()),
            online: &online,
        };
        let matched = match tree::walk(tree, input, &cx) {
            Ok(matched) => matched,
            Err(why) => return Ok((Err(why), Effects::default())),
        };
        let call = Invocation {
            handler: matched.handler,
            sender: Some(player.to_wit()),
            args: matched.args,
            line: line.to_owned(),
        };
        self.invoke(Some(player), &call)
    }

    /// Completions for the argument at the end of `line` (no slash), asked
    /// of the guest (its queued effects are dropped) for each suggesting node the line can reach; empty when
    /// the guest exports no `suggest`.
    pub fn suggest(&mut self, player: Player, line: &str) -> Result<Vec<String>, Fault> {
        let state = self.runtime.host();
        let Some((tree, input)) = state.registration.tree_for(line) else {
            return Ok(Vec::new());
        };
        let permitted = |node: &str| permitted(state, &player, node);
        let dimension = player.position.as_ref().map(|p| p.dimension.clone());
        let online = || online_players(state, dimension.as_deref());
        let cx = tree::Context {
            permitted: &permitted,
            origin: player.position.as_ref().map(|p| p.at),
            sender: Some(player.to_wit()),
            online: &online,
        };
        let targets = tree::suggest_targets(tree, input, &cx);
        if targets.is_empty() || !self.runtime.exports(bindings::GUEST_SUGGEST) {
            return Ok(Vec::new());
        }
        let mut found: Vec<String> = Vec::new();
        for target in targets {
            let request = SuggestRequest {
                node: target.node,
                partial: target.partial,
                sender: Some(player.to_wit()),
                args: target.args,
                line: line.to_owned(),
            };
            self.begin(Some(player.clone()));
            let result = bindings::guest_suggest(&mut self.runtime, &request);
            drop(self.finish());
            found.extend(result?);
        }
        Ok(found)
    }

    /// Runs `run-command` with a prepared invocation, acting for `player`.
    pub fn invoke(
        &mut self,
        player: Option<Player>,
        call: &Invocation,
    ) -> Result<(Result<i32, String>, Effects), Fault> {
        self.require(bindings::GUEST_RUN_COMMAND)?;
        self.begin(player);
        let result = bindings::guest_run_command(&mut self.runtime, call);
        let effects = self.finish();
        Ok((result?, effects))
    }

    /// Delivers `event`, acting for `player`, if the plugin subscribed.
    pub fn event(&mut self, event: &Event, player: Option<Player>) -> Result<Effects, Fault> {
        self.event_with(event, player, None)
    }

    /// [`Self::event`] with what the `acting-*` imports answer meanwhile.
    pub fn event_with(
        &mut self,
        event: &Event,
        player: Option<Player>,
        detail: Option<&crate::guard::Detail>,
    ) -> Result<Effects, Fault> {
        if !self
            .runtime
            .host()
            .registration
            .events
            .intersects(kind_of(event))
        {
            return Ok(Effects::default());
        }
        self.require(bindings::GUEST_ON_EVENT)?;
        self.begin(player);
        self.state().deciding_login = matches!(event, Event::Login(_));
        self.state().guard.detail = detail.cloned();
        let result = bindings::guest_on_event(&mut self.runtime, event);
        self.state().deciding_login = false;
        self.state().guard.detail = None;
        let effects = self.finish();
        result?;
        Ok(effects)
    }

    /// Fires the timer the plugin scheduled with `token`.
    pub fn timer(&mut self, token: u32) -> Result<Effects, Fault> {
        self.optional(bindings::GUEST_ON_TIMER, None, |runtime| {
            bindings::guest_on_timer(runtime, &token)
        })
    }

    /// Delivers a message another plugin sent, if the plugin takes them.
    pub fn message(
        &mut self,
        msg: &bindings::capability_types::Incoming,
    ) -> Result<Effects, Fault> {
        if let Some(request) = msg.request {
            self.state().owed.insert(request);
        }
        self.optional(bindings::GUEST_ON_MESSAGE, None, |runtime| {
            bindings::guest_on_message(runtime, msg)
        })
    }

    /// Lets the plugin see the loaded plugins and their interfaces.
    pub fn attach_index(&mut self, index: tokio::sync::watch::Receiver<Arc<crate::host::Index>>) {
        self.state().index = Some(index);
    }

    /// Gives guest code a way into the server: world queries, operator
    /// levels and the permission store.
    pub fn connect(&mut self, env: Env) {
        self.state().env = Some(env);
    }

    /// Gives the plugin its network context; without one every network
    /// call is refused.
    /// Lets the plugin run background tasks on separate instances of its
    /// module; results arrive through `events`. Must be called inside the
    /// tokio runtime that runs them.
    pub fn attach_background(&mut self, events: crate::net::Events) {
        let limits = self.state().limits;
        let module = self.module.clone();
        self.state().background = Some(crate::background::Background::new(module, limits, events));
    }

    pub fn attach_net(&mut self, net: crate::net::Net) {
        self.state().net = Some(net);
    }

    pub fn set_files(&mut self, files: Arc<Files>) {
        self.state().files = Some(files);
    }

    pub fn set_policy(&mut self, policy: tokio::sync::watch::Receiver<Arc<crate::caps::Policy>>) {
        self.state().policy = Some(policy);
    }

    /// Delivers one asynchronous capability result to `on-io`, if exported.
    pub fn io(&mut self, event: &bindings::capability_types::IoEvent) -> Result<Effects, Fault> {
        if let bindings::capability_types::IoEvent::Reply(done) = event {
            self.state().outstanding.remove(&done.handle);
        }
        self.optional(bindings::CAPABILITY_EVENTS_ON_IO, None, |runtime| {
            bindings::capability_events_on_io(runtime, event)
        })
    }

    /// Puts items taken from a player into one of the plugin's stashes.
    pub fn fill_stash(&mut self, stash: u64, stacks: Vec<tuff_item::item::ItemStack>) {
        self.state().stashes.fill(stash, stacks);
    }

    /// Replaces the plugin's working copy of its persistent store.
    pub fn set_storage(&mut self, storage: Storage) {
        self.state().storage = storage;
    }

    pub fn storage(&self) -> &Storage {
        &self.runtime.host().storage
    }

    /// Drops everything the plugin keeps for player `uuid`.
    pub fn forget_player(&mut self, uuid: u128) {
        self.state().players.remove(&uuid);
    }

    /// How many players the plugin keeps state for.
    pub fn players_with_state(&self) -> usize {
        self.runtime.host().players.len()
    }
}
