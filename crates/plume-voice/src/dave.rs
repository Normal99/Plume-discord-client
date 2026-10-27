//! DAVE, Discord's end-to-end encryption for calls, on top of `davey`.
//!
//! The voice server drives an MLS group: it hands us an external sender,
//! we send a key package, and proposals, commits and welcomes follow as
//! people join and leave. Each change is a "transition" that everyone
//! acknowledges before it takes effect. This module turns those messages
//! into replies and keeps the session in step.

use std::borrow::Cow;
use std::num::NonZeroU16;

use davey::{DaveSession, MediaType, OPUS_SILENCE_PACKET, ProposalsOperationType};
use tracing::{debug, warn};

use crate::protocol::{self, op};

/// The DAVE version we speak.
pub const MAX_PROTOCOL_VERSION: u16 = davey::DAVE_PROTOCOL_VERSION;

/// How long (seconds) unencrypted audio is still accepted after a change.
const TRANSITION_EXPIRY: u32 = 10;

/// Something to send back to the voice server.
#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    Text(String),
    Binary(Vec<u8>),
}

pub struct Dave {
    user_id: u64,
    channel_id: u64,
    protocol_version: u16,
    session: Option<DaveSession>,
    /// A transition we've acknowledged but not executed: (id, version).
    pending: Option<(u16, u16)>,
    downgraded: bool,
}

impl Dave {
    pub fn new(user_id: u64, channel_id: u64) -> Self {
        Dave {
            user_id,
            channel_id,
            protocol_version: 0,
            session: None,
            pending: None,
            downgraded: false,
        }
    }

    /// Whether calls are end-to-end encrypted right now.
    pub fn is_active(&self) -> bool {
        self.protocol_version != 0 && self.session.as_ref().is_some_and(DaveSession::is_ready)
    }

    /// The code people can compare to check nobody's listening in.
    pub fn privacy_code(&self) -> Option<&str> {
        self.session.as_ref()?.voice_privacy_code().filter(|c| !c.is_empty())
    }

    /// The session description says which version the call uses.
    pub fn start(&mut self, protocol_version: u16) -> Vec<Reply> {
        self.protocol_version = protocol_version;
        self.reinit()
    }

    pub fn on_prepare_transition(&mut self, transition_id: u16, protocol_version: u16) -> Vec<Reply> {
        self.pending = Some((transition_id, protocol_version));
        if transition_id == 0 {
            // Transition 0 means "now": there's nobody to wait for.
            self.execute(0);
            return Vec::new();
        }
        if protocol_version == 0
            && let Some(session) = &mut self.session
        {
            session.set_passthrough_mode(true, Some(120));
        }
        vec![Reply::Text(protocol::transition_ready(transition_id))]
    }

    pub fn on_execute_transition(&mut self, transition_id: u16) {
        self.execute(transition_id);
    }

    pub fn on_prepare_epoch(&mut self, epoch: u64, protocol_version: u16) -> Vec<Reply> {
        // Epoch 1 is a brand-new group: start over with a fresh key package.
        if epoch == 1 {
            self.protocol_version = protocol_version;
            return self.reinit();
        }
        Vec::new()
    }

    /// A binary MLS message. `connected` is everyone the server said is in
    /// the call, used to check that proposals only add people who are.
    pub fn on_binary(&mut self, opcode: u8, payload: &[u8], connected: &[u64]) -> Vec<Reply> {
        let Some(session) = &mut self.session else {
            debug!("DAVE message {opcode} without a session");
            return Vec::new();
        };
        match opcode {
            op::DAVE_EXTERNAL_SENDER => {
                if let Err(error) = session.set_external_sender(payload) {
                    warn!("DAVE external sender: {error}");
                }
                Vec::new()
            }
            op::DAVE_PROPOSALS => {
                let Some((&kind, proposals)) = payload.split_first() else {
                    return Vec::new();
                };
                let kind = if kind == 1 {
                    ProposalsOperationType::REVOKE
                } else {
                    ProposalsOperationType::APPEND
                };
                match session.process_proposals(kind, proposals, Some(connected)) {
                    Ok(Some(commit)) => {
                        let welcome = commit.welcome.unwrap_or_default();
                        vec![Reply::Binary(protocol::binary(
                            op::DAVE_COMMIT_WELCOME,
                            &[&commit.commit, &welcome],
                        ))]
                    }
                    Ok(None) => Vec::new(),
                    Err(error) => {
                        warn!("DAVE proposals: {error}");
                        Vec::new()
                    }
                }
            }
            op::DAVE_ANNOUNCE_COMMIT | op::DAVE_WELCOME => {
                let Some((id, message)) = payload.split_first_chunk::<2>() else {
                    return Vec::new();
                };
                let transition_id = u16::from_be_bytes(*id);
                let result = if opcode == op::DAVE_WELCOME {
                    session.process_welcome(message).map_err(|e| e.to_string())
                } else {
                    session.process_commit(message).map_err(|e| e.to_string())
                };
                match result {
                    Ok(()) if transition_id == 0 => Vec::new(),
                    Ok(()) => {
                        self.pending = Some((transition_id, self.protocol_version));
                        vec![Reply::Text(protocol::transition_ready(transition_id))]
                    }
                    Err(error) => {
                        warn!(
                            "DAVE {}: {error}; starting over",
                            if opcode == op::DAVE_WELCOME {
                                "welcome"
                            } else {
                                "commit"
                            }
                        );
                        let mut replies = vec![Reply::Text(protocol::invalid_commit_welcome(transition_id))];
                        replies.extend(self.reinit());
                        replies
                    }
                }
            }
            other => {
                debug!("unknown DAVE opcode {other}");
                Vec::new()
            }
        }
    }

    /// Encrypts an Opus frame for sending. Before the group is ready (or
    /// with DAVE off) frames go out as they are, like the official client.
    pub fn encrypt<'a>(&mut self, opus: &'a [u8]) -> Cow<'a, [u8]> {
        if !self.is_active() || opus == OPUS_SILENCE_PACKET {
            return Cow::Borrowed(opus);
        }
        let session = self.session.as_mut().expect("active means a session");
        match session.encrypt_opus(opus) {
            Ok(frame) => frame,
            Err(error) => {
                warn!("DAVE encrypt: {error}");
                Cow::Borrowed(opus)
            }
        }
    }

    /// Decrypts someone's Opus frame. `None` means it couldn't be (drop it).
    pub fn decrypt<'a>(&mut self, user_id: u64, frame: &'a [u8]) -> Option<Cow<'a, [u8]>> {
        let Some(session) = self.session.as_mut().filter(|s| s.is_ready()) else {
            return Some(Cow::Borrowed(frame));
        };
        if frame == OPUS_SILENCE_PACKET || (self.protocol_version == 0 && session.can_passthrough(user_id)) {
            return Some(Cow::Borrowed(frame));
        }
        match session.decrypt(user_id, MediaType::AUDIO, frame) {
            Ok(plain) => Some(Cow::Owned(plain)),
            // During a transition unencrypted frames are still allowed.
            Err(_) if session.can_passthrough(user_id) => Some(Cow::Borrowed(frame)),
            Err(error) => {
                debug!("DAVE decrypt from {user_id}: {error}");
                None
            }
        }
    }

    fn execute(&mut self, transition_id: u16) {
        let Some((pending_id, version)) = self.pending.take() else {
            return;
        };
        if pending_id != transition_id {
            warn!("DAVE: executing transition {transition_id}, expected {pending_id}");
            return;
        }
        if version == 0 {
            self.downgraded = true;
            self.protocol_version = 0;
        } else if transition_id > 0 && self.downgraded {
            self.downgraded = false;
            if let Some(session) = &mut self.session {
                session.set_passthrough_mode(true, Some(TRANSITION_EXPIRY));
            }
        }
    }

    /// (Re)creates the session and sends a fresh key package.
    fn reinit(&mut self) -> Vec<Reply> {
        let Some(version) = NonZeroU16::new(self.protocol_version) else {
            if let Some(session) = &mut self.session {
                let _ = session.reset();
                session.set_passthrough_mode(true, None);
            }
            return Vec::new();
        };
        let result = match &mut self.session {
            Some(session) => session
                .reinit(version, self.user_id, self.channel_id, None)
                .map_err(|e| e.to_string()),
            None => DaveSession::new(version, self.user_id, self.channel_id, None)
                .map(|session| self.session = Some(session))
                .map_err(|e| e.to_string()),
        };
        if let Err(error) = result {
            warn!("DAVE init: {error}");
            return Vec::new();
        }
        let session = self.session.as_mut().expect("just created");
        match session.create_key_package() {
            Ok(package) => vec![Reply::Binary(protocol::binary(op::DAVE_KEY_PACKAGE, &[&package]))],
            Err(error) => {
                warn!("DAVE key package: {error}");
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_zero_passes_audio_through() {
        let mut dave = Dave::new(1, 100);
        assert!(dave.start(0).is_empty(), "no key package without DAVE");
        assert!(!dave.is_active());
        assert_eq!(dave.encrypt(b"opus").as_ref(), b"opus");
        assert_eq!(dave.decrypt(2, b"opus").unwrap().as_ref(), b"opus");
    }

    #[test]
    fn starting_sends_a_key_package_and_waits_for_the_group() {
        let mut dave = Dave::new(1, 100);
        let replies = dave.start(1);
        let [Reply::Binary(frame)] = replies.as_slice() else {
            panic!("expected one key package, got {replies:?}");
        };
        assert_eq!(frame[0], op::DAVE_KEY_PACKAGE);
        assert!(frame.len() > 100, "an MLS key package");
        // Not in a group yet: audio still flows unencrypted, silence always does.
        assert!(!dave.is_active());
        assert_eq!(dave.encrypt(b"opus").as_ref(), b"opus");
        assert_eq!(dave.encrypt(&OPUS_SILENCE_PACKET).as_ref(), OPUS_SILENCE_PACKET);

        // A new epoch 1 means a new group: another (different) key package.
        let again = dave.on_prepare_epoch(1, 1);
        assert!(matches!(again.as_slice(), [Reply::Binary(f)] if f != frame));
        assert!(dave.on_prepare_epoch(5, 1).is_empty());
    }

    #[test]
    fn transitions_are_acknowledged_then_executed() {
        let mut dave = Dave::new(1, 100);
        dave.start(1);
        assert_eq!(
            dave.on_prepare_transition(7, 0),
            [Reply::Text(protocol::transition_ready(7))]
        );
        dave.on_execute_transition(7);
        assert!(dave.downgraded);
        assert_eq!(dave.protocol_version, 0);
        // Transition 0 executes at once, with nothing to acknowledge.
        assert!(dave.on_prepare_transition(0, 1).is_empty());
        assert!(dave.pending.is_none());
    }

    // tls_codec debug-asserts on truncated input (it returns an error in
    // release builds, which is what ships), so this runs with --release.
    #[test]
    #[cfg_attr(debug_assertions, ignore)]
    fn bad_mls_messages_are_reported_and_recovered() {
        let mut dave = Dave::new(1, 100);
        dave.start(1);
        let replies = dave.on_binary(op::DAVE_ANNOUNCE_COMMIT, &[0, 9, 0xDE, 0xAD], &[1, 2]);
        assert_eq!(replies[0], Reply::Text(protocol::invalid_commit_welcome(9)));
        assert!(matches!(&replies[1], Reply::Binary(f) if f[0] == op::DAVE_KEY_PACKAGE));
        assert!(dave.on_binary(op::DAVE_EXTERNAL_SENDER, b"junk", &[]).is_empty());
        assert!(dave.on_binary(op::DAVE_PROPOSALS, &[], &[]).is_empty());
        assert!(dave.on_binary(op::DAVE_WELCOME, &[0], &[]).is_empty());
    }
}
