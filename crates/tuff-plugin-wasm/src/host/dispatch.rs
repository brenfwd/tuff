//! Commands, events, logins, io and timers delivered to plugins.

use super::*;

impl Actor {
    pub(super) async fn command(&mut self, player: Player, line: String) -> Option<CommandOutcome> {
        let (owner, line) = self.index.borrow().resolve(&line)?;
        match self
            .call(&owner, move |sandbox| sandbox.command(player, &line))
            .await?
        {
            Ok((result, effects)) => Some(CommandOutcome {
                result,
                replies: self.apply(&owner, effects).await,
            }),
            Err(fault) => {
                self.fail(&owner, "a command", &fault).await;
                Some(CommandOutcome {
                    result: Err(format!("{owner} failed and was unloaded")),
                    replies: Vec::new(),
                })
            }
        }
    }

    pub(super) async fn suggest(&mut self, player: Player, line: String) -> Option<Vec<String>> {
        let (owner, mut resolved) = self.index.borrow().resolve(&line)?;
        if line.ends_with(' ') && !resolved.ends_with(' ') {
            resolved.push(' ');
        }
        let line = resolved;
        match self
            .call(&owner, move |sandbox| sandbox.suggest(player, &line))
            .await?
        {
            Ok(found) => Some(found),
            Err(fault) => {
                self.fail(&owner, "a suggestion", &fault).await;
                None
            }
        }
    }

    pub(super) async fn event(&mut self, event: Event, player: Option<Player>) {
        let kind = sandbox::kind_of(&event);
        let listeners: Vec<String> = self
            .plugins
            .values()
            .filter(|plugin| plugin.registration.events.intersects(kind))
            .filter(|plugin| match &event {
                Event::ConfigChange(_) => false,
                Event::Payload(message) => plugin.registration.channels.contains(&message.channel),
                Event::InsideBlock(inside) => {
                    let block = inside.state.split('[').next().unwrap_or_default();
                    plugin
                        .registration
                        .watched
                        .iter()
                        .any(|watched| crate::portal::same_block(watched, block))
                }
                _ => true,
            })
            .map(|plugin| plugin.manifest.id.clone())
            .collect();
        self.deliver_event(listeners, event, player.clone()).await;
        if kind == EventKinds::LEAVE
            && let Some(player) = player
        {
            for plugin in self.plugins.values_mut() {
                if let Some(sandbox) = plugin.sandbox.as_mut() {
                    sandbox.forget_player(player.uuid);
                }
            }
        }
    }

    pub(super) async fn login(&mut self, player: Player, address: String) -> Option<String> {
        let listeners: Vec<String> = self
            .plugins
            .values()
            .filter(|plugin| plugin.registration.events.intersects(EventKinds::LOGIN))
            .map(|plugin| plugin.manifest.id.clone())
            .collect();
        let event = Arc::new(Event::Login(crate::wit::bindings::types::LoginAttempt {
            player: player.to_wit(),
            address,
        }));
        let mut refusal = None;
        for id in listeners {
            let (who, event) = (player.clone(), event.clone());
            match self
                .call(&id, move |sandbox| sandbox.event(&event, Some(who)))
                .await
            {
                Some(Ok(mut effects)) => {
                    refusal = refusal.or(effects.login_refusal.take());
                    self.apply(&id, effects).await;
                }
                Some(Err(fault)) => self.fail(&id, "a login event", &fault).await,
                None => {}
            }
            if refusal.is_some() {
                break;
            }
        }
        refusal
    }

    pub(super) fn detailed_event(
        &mut self,
        event: Event,
        player: Option<Player>,
        detail: crate::guard::Detail,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let kind = sandbox::kind_of(&event);
        let listeners: Vec<String> = self
            .plugins
            .values()
            .filter(|plugin| plugin.registration.events.intersects(kind))
            .map(|plugin| plugin.manifest.id.clone())
            .collect();
        self.deliver_event_with(listeners, event, player, Some(Arc::new(detail)))
    }

    pub(super) async fn deliver_event(
        &mut self,
        listeners: Vec<String>,
        event: Event,
        player: Option<Player>,
    ) {
        self.deliver_event_with(listeners, event, player, None)
            .await;
    }

    pub(super) fn deliver_event_with(
        &mut self,
        listeners: Vec<String>,
        event: Event,
        player: Option<Player>,
        detail: Option<Arc<crate::guard::Detail>>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let event = Arc::new(event);
            for id in listeners {
                let (player, event, detail) = (player.clone(), event.clone(), detail.clone());
                match self
                    .call(&id, move |sandbox| {
                        sandbox.event_with(&event, player, detail.as_deref())
                    })
                    .await
                {
                    Some(Ok(effects)) => {
                        self.apply(&id, effects).await;
                    }
                    Some(Err(fault)) => self.fail(&id, "an event", &fault).await,
                    None => {}
                }
            }
        })
    }

    pub(super) async fn deliver_io(&mut self, delivery: crate::net::Delivery) {
        let id = delivery.plugin.clone();
        let current = self
            .plugins
            .get(&id)
            .is_some_and(|plugin| plugin.generation == delivery.generation);
        if !current {
            return;
        }
        match self
            .call(&id, move |sandbox| sandbox.io(&delivery.event))
            .await
        {
            Some(Ok(effects)) => {
                self.apply(&id, effects).await;
            }
            Some(Err(fault)) => self.fail(&id, "an I/O event", &fault).await,
            None => {}
        }
    }

    pub(super) async fn fire_due(&mut self) {
        let now = *self.shared.server_ticks.borrow();
        self.service_traffic(now);
        self.deliver().await;
        if now >= self.flushed_at + FLUSH_TICKS {
            self.flushed_at = now;
            self.flush_all().await;
        }
        while let Some(entry) = self.timers.first_entry() {
            if entry.key().0 > now {
                break;
            }
            let timer = entry.remove();
            let current = self
                .plugins
                .get(&timer.plugin)
                .is_some_and(|plugin| plugin.generation == timer.generation);
            if !current {
                continue;
            }
            let token = timer.token;
            match self
                .call(&timer.plugin, move |sandbox| sandbox.timer(token))
                .await
            {
                Some(Ok(effects)) => {
                    self.apply(&timer.plugin, effects).await;
                }
                Some(Err(fault)) => self.fail(&timer.plugin, "a timer", &fault).await,
                None => {}
            }
            self.deliver().await;
        }
    }
}
