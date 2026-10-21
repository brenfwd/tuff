//! Packet tracing lives in `tuff-trace`; this module re-exports it and wires
//! in the packet decoders and actor supervision that only core can provide.

use std::path::PathBuf;

use tuff_trace::Tracer;

/// Spawns the tracer actor under panic supervision.
pub fn spawn_tracer(directory: PathBuf) -> Tracer {
    Tracer::spawn_using(directory, |runner| {
        tuff_supervise::spawn(
            "the packet tracer",
            tuff_supervise::restarting("the packet tracer", runner, |runner| {
                Box::pin(runner.run())
            }),
        );
    })
}

pub mod dump {
    use std::io::Read;

    use bytes::Bytes;

    use crate::protocol::handshake::Intention;
    use crate::protocol::login::{LoginAcknowledged, LoginStart};
    use crate::protocol::{Packet, configuration, play};
    use tuff_trace::ConnState;
    use tuff_trace::format::FormatError;
    use tuff_wire::PacketReader;

    pub use tuff_trace::dump::{Decoder, Options, RawOnly};

    pub struct CoreDecoder;

    impl Decoder for CoreDecoder {
        fn clientbound(&self, state: ConnState, payload: &Bytes) -> Option<String> {
            crate::trace_decode::typed_text(state, payload)
        }

        fn serverbound(&self, state: ConnState, payload: &Bytes) -> Option<String> {
            match state {
                ConnState::Handshake => decode::<Intention>(payload),
                ConnState::Login => {
                    decode::<LoginStart>(payload).or_else(|| decode::<LoginAcknowledged>(payload))
                }
                ConnState::Configuration => configuration::Serverbound::decode(payload.clone())
                    .ok()
                    .map(|p| format!("{p:?}")),
                ConnState::Play => match play::Serverbound::decode(payload.clone()) {
                    Ok(play::Serverbound::Ignored) | Err(_) => None,
                    Ok(packet) => Some(format!("{packet:?}")),
                },
                ConnState::Status => None,
            }
        }
    }

    fn decode<P: Packet + std::fmt::Debug>(payload: &Bytes) -> Option<String> {
        let mut reader = PacketReader::new(payload.clone());
        if reader.read_var_int().ok()? != P::ID {
            return None;
        }
        let packet = P::decode(&mut reader).ok()?;
        (reader.remaining() == 0).then(|| format!("{packet:?}"))
    }

    pub fn render(
        input: impl Read,
        options: Options,
        line: impl FnMut(&str),
    ) -> Result<u64, FormatError> {
        tuff_trace::dump::render(input, options, &CoreDecoder, line)
    }

    pub fn render_to_string(input: impl Read, options: Options) -> Result<String, FormatError> {
        tuff_trace::dump::render_to_string(input, options, &CoreDecoder)
    }
}
