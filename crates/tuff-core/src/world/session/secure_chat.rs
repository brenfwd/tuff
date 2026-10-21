use super::*;
use crate::protocol::chat_session::{
    self as secure, ChatChain, ChatError, ChatSession, MessageBody, Signature,
};

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

impl PlayerSession {
    /// Turns the wire forms of chat into the plain `Chat` and `ChatCommand`
    /// the rest of the pipeline handles, after checking acknowledgements and
    /// signatures. `None` when the packet was consumed or refused.
    pub(super) fn secure_chat(&mut self, packet: Serverbound) -> Option<Serverbound> {
        match packet {
            Serverbound::ChatAck(offset) => {
                if let Err(error) = self.body.last_seen.apply_offset(offset) {
                    self.chat_validation_failed(&error.0);
                }
                None
            }
            Serverbound::SignedChat(chat) => {
                let seen = self.apply_last_seen(&chat.update)?;
                let body = MessageBody {
                    content: &chat.message,
                    timestamp_ms: chat.timestamp_ms,
                    salt: chat.salt,
                    last_seen: &seen,
                };
                if let Err(error) = self.unpack_message(chat.signature.as_ref(), &body) {
                    self.refuse_chat(error);
                    return None;
                }
                Some(Serverbound::Chat(chat.message))
            }
            Serverbound::SignedCommand(command) => {
                self.apply_last_seen(&command.update)?;
                if let Some(error) =
                    self.skip_signed_arguments(command.signatures.len(), command.timestamp_ms)
                {
                    self.refuse_chat(error);
                    return None;
                }
                Some(Serverbound::ChatCommand(command.command))
            }
            other => Some(other),
        }
    }

    fn apply_last_seen(&mut self, update: &secure::LastSeenUpdate) -> Option<Vec<Signature>> {
        match self.body.last_seen.apply_update(update) {
            Ok(seen) => Some(seen),
            Err(error) => {
                self.chat_validation_failed(&error.0);
                None
            }
        }
    }

    fn chat_validation_failed(&mut self, detail: &str) {
        log::error!(
            "Failed to validate message acknowledgements from {}: {detail}",
            self.profile.name
        );
        self.kick(TextComponent::translate(secure::CHAT_VALIDATION_FAILED, []));
    }

    fn refuse_chat(&mut self, error: ChatError) {
        log::warn!(
            "Failed to update secure chat state for {}: {}",
            self.profile.name,
            error.translation_key()
        );
        self.system_message(&TextComponent::translate(error.translation_key(), []).color("red"));
    }

    fn unpack_message(
        &mut self,
        signature: Option<&Signature>,
        body: &MessageBody,
    ) -> Result<(), ChatError> {
        let key = self
            .body
            .chat_session
            .as_ref()
            .and_then(ChatSession::key)
            .zip(
                self.body
                    .chat_session
                    .as_ref()
                    .map(|s| s.is_expired(now_ms())),
            );
        let enforced = self.shared.profile_keys.enforcing();
        match (key, self.body.chat_chain.as_mut()) {
            (Some((key, expired)), Some(chain)) => {
                chain.unpack(&key, expired, signature.map(|s| s.as_slice()), body)
            }
            _ if enforced => Err(ChatError::MissingProfileKey),
            _ => Ok(()),
        }
    }

    fn skip_signed_arguments(&mut self, count: usize, timestamp_ms: i64) -> Option<ChatError> {
        if count == 0 {
            return None;
        }
        self.body
            .chat_chain
            .as_mut()
            .and_then(|chain| chain.skip(count, timestamp_ms).err())
    }

    pub(super) fn chat_session_update(&mut self, session: ChatSession) {
        let Some(old) = &self.body.chat_session else {
            return self.accept_chat_session(session);
        };
        if old.public_key == session.public_key && old.expires_at_ms == session.expires_at_ms {
            return;
        }
        if session.expires_at_ms < old.expires_at_ms {
            self.kick(TextComponent::translate(secure::EXPIRED_PUBLIC_KEY, []));
            return;
        }
        self.accept_chat_session(session);
    }

    fn accept_chat_session(&mut self, session: ChatSession) {
        if !self.shared.profile_keys.can_validate() {
            log::warn!(
                "Ignoring chat session from {}: Mojang's public keys are not loaded",
                self.profile.name
            );
            return;
        }
        if session.is_expired(now_ms()) {
            self.kick(TextComponent::translate(secure::EXPIRED_PUBLIC_KEY, []));
            return;
        }
        if session.key().is_none() || !self.shared.profile_keys.validate(self.profile.id, &session)
        {
            self.kick(TextComponent::translate(
                secure::INVALID_PUBLIC_KEY_SIGNATURE,
                [],
            ));
            return;
        }
        self.body.chat_chain = Some(ChatChain::new(self.profile.id, session.session_id));
        self.body.chat_session = Some(session);
    }
}
