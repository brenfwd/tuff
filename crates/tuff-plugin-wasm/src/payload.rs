//! Custom payload packets from clients, reported to the plugins that
//! registered the channel.

use std::future::Future;
use std::pin::Pin;

use tuff_core::protocol::play::Serverbound;
use tuff_core::world::session::PlayerSession;
use tuff_core::world::session::hooks::{PacketInterceptor, Verdict};

use crate::host::WasmHost;
use crate::wit::bindings::types::{Event, PayloadMessage};

pub struct Payloads;

impl PacketInterceptor for Payloads {
    fn intercept<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        packet: &'a Serverbound,
    ) -> Pin<Box<dyn Future<Output = Verdict> + Send + 'a>> {
        if let Serverbound::CustomPayload { channel, data } = packet
            && let Some(host) = session.shared.services.get::<WasmHost>()
            && host.snapshot().channels.iter().any(|c| c == channel)
        {
            let who = crate::bridge::player(session);
            host.event(
                Event::Payload(PayloadMessage {
                    player: who.to_wit(),
                    channel: channel.clone(),
                    data: data.to_vec(),
                }),
                Some(who),
            );
            return Box::pin(async { Verdict::Cancel });
        }
        Box::pin(async { Verdict::Allow })
    }
}
