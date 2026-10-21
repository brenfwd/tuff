//! Plugin-to-plugin notifies and requests: queued in the actor, delivered
//! as fresh guest calls, never nested.

use super::*;
use crate::sandbox::OutMessage;
use crate::wit::bindings::capability_types::{CallError, Incoming, PluginChange, ReplyDone};
use std::collections::{BTreeSet, HashMap};

type Outcome = Result<Vec<u8>, CallError>;

/// One unit of work waiting for its turn; `hops` is how many deliveries
/// led to it.
pub(super) enum Item {
    Message {
        from: String,
        target: String,
        interface: String,
        method: u64,
        payload: Vec<u8>,
        request: Option<u64>,
        hops: usize,
    },
    Complete {
        requester: String,
        generation: u64,
        handle: u32,
        outcome: Outcome,
        hops: usize,
    },
    Change {
        target: String,
        change: PluginChange,
    },
}

impl Item {
    fn reset_hops(&mut self) {
        match self {
            Item::Message { hops, .. } | Item::Complete { hops, .. } => *hops = 0,
            Item::Change { .. } => {}
        }
    }
}

struct Pending {
    requester: String,
    requester_generation: u64,
    handle: u32,
    provider: String,
    provider_generation: u64,
}

#[derive(Default)]
pub(super) struct Traffic {
    pub outbox: VecDeque<Item>,
    deferred: VecDeque<Item>,
    pending: HashMap<u64, Pending>,
    deadlines: BTreeSet<(u64, u64)>,
    deadline_of: HashMap<u64, u64>,
    next_request: u64,
    inbox: HashMap<String, usize>,
}

impl Actor {
    fn generation_of(&self, id: &str) -> Option<u64> {
        self.plugins.get(id).map(|plugin| plugin.generation)
    }

    fn complete(&mut self, pending: Pending, outcome: Outcome, hops: usize) {
        let current = self.generation_of(&pending.requester) == Some(pending.requester_generation);
        if current {
            self.traffic.outbox.push_back(Item::Complete {
                requester: pending.requester,
                generation: pending.requester_generation,
                handle: pending.handle,
                outcome,
                hops,
            });
        }
    }

    fn settle(&mut self, request: u64) -> Option<Pending> {
        if let Some(deadline) = self.traffic.deadline_of.remove(&request) {
            self.traffic.deadlines.remove(&(deadline, request));
        }
        self.traffic.pending.remove(&request)
    }

    /// Queues one message a call produced.
    pub(super) fn enqueue(&mut self, from: &str, message: OutMessage) {
        let OutMessage {
            target,
            interface,
            method,
            payload,
            request,
        } = message;
        let requester = request.map(|(handle, timeout)| {
            (
                Pending {
                    requester: from.to_owned(),
                    requester_generation: self.generation_of(from).unwrap_or(0),
                    handle,
                    provider: target.clone(),
                    provider_generation: self.generation_of(&target).unwrap_or(0),
                },
                timeout,
            )
        });
        let refuse = |this: &mut Self, pending: Option<(Pending, u32)>, error: CallError| {
            if let Some((pending, _)) = pending {
                this.complete(pending, Err(error), this.hops);
            }
        };
        if !self.plugins.contains_key(&target) {
            return refuse(self, requester, CallError::TargetGone);
        }
        if self.traffic.inbox.get(&target).copied().unwrap_or(0) >= abi::MAX_INBOX {
            log::debug!("wasm plugin {target} has a full inbox; dropped a message from {from}");
            return refuse(self, requester, CallError::Busy);
        }
        let id = requester.map(|(pending, timeout)| {
            self.traffic.next_request += 1;
            let id = self.traffic.next_request;
            let due = *self.shared.server_ticks.borrow() + u64::from(timeout);
            self.traffic.deadlines.insert((due, id));
            self.traffic.deadline_of.insert(id, due);
            self.traffic.pending.insert(id, pending);
            id
        });
        *self.traffic.inbox.entry(target.clone()).or_default() += 1;
        self.traffic.outbox.push_back(Item::Message {
            from: from.to_owned(),
            target,
            interface,
            method,
            payload,
            request: id,
            hops: self.hops,
        });
    }

    /// A provider answered request `request`.
    pub(super) fn answer(&mut self, from: &str, request: u64, outcome: Result<Vec<u8>, String>) {
        if self
            .traffic
            .pending
            .get(&request)
            .is_none_or(|pending| pending.provider != from)
        {
            return;
        }
        if let Some(pending) = self.settle(request) {
            let outcome = outcome.map_err(CallError::Refused);
            self.complete(pending, outcome, self.hops);
        }
    }

    /// Plugin `id` is gone: its requests vanish and requests to it fail.
    pub(super) fn drop_traffic(&mut self, id: &str) {
        let doomed: Vec<u64> = self
            .traffic
            .pending
            .iter()
            .filter(|(_, p)| p.requester == id || p.provider == id)
            .map(|(request, _)| *request)
            .collect();
        for request in doomed {
            if let Some(pending) = self.settle(request)
                && pending.provider == id
                && pending.requester != id
            {
                self.complete(pending, Err(CallError::TargetGone), 0);
            }
        }
        let keep = |item: &Item| match item {
            Item::Message { target, .. } => target != id,
            Item::Complete { requester, .. } => requester != id,
            Item::Change { target, .. } => target != id,
        };
        self.traffic.outbox.retain(keep);
        self.traffic.deferred.retain(keep);
        self.traffic.inbox.remove(id);
    }

    /// Tells plugins that declare `id` as a dependency that it changed.
    pub(super) fn announce_change(&mut self, id: &str, version: String, loaded: bool) {
        let targets: Vec<String> = self
            .plugins
            .values()
            .filter(|plugin| {
                plugin.manifest.id != id
                    && plugin
                        .manifest
                        .depends
                        .iter()
                        .chain(&plugin.manifest.soft_depends)
                        .any(|dep| dep.id == id)
            })
            .map(|plugin| plugin.manifest.id.clone())
            .collect();
        for target in targets {
            self.traffic.outbox.push_back(Item::Change {
                target,
                change: PluginChange {
                    id: id.to_owned(),
                    version: version.clone(),
                    loaded,
                },
            });
        }
    }

    /// Moves deferred work back to the front of the line and times out
    /// requests that ran out of ticks; run on every server tick.
    pub(super) fn service_traffic(&mut self, now: u64) {
        let mut deferred = std::mem::take(&mut self.traffic.deferred);
        for item in &mut deferred {
            item.reset_hops();
        }
        deferred.append(&mut self.traffic.outbox);
        self.traffic.outbox = deferred;
        while let Some(&(due, request)) = self.traffic.deadlines.first() {
            if due > now {
                break;
            }
            if let Some(pending) = self.settle(request) {
                self.complete(pending, Err(CallError::TimedOut), 0);
            }
        }
    }

    fn defer(&mut self, item: Item) {
        self.traffic.deferred.push_back(item);
    }

    /// Hands queued work to its plugins, following what that produces
    /// until [`abi::MAX_HOPS`] deep or the turn budget runs out; the rest
    /// waits for the next tick.
    pub(super) async fn deliver(&mut self) {
        let mut budget = abi::TURN_BUDGET;
        while let Some(item) = self.traffic.outbox.pop_front() {
            let hops = match &item {
                Item::Message { hops, .. } | Item::Complete { hops, .. } => *hops,
                Item::Change { .. } => 0,
            };
            if budget == 0 || hops >= abi::MAX_HOPS {
                self.defer(item);
                continue;
            }
            budget -= 1;
            if let Item::Message { target, .. } = &item
                && let Some(count) = self.traffic.inbox.get_mut(target)
            {
                *count = count.saturating_sub(1);
            }
            match item {
                Item::Message {
                    from,
                    target,
                    interface,
                    method,
                    payload,
                    request,
                    hops,
                } => {
                    let live = request.is_none_or(|id| {
                        self.traffic.pending.get(&id).is_some_and(|pending| {
                            self.generation_of(&target) == Some(pending.provider_generation)
                        })
                    });
                    if !live {
                        continue;
                    }
                    let msg = Incoming {
                        sender: from.clone(),
                        interface,
                        method,
                        payload,
                        request,
                    };
                    match self
                        .call(&target, move |sandbox| sandbox.message(&msg))
                        .await
                    {
                        Some(Ok(effects)) => {
                            self.hops = hops + 1;
                            self.apply(&target, effects).await;
                            self.hops = 0;
                        }
                        Some(Err(fault)) => {
                            if let Some(pending) = request.and_then(|id| self.settle(id)) {
                                self.complete(pending, Err(CallError::Faulted), hops + 1);
                            }
                            self.fail(&target, "a message", &fault).await;
                        }
                        None => {
                            if let Some(pending) = request.and_then(|id| self.settle(id)) {
                                self.complete(pending, Err(CallError::TargetGone), hops + 1);
                            }
                        }
                    }
                }
                Item::Complete {
                    requester,
                    generation,
                    handle,
                    outcome,
                    hops,
                } => {
                    if self.generation_of(&requester) != Some(generation) {
                        continue;
                    }
                    let event = IoEvent::Reply(ReplyDone { handle, outcome });
                    match self
                        .call(&requester, move |sandbox| sandbox.io(&event))
                        .await
                    {
                        Some(Ok(effects)) => {
                            self.hops = hops + 1;
                            self.apply(&requester, effects).await;
                            self.hops = 0;
                        }
                        Some(Err(fault)) => self.fail(&requester, "a reply", &fault).await,
                        None => {}
                    }
                }
                Item::Change { target, change } => {
                    let event = IoEvent::PluginChange(change);
                    match self.call(&target, move |sandbox| sandbox.io(&event)).await {
                        Some(Ok(effects)) => {
                            self.apply(&target, effects).await;
                        }
                        Some(Err(fault)) => self.fail(&target, "a plugin change", &fault).await,
                        None => {}
                    }
                }
            }
        }
    }
}
