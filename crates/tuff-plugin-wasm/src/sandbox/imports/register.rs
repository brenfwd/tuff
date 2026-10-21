use super::super::*;

impl State {
    pub(in crate::sandbox) fn resolve_providers(
        &self,
        target: Option<&str>,
        interface: &str,
        method: u64,
        payload: &[u8],
    ) -> Result<Vec<String>, CallError> {
        if !self.running() {
            return Err(CallError::Refused("messages are sent while running".into()));
        }
        if payload.len() > self.message_bytes() {
            return Err(CallError::TooLarge);
        }
        if self.effects.messages.len() >= self.limits.sends {
            return Err(CallError::Refused("too many messages in one call".into()));
        }
        let index = self.index.as_ref().map(|index| index.borrow().clone());
        let table = index
            .as_ref()
            .and_then(|index| index.providers.get(interface));
        let mut found: Vec<String> = Vec::new();
        let mut known = false;
        for (plugin, methods) in table.into_iter().flatten() {
            if target.is_some_and(|wanted| wanted != plugin) || *plugin == self.plugin {
                continue;
            }
            known = true;
            if methods.contains(&method) {
                found.push(plugin.clone());
            }
        }
        if !known {
            return Err(CallError::NoProvider);
        }
        if found.is_empty() {
            return Err(CallError::NoMethod);
        }
        for plugin in &found {
            self.may_message(plugin)?;
        }
        Ok(found)
    }

    pub(in crate::sandbox) fn host_log(
        &mut self,
        level: LogLevel,
        message: &str,
    ) -> value::Result<()> {
        let plugin = &self.plugin;
        match level {
            LogLevel::Error => log::error!(target: "wasm-plugin", "[{plugin}] {message}"),
            LogLevel::Warn => log::warn!(target: "wasm-plugin", "[{plugin}] {message}"),
            LogLevel::Info => log::info!(target: "wasm-plugin", "[{plugin}] {message}"),
            LogLevel::Debug => log::debug!(target: "wasm-plugin", "[{plugin}] {message}"),
            LogLevel::Trace => log::trace!(target: "wasm-plugin", "[{plugin}] {message}"),
        }
        Ok(())
    }

    pub(in crate::sandbox) fn host_register_command(
        &mut self,
        tree: CommandTree,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.register_tree(tree, false))
    }

    pub(in crate::sandbox) fn host_register_tuff_subcommand(
        &mut self,
        tree: CommandTree,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.register_tree(tree, true))
    }

    pub(in crate::sandbox) fn host_register_alias(
        &mut self,
        name: &str,
        target: &str,
    ) -> value::Result<Result<(), RegisterError>> {
        Ok(self.registering().and_then(|()| {
            if !abi::valid_command(name) {
                return Err(RegisterError::Invalid(format!(
                    "{name:?} is not a valid alias name"
                )));
            }
            if !abi::valid_alias_target(target) {
                return Err(RegisterError::Invalid(format!(
                    "{target:?} is not a valid alias target"
                )));
            }
            let aliases = &mut self.registration.aliases;
            if aliases.iter().any(|(known, _)| known == name) {
                return Err(RegisterError::Invalid(format!(
                    "{name} is already an alias"
                )));
            }
            if aliases.len() >= abi::MAX_ALIASES {
                return Err(RegisterError::LimitReached(abi::MAX_ALIASES as u32));
            }
            aliases.push((name.to_owned(), target.to_owned()));
            Ok(())
        }))
    }
}
