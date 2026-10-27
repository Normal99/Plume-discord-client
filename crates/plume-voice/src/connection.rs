//! One voice connection: the voice gateway, UDP audio, encryption and DAVE,
//! run as a task. The UI talks to it through a [`VoiceHandle`] and hears
//! back through [`VoiceEvent`]s; audio goes in and out as Opus frames.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use plume_core::Snowflake;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep, timeout};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, info, warn};

use crate::crypto::{Crypto, Mode};
use crate::dave::{self, Dave, Reply};
use crate::protocol::{self, Incoming};
use crate::{rtp, udp};

/// Opus "silence", sent a few times after talking so others' decoders
/// don't smear the last word.
const SILENCE: [u8; 3] = [0xF8, 0xFF, 0xFE];
const SILENCE_FRAMES: usize = 5;
/// How long the handshake may take before we give up on a server.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// Keeps the UDP path (and any NAT in between) open while nobody talks.
const KEEPALIVE: Duration = Duration::from_secs(5);
const MAX_RETRIES: u32 = 3;

/// Everything needed to connect, from VOICE_STATE_UPDATE (your session
/// ID) and VOICE_SERVER_UPDATE (endpoint and token).
#[derive(Debug, Clone)]
pub struct ConnectInfo {
    /// `host:port` from Discord, or a full `ws://` URL (tests).
    pub endpoint: String,
    /// The server, or the channel for DM calls.
    pub server_id: Snowflake,
    pub channel_id: Snowflake,
    pub user_id: Snowflake,
    pub session_id: String,
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceEvent {
    /// Audio can flow. `encrypted` is true when DAVE is on.
    Connected {
        encrypted: bool,
    },
    /// DAVE finished (re)keying; compare this code to check the call is private.
    PrivacyCode(String),
    Speaking {
        user_id: Snowflake,
        speaking: bool,
    },
    /// An Opus frame from someone, decrypted.
    Audio {
        user_id: Snowflake,
        opus: Vec<u8>,
    },
    /// The connection ended for good (after retries, or when kicked).
    Disconnected {
        reason: String,
    },
}

enum Command {
    Opus(Vec<u8>),
    StopSpeaking,
    Disconnect,
}

/// Controls a running connection. Dropping every handle disconnects.
#[derive(Clone)]
pub struct VoiceHandle {
    commands: mpsc::Sender<Command>,
}

impl VoiceHandle {
    /// Sends one 20 ms Opus frame. Frames are dropped (not queued up) if
    /// the connection falls behind, so latency never builds.
    pub fn send_opus(&self, frame: Vec<u8>) {
        let _ = self.commands.try_send(Command::Opus(frame));
    }

    /// Call when you stop talking (push-to-talk released, or silence).
    pub fn stop_speaking(&self) {
        let _ = self.commands.try_send(Command::StopSpeaking);
    }

    pub fn disconnect(&self) {
        let _ = self.commands.try_send(Command::Disconnect);
    }
}

/// Starts connecting in the background.
pub fn connect(info: ConnectInfo, events: mpsc::Sender<VoiceEvent>) -> VoiceHandle {
    let (commands, receiver) = mpsc::channel(16);
    tokio::spawn(run(info, events, receiver));
    VoiceHandle { commands }
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("WebSocket: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("bad message from the voice server: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Discovery(#[from] udp::DiscoveryError),
    #[error("UDP: {0}")]
    Io(#[from] std::io::Error),
    #[error("voice server closed the connection ({code}: {reason})")]
    Closed { code: u16, reason: String },
    #[error("{0}")]
    Protocol(String),
}

impl Error {
    fn is_fatal(&self) -> bool {
        match self {
            Error::Closed { code, .. } => protocol::is_fatal_close(*code),
            Error::Protocol(_) => true,
            _ => false,
        }
    }
}

enum End {
    /// We were asked to leave, or nobody's listening any more.
    Stopped,
}

async fn run(info: ConnectInfo, events: mpsc::Sender<VoiceEvent>, mut commands: mpsc::Receiver<Command>) {
    let mut failures = 0;
    loop {
        let started = Instant::now();
        let error = match session(&info, &events, &mut commands).await {
            Ok(End::Stopped) => return,
            Err(error) => error,
        };
        // A connection that lasted a while earns a fresh set of retries.
        if started.elapsed() > Duration::from_secs(60) {
            failures = 0;
        }
        failures += 1;
        if error.is_fatal() || failures > MAX_RETRIES {
            warn!("voice: giving up: {error}");
            let _ = events
                .send(VoiceEvent::Disconnected {
                    reason: error.to_string(),
                })
                .await;
            return;
        }
        warn!("voice: {error}; reconnecting");
        sleep(Duration::from_millis(500 * u64::from(failures))).await;
    }
}

/// The parts of a connection that exist once the server is Ready.
struct Media {
    udp: UdpSocket,
    server: SocketAddr,
    ssrc: u32,
    mode: Mode,
    crypto: Option<Crypto>,
}

async fn session(
    info: &ConnectInfo,
    events: &mpsc::Sender<VoiceEvent>,
    commands: &mut mpsc::Receiver<Command>,
) -> Result<End, Error> {
    let url = if info.endpoint.starts_with("ws://") || info.endpoint.starts_with("wss://") {
        info.endpoint.clone()
    } else {
        format!("wss://{}/?v=8", info.endpoint)
    };
    let (mut socket, _) = timeout(HANDSHAKE_TIMEOUT, connect_async(&url))
        .await
        .map_err(|_| Error::Protocol("timed out connecting to the voice server".into()))??;
    socket
        .send(WsMessage::text(protocol::identify(&protocol::Identify {
            server_id: info.server_id,
            user_id: info.user_id,
            session_id: &info.session_id,
            token: &info.token,
            max_dave_protocol_version: dave::MAX_PROTOCOL_VERSION,
        })))
        .await?;

    let mut seq_ack: Option<u64> = None;
    let mut media: Option<Media> = None;
    let mut dave = Dave::new(info.user_id.0, info.channel_id.0);
    let mut users_by_ssrc: HashMap<u32, Snowflake> = HashMap::new();
    let mut connected: HashSet<u64> = HashSet::from([info.user_id.0]);
    let mut privacy_code = String::new();

    // Heartbeats start once HELLO says how often.
    let far_future = Instant::now() + Duration::from_secs(86_400 * 365);
    let mut heartbeat = interval_at(far_future, Duration::from_secs(86_400));
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut awaiting_ack = false;
    let mut keepalive = interval_at(Instant::now() + KEEPALIVE, KEEPALIVE);
    let handshake_deadline = sleep(HANDSHAKE_TIMEOUT);
    tokio::pin!(handshake_deadline);

    let mut sequence: u16 = 0;
    let mut timestamp: u32 = 0;
    let mut speaking = false;
    let mut packet = Vec::with_capacity(512);
    let mut buffer = vec![0u8; 2048];

    loop {
        let ready = media.as_ref().is_some_and(|m| m.crypto.is_some());
        tokio::select! {
            message = socket.next() => {
                let message = match message {
                    Some(message) => message?,
                    None => return Err(Error::Closed { code: 1006, reason: "connection lost".into() }),
                };
                let (incoming, seq) = match message {
                    WsMessage::Text(text) => protocol::parse_text(&text)?,
                    WsMessage::Binary(frame) => match protocol::parse_binary(&frame) {
                        Some((incoming, seq)) => (incoming, Some(seq)),
                        None => continue,
                    },
                    WsMessage::Close(frame) => {
                        let (code, reason) = frame.map_or((1000, String::new()), |f| (u16::from(f.code), f.reason.to_string()));
                        return Err(Error::Closed { code, reason });
                    }
                    _ => continue,
                };
                if seq.is_some() {
                    seq_ack = seq;
                }
                let mut replies: Vec<Reply> = Vec::new();
                match incoming {
                    Incoming::Hello { heartbeat_interval_ms } => {
                        let period = Duration::from_secs_f64((heartbeat_interval_ms / 1000.0).max(1.0));
                        heartbeat = interval_at(Instant::now() + period, period);
                        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
                    }
                    Incoming::Ready(ready) => {
                        let mode = Mode::choose(ready.modes.iter().map(String::as_str))
                            .ok_or_else(|| Error::Protocol(format!("no supported encryption mode in {:?}", ready.modes)))?;
                        let ip: IpAddr = ready.ip.parse().map_err(|_| Error::Protocol(format!("bad voice IP {}", ready.ip)))?;
                        let server = SocketAddr::new(ip, ready.port);
                        let bind = if server.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
                        let udp = UdpSocket::bind(bind).await?;
                        let (address, port) = udp::discover(&udp, server, ready.ssrc).await?;
                        debug!("voice: our address is {address}:{port}, mode {}", mode.name());
                        socket.send(WsMessage::text(protocol::select_protocol(&address, port, mode.name()))).await?;
                        media = Some(Media { udp, server, ssrc: ready.ssrc, mode, crypto: None });
                    }
                    Incoming::SessionDescription(description) => {
                        let Some(media) = media.as_mut() else {
                            return Err(Error::Protocol("session description before ready".into()));
                        };
                        let mode = Mode::from_name(&description.mode).unwrap_or(media.mode);
                        media.crypto = Some(Crypto::new(mode, &description.secret_key)
                            .map_err(|e| Error::Protocol(e.to_string()))?);
                        replies.extend(dave.start(description.dave_protocol_version));
                        info!("voice connected (DAVE v{})", description.dave_protocol_version);
                        let _ = events.send(VoiceEvent::Connected { encrypted: description.dave_protocol_version > 0 }).await;
                    }
                    Incoming::Speaking { user_id, ssrc, speaking } => {
                        users_by_ssrc.insert(ssrc, user_id);
                        connected.insert(user_id.0);
                        let _ = events.try_send(VoiceEvent::Speaking { user_id, speaking });
                    }
                    Incoming::HeartbeatAck => awaiting_ack = false,
                    Incoming::Resumed => {}
                    Incoming::ClientsConnect(users) => connected.extend(users.iter().map(|u| u.0)),
                    Incoming::ClientDisconnect(user) => {
                        connected.remove(&user.0);
                        users_by_ssrc.retain(|_, u| *u != user);
                    }
                    Incoming::PrepareTransition { transition_id, protocol_version } => {
                        replies.extend(dave.on_prepare_transition(transition_id, protocol_version));
                    }
                    Incoming::ExecuteTransition { transition_id } => dave.on_execute_transition(transition_id),
                    Incoming::PrepareEpoch { epoch, protocol_version } => {
                        replies.extend(dave.on_prepare_epoch(epoch, protocol_version));
                    }
                    Incoming::Dave { op, payload } => {
                        let members: Vec<u64> = connected.iter().copied().collect();
                        replies.extend(dave.on_binary(op, &payload, &members));
                    }
                    Incoming::Other(op) => debug!("voice: ignoring op {op}"),
                }
                for reply in replies {
                    let message = match reply {
                        Reply::Text(text) => WsMessage::text(text),
                        Reply::Binary(bytes) => WsMessage::binary(bytes),
                    };
                    socket.send(message).await?;
                }
                if let Some(code) = dave.privacy_code().filter(|c| *c != privacy_code) {
                    privacy_code = code.to_owned();
                    let _ = events.try_send(VoiceEvent::PrivacyCode(privacy_code.clone()));
                }
            }
            _ = heartbeat.tick() => {
                if awaiting_ack {
                    return Err(Error::Closed { code: 1006, reason: "voice server stopped answering".into() });
                }
                awaiting_ack = true;
                socket.send(WsMessage::text(protocol::heartbeat(now_ms(), seq_ack))).await?;
            }
            _ = &mut handshake_deadline, if !ready => {
                return Err(Error::Closed { code: 1006, reason: "voice handshake timed out".into() });
            }
            _ = keepalive.tick(), if media.is_some() => {
                let media = media.as_ref().expect("checked");
                media.udp.send_to(&udp::request(media.ssrc), media.server).await?;
            }
            received = recv(media.as_ref(), &mut buffer) => {
                let len = received?;
                let packet = &mut buffer[..len];
                if rtp::Header::is_rtcp(packet) {
                    continue;
                }
                let Some(header) = rtp::Header::parse(packet) else { continue };
                let Some(&user_id) = users_by_ssrc.get(&header.ssrc) else { continue };
                let Some(crypto) = media.as_ref().and_then(|m| m.crypto.as_ref()) else { continue };
                let Ok(opus) = crypto.decrypt(packet) else { continue };
                if let Some(opus) = dave.decrypt(user_id.0, opus) {
                    // A full queue means the UI is busy: drop audio rather than lag.
                    let _ = events.try_send(VoiceEvent::Audio { user_id, opus: opus.into_owned() });
                }
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    let _ = socket.close(None).await;
                    return Ok(End::Stopped);
                };
                let Some(media) = media.as_mut().filter(|_| ready) else {
                    if matches!(command, Command::Disconnect) {
                        let _ = socket.close(None).await;
                        return Ok(End::Stopped);
                    }
                    continue;
                };
                let frames: Vec<Vec<u8>> = match command {
                    Command::Opus(frame) => {
                        if !speaking {
                            speaking = true;
                            socket.send(WsMessage::text(protocol::speaking(media.ssrc, true))).await?;
                        }
                        vec![frame]
                    }
                    Command::StopSpeaking if speaking => {
                        speaking = false;
                        socket.send(WsMessage::text(protocol::speaking(media.ssrc, false))).await?;
                        vec![SILENCE.to_vec(); SILENCE_FRAMES]
                    }
                    Command::StopSpeaking => Vec::new(),
                    Command::Disconnect => {
                        let _ = socket.close(None).await;
                        return Ok(End::Stopped);
                    }
                };
                let crypto = media.crypto.as_mut().expect("ready");
                for frame in frames {
                    let header = rtp::Header::outgoing(sequence, timestamp, media.ssrc).to_bytes();
                    sequence = sequence.wrapping_add(1);
                    timestamp = timestamp.wrapping_add(rtp::SAMPLES_PER_FRAME);
                    let payload = dave.encrypt(&frame);
                    if crypto.encrypt(&header, &payload, &mut packet).is_ok() {
                        media.udp.send_to(&packet, media.server).await?;
                    }
                }
            }
        }
    }
}

/// Waits for a UDP packet, or forever if there's no socket yet.
async fn recv(media: Option<&Media>, buffer: &mut [u8]) -> std::io::Result<usize> {
    match media {
        Some(media) => media.udp.recv(buffer).await,
        None => std::future::pending().await,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
