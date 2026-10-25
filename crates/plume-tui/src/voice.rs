//! Voice calls from the TUI: joining through the gateway, connecting to
//! the voice server once Discord says where, and the microphone/speakers.
//!
//! Without the `voice` feature this still tracks what you asked for, so
//! the UI can say the build has no voice instead of silently doing nothing.

use std::collections::HashSet;

use plume_core::{Event, Snowflake, Store};
use plume_gateway::{GatewayHandle, VoiceRequest};
use tokio::sync::mpsc;

#[cfg(feature = "voice")]
pub use plume_voice::VoiceEvent;

/// Without voice support there are no voice events.
#[cfg(not(feature = "voice"))]
pub enum VoiceEvent {}

/// Where voice events arrive; never ready without the `voice` feature.
pub struct VoiceEvents(#[allow(dead_code)] mpsc::Receiver<VoiceEvent>);

impl VoiceEvents {
    pub async fn recv(&mut self) -> Option<VoiceEvent> {
        #[cfg(feature = "voice")]
        return self.0.recv().await;
        #[cfg(not(feature = "voice"))]
        std::future::pending().await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(feature = "voice"), allow(dead_code))]
pub enum Status {
    /// Asked Discord to join; waiting for where to connect.
    Joining,
    Connecting,
    Connected {
        encrypted: bool,
    },
}

pub struct Call {
    pub guild_id: Option<Snowflake>,
    pub channel_id: Snowflake,
    pub status: Status,
    /// Who's talking right now.
    pub speaking: HashSet<Snowflake>,
    pub privacy_code: Option<String>,
    session_id: Option<String>,
    server: Option<(String, String)>,
    #[cfg(feature = "voice")]
    connection: Option<plume_voice::VoiceHandle>,
    #[cfg(feature = "voice")]
    audio: Option<plume_voice::audio::AudioEngine>,
}

pub struct Voice {
    pub call: Option<Call>,
    pub muted: bool,
    pub deafened: bool,
    #[allow(dead_code)]
    events: mpsc::Sender<VoiceEvent>,
}

pub fn new() -> (Voice, VoiceEvents) {
    let (events, receiver) = mpsc::channel(256);
    let voice = Voice {
        call: None,
        muted: false,
        deafened: false,
        events,
    };
    (voice, VoiceEvents(receiver))
}

/// Whether this build can make calls.
pub const SUPPORTED: bool = cfg!(feature = "voice");

impl Voice {
    /// Joins a voice channel or DM call (or moves there from another).
    pub fn join(&mut self, gateway: &GatewayHandle, store: &Store, channel_id: Snowflake) -> Result<(), String> {
        if !SUPPORTED {
            return Err("This build has no voice support (it was built without the `voice` feature)".into());
        }
        if self.call.as_ref().is_some_and(|c| c.channel_id == channel_id) {
            return Ok(());
        }
        let guild_id = store.channel(channel_id).and_then(|c| c.guild_id);
        self.disconnect_media();
        self.call = Some(Call {
            guild_id,
            channel_id,
            status: Status::Joining,
            speaking: HashSet::new(),
            privacy_code: None,
            session_id: None,
            server: None,
            #[cfg(feature = "voice")]
            connection: None,
            #[cfg(feature = "voice")]
            audio: None,
        });
        self.send_state(gateway);
        Ok(())
    }

    pub fn leave(&mut self, gateway: &GatewayHandle) {
        let Some(call) = self.call.as_ref() else { return };
        gateway.update_voice(VoiceRequest {
            guild_id: call.guild_id,
            channel_id: None,
            self_mute: self.muted,
            self_deaf: self.deafened,
        });
        self.disconnect_media();
        self.call = None;
    }

    pub fn toggle_mute(&mut self, gateway: &GatewayHandle) {
        self.muted = !self.muted;
        // Unmuting while deafened undeafens too, like Discord.
        if !self.muted {
            self.deafened = false;
        }
        self.apply_switches(gateway);
    }

    pub fn toggle_deafen(&mut self, gateway: &GatewayHandle) {
        self.deafened = !self.deafened;
        self.muted = self.deafened || self.muted;
        self.apply_switches(gateway);
    }

    /// Picks our session and the voice server out of gateway events.
    /// Returns a message to show if the call ended.
    pub fn on_gateway(&mut self, event: &Event, me: Option<Snowflake>, gateway: &GatewayHandle) -> Option<String> {
        let call = self.call.as_mut()?;
        match event {
            Event::VoiceStateUpdate { state, left } if Some(state.user_id) == me => {
                if *left {
                    if call.status != Status::Joining {
                        self.disconnect_media();
                        self.call = None;
                        return Some("You left the voice channel".into());
                    }
                    return None;
                }
                // A moderator may have moved us.
                call.channel_id = state.channel_id;
                call.guild_id = state.guild_id;
                call.session_id = Some(state.session_id.clone());
            }
            Event::VoiceServerUpdate {
                guild_id,
                channel_id,
                endpoint,
                token,
            } => {
                let ours = match guild_id {
                    Some(guild) => call.guild_id == Some(*guild),
                    None => *channel_id == Some(call.channel_id),
                };
                if !ours {
                    return None;
                }
                let Some(endpoint) = endpoint else {
                    // Discord is moving the call to another server; a new update follows.
                    return None;
                };
                call.server = Some((endpoint.clone(), token.clone()));
                // A new server means a new connection.
                self.disconnect_media();
            }
            Event::Ready { .. } => {
                // A new gateway session rejoins (the gateway re-sends our
                // voice state); the old voice session is gone.
                self.disconnect_media();
                if let Some(call) = self.call.as_mut() {
                    call.session_id = None;
                    call.server = None;
                }
                let _ = gateway;
                return None;
            }
            _ => return None,
        }
        self.try_connect(me)
    }

    /// Handles something from the voice connection. Returns a message to show.
    #[cfg(feature = "voice")]
    pub fn on_voice(&mut self, event: VoiceEvent, gateway: &GatewayHandle) -> Option<String> {
        let call = self.call.as_mut()?;
        match event {
            VoiceEvent::Connected { encrypted } => call.status = Status::Connected { encrypted },
            VoiceEvent::PrivacyCode(code) => call.privacy_code = Some(code),
            VoiceEvent::Speaking { user_id, speaking } => {
                if speaking {
                    call.speaking.insert(user_id);
                } else {
                    call.speaking.remove(&user_id);
                }
            }
            VoiceEvent::Audio { user_id, opus } => {
                if let Some(audio) = &call.audio {
                    audio.play(user_id, opus);
                }
            }
            VoiceEvent::Disconnected { reason } => {
                self.leave(gateway);
                return Some(format!("Voice disconnected: {reason}"));
            }
        }
        None
    }

    #[cfg(not(feature = "voice"))]
    pub fn on_voice(&mut self, event: VoiceEvent, _gateway: &GatewayHandle) -> Option<String> {
        match event {}
    }

    /// Whether we're sending audio right now.
    pub fn talking(&self) -> bool {
        #[cfg(feature = "voice")]
        return self
            .call
            .as_ref()
            .and_then(|c| c.audio.as_ref())
            .is_some_and(|a| a.is_talking());
        #[cfg(not(feature = "voice"))]
        false
    }

    #[cfg(feature = "voice")]
    fn try_connect(&mut self, me: Option<Snowflake>) -> Option<String> {
        let call = self.call.as_mut()?;
        if call.connection.is_some() {
            return None;
        }
        let (Some(session_id), Some((endpoint, token)), Some(me)) = (&call.session_id, &call.server, me) else {
            return None;
        };
        let info = plume_voice::ConnectInfo {
            endpoint: endpoint.clone(),
            server_id: call.guild_id.unwrap_or(call.channel_id),
            channel_id: call.channel_id,
            user_id: me,
            session_id: session_id.clone(),
            token: token.clone(),
        };
        let connection = plume_voice::connect(info, self.events.clone());
        call.status = Status::Connecting;
        let message = match plume_voice::audio::AudioEngine::start(connection.clone()) {
            Ok(audio) => {
                audio.set_muted(self.muted);
                audio.set_deafened(self.deafened);
                let message = (!audio.has_microphone).then(|| "No microphone found: you can listen only".to_owned());
                call.audio = Some(audio);
                message
            }
            Err(error) => Some(format!("Audio: {error}")),
        };
        call.connection = Some(connection);
        message
    }

    #[cfg(not(feature = "voice"))]
    fn try_connect(&mut self, _me: Option<Snowflake>) -> Option<String> {
        None
    }

    fn disconnect_media(&mut self) {
        #[cfg(feature = "voice")]
        if let Some(call) = self.call.as_mut() {
            call.audio = None;
            if let Some(connection) = call.connection.take() {
                connection.disconnect();
            }
            call.speaking.clear();
            call.privacy_code = None;
            if call.status != Status::Joining {
                call.status = Status::Joining;
            }
        }
    }

    fn apply_switches(&mut self, gateway: &GatewayHandle) {
        #[cfg(feature = "voice")]
        if let Some(audio) = self.call.as_ref().and_then(|c| c.audio.as_ref()) {
            audio.set_muted(self.muted);
            audio.set_deafened(self.deafened);
        }
        if self.call.is_some() {
            self.send_state(gateway);
        }
    }

    fn send_state(&self, gateway: &GatewayHandle) {
        let Some(call) = &self.call else { return };
        gateway.update_voice(VoiceRequest {
            guild_id: call.guild_id,
            channel_id: Some(call.channel_id),
            self_mute: self.muted,
            self_deaf: self.deafened,
        });
    }
}
