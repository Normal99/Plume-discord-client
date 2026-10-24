use std::collections::BTreeSet;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use plume_core::identity::{BROWSER_VERSION, OS, USER_AGENT};
use plume_core::{Event, Snowflake, Token};
use serde::Deserialize;
use serde_json::json;
use serde_json::value::RawValue;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::sleep_until;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::{debug, info, warn};

use crate::compression::Inflater;
use crate::dispatch;
use crate::error::GatewayError;
use crate::heartbeat::{Beat, Heartbeat, jitter};
use crate::opcodes::{self, CloseAction};
use crate::resume::ResumeState;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Requests from the UI to the connection.
#[derive(Debug)]
enum Command {
    /// Ask for typing indicators (and threads) in a server.
    SubscribeGuild(Snowflake),
}

/// Sends requests to a running [`Gateway`]. Cheap to clone. If the gateway
/// has stopped, requests are silently dropped.
#[derive(Debug, Clone)]
pub struct GatewayHandle {
    commands: mpsc::UnboundedSender<Command>,
}

impl GatewayHandle {
    /// Called when a channel is opened. Discord only sends typing events for
    /// servers the client says it's looking at. Repeats are free.
    pub fn subscribe_channel(&self, guild_id: Snowflake, _channel_id: Snowflake) {
        let _ = self.commands.send(Command::SubscribeGuild(guild_id));
    }
}

/// The connection to Discord's gateway. Create it, keep a [`handle`], then
/// spawn [`run`].
///
/// [`handle`]: Gateway::handle
/// [`run`]: Gateway::run
pub struct Gateway {
    token: Token,
    url: String,
    events: mpsc::Sender<Event>,
    commands: mpsc::UnboundedReceiver<Command>,
    commands_tx: mpsc::UnboundedSender<Command>,
    resume: Option<ResumeState>,
    /// The last sequence number seen, for heartbeats and RESUME.
    seq: Option<u64>,
    /// Servers we've asked for typing events in; re-sent after a new READY.
    subscribed: BTreeSet<Snowflake>,
    /// The logged-in user, from READY.
    me: Option<Snowflake>,
}

/// How a connection ended.
enum Outcome {
    /// Nobody is listening for events any more.
    Shutdown,
    Reconnect {
        resume: bool,
        /// Discord asked us to (op 7), so there's no need to back off.
        immediately: bool,
    },
}

#[derive(Deserialize)]
struct Payload<'a> {
    op: u8,
    #[serde(borrow, default)]
    d: Option<&'a RawValue>,
    #[serde(default)]
    s: Option<u64>,
    #[serde(default)]
    t: Option<&'a str>,
}

#[derive(Deserialize)]
struct Hello {
    heartbeat_interval: u64,
}

impl Gateway {
    pub fn new(token: Token, events: mpsc::Sender<Event>) -> Self {
        let (commands_tx, commands) = mpsc::unbounded_channel();
        Gateway {
            token,
            url: opcodes::GATEWAY_URL.to_owned(),
            events,
            commands,
            commands_tx,
            resume: None,
            seq: None,
            subscribed: BTreeSet::new(),
            me: None,
        }
    }

    /// Connects somewhere other than Discord: a local test server, say.
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }

    pub fn handle(&self) -> GatewayHandle {
        GatewayHandle {
            commands: self.commands_tx.clone(),
        }
    }

    /// Connects and keeps the connection alive until the event receiver is
    /// dropped (returns `Ok`) or reconnecting can't help (returns the error).
    /// Network trouble is never an error here: it just reconnects.
    pub async fn run(mut self) -> Result<(), GatewayError> {
        // Same TLS backend as the REST client. An error means it's installed already.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut backoff = MIN_BACKOFF;

        loop {
            if self.events.is_closed() {
                return Ok(());
            }
            let (outcome, healthy) = self.connect_once().await;
            if healthy {
                backoff = MIN_BACKOFF;
            }
            let delay = match outcome {
                Ok(Outcome::Shutdown) => return Ok(()),
                Ok(Outcome::Reconnect { resume, immediately }) => {
                    if !resume {
                        self.resume = None;
                        self.seq = None;
                    }
                    if immediately { Duration::ZERO } else { backoff }
                }
                Err(error @ (GatewayError::AuthenticationFailed | GatewayError::Fatal { .. })) => return Err(error),
                Err(error) => {
                    warn!("gateway connection lost: {error}");
                    backoff
                }
            };
            // Let the UI show that we're offline. If the queue is full it's
            // busy anyway, and READY/RESUMED will follow.
            let _ = self.events.try_send(Event::Disconnected);
            if !delay.is_zero() {
                // Up to 50% extra, so many clients don't reconnect in lockstep.
                let delay = delay.mul_f64(1.0 + jitter() / 2.0);
                info!("reconnecting in {delay:.1?}");
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = self.events.closed() => return Ok(()),
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }

    /// One connection, from connect to close. Also says whether it got as
    /// far as READY/RESUMED, which resets the backoff.
    async fn connect_once(&mut self) -> (Result<Outcome, GatewayError>, bool) {
        let mut healthy = false;
        let outcome = self.connection(&mut healthy).await;
        (outcome, healthy)
    }

    async fn connection(&mut self, healthy: &mut bool) -> Result<Outcome, GatewayError> {
        let url = match &self.resume {
            Some(resume) => format!("{}{}", resume.resume_url.trim_end_matches('/'), opcodes::GATEWAY_QUERY),
            None => format!("{}{}", self.url.trim_end_matches('/'), opcodes::GATEWAY_QUERY),
        };
        debug!("connecting to {url}");
        let (mut socket, _) = tokio_tungstenite::connect_async(url.as_str()).await?;
        let mut inflater = Inflater::default();
        let mut heartbeat = Heartbeat::idle();

        loop {
            let frame = tokio::select! {
                frame = socket.next() => frame,
                _ = sleep_until(heartbeat.next()) => {
                    match heartbeat.due() {
                        Beat::Send => self.send_heartbeat(&mut socket).await?,
                        Beat::Zombie => {
                            warn!("no heartbeat ACK from Discord; reconnecting");
                            return Ok(Outcome::Reconnect { resume: true, immediately: false });
                        }
                    }
                    continue;
                }
                Some(command) = self.commands.recv() => {
                    self.on_command(&mut socket, command, *healthy).await?;
                    continue;
                }
                _ = self.events.closed() => {
                    let _ = socket.close(None).await;
                    return Ok(Outcome::Shutdown);
                }
            };

            let frame = match frame {
                Some(Ok(frame)) => frame,
                Some(Err(error)) => return Err(error.into()),
                None => {
                    return Ok(Outcome::Reconnect {
                        resume: true,
                        immediately: false,
                    });
                }
            };
            let text: &[u8] = match &frame {
                WsMessage::Binary(bytes) => match inflater.push(bytes)? {
                    Some(text) => text,
                    None => continue, // the rest is in the next frame
                },
                WsMessage::Text(text) => text.as_bytes(),
                WsMessage::Close(close) => return close_outcome(close.as_ref()),
                _ => continue, // pings are answered by tungstenite
            };

            let payload: Payload = serde_json::from_slice(text)?;
            if let Some(seq) = payload.s {
                self.seq = Some(seq);
            }
            match payload.op {
                opcodes::DISPATCH => {
                    let (Some(name), Some(data)) = (payload.t, payload.d) else {
                        continue;
                    };
                    if name == "READY" || name == "RESUMED" {
                        *healthy = true;
                    }
                    let decoded = match dispatch::decode(name, data, self.me) {
                        Ok(decoded) => decoded,
                        Err(error) if name == "READY" => return Err(error.into()),
                        Err(error) => {
                            // One odd event shouldn't take the connection down.
                            warn!("couldn't decode {name}: {error}");
                            continue;
                        }
                    };
                    let is_ready = decoded.session.is_some();
                    if let Event::Ready { me, .. } = &decoded.event {
                        self.me = Some(me.id);
                    }
                    if let Some(session) = decoded.session {
                        info!("READY ({} bytes)", data.get().len());
                        self.resume = Some(session);
                    }
                    if self.events.send(decoded.event).await.is_err() {
                        return Ok(Outcome::Shutdown);
                    }
                    if is_ready {
                        // A new session forgets subscriptions, so ask again.
                        let guilds: Vec<Snowflake> = self.subscribed.iter().copied().collect();
                        self.send_subscriptions(&mut socket, &guilds).await?;
                    }
                }
                opcodes::HEARTBEAT => {
                    heartbeat.sent();
                    self.send_heartbeat(&mut socket).await?;
                }
                opcodes::HEARTBEAT_ACK => heartbeat.acked(),
                opcodes::RECONNECT => {
                    info!("Discord asked us to reconnect");
                    return Ok(Outcome::Reconnect {
                        resume: true,
                        immediately: true,
                    });
                }
                opcodes::INVALID_SESSION => {
                    let resumable = payload.d.is_some_and(|d| d.get() == "true");
                    info!("session invalidated (resumable: {resumable})");
                    return Ok(Outcome::Reconnect {
                        resume: resumable,
                        immediately: false,
                    });
                }
                opcodes::HELLO => {
                    let hello: Hello = serde_json::from_str(payload.d.map_or("null", |d| d.get()))?;
                    heartbeat = Heartbeat::start(Duration::from_millis(hello.heartbeat_interval));
                    self.send_login(&mut socket).await?;
                }
                other => debug!("ignoring gateway op {other}"),
            }
        }
    }

    async fn send_login(&self, socket: &mut Socket) -> Result<(), GatewayError> {
        let payload = match &self.resume {
            Some(resume) => json!({
                "op": opcodes::RESUME,
                "d": {
                    "token": self.token.expose(),
                    "session_id": resume.session_id,
                    "seq": self.seq,
                },
            }),
            None => identify_payload(&self.token),
        };
        // Not logged: it contains the token.
        socket.send(WsMessage::text(payload.to_string())).await?;
        Ok(())
    }

    async fn send_heartbeat(&self, socket: &mut Socket) -> Result<(), GatewayError> {
        let payload = json!({ "op": opcodes::HEARTBEAT, "d": self.seq });
        socket.send(WsMessage::text(payload.to_string())).await?;
        Ok(())
    }

    async fn on_command(&mut self, socket: &mut Socket, command: Command, ready: bool) -> Result<(), GatewayError> {
        match command {
            Command::SubscribeGuild(guild_id) => {
                // If we're not logged in yet, READY will send it.
                if self.subscribed.insert(guild_id) && ready {
                    self.send_subscriptions(socket, &[guild_id]).await?;
                }
            }
        }
        Ok(())
    }

    async fn send_subscriptions(&self, socket: &mut Socket, guilds: &[Snowflake]) -> Result<(), GatewayError> {
        if guilds.is_empty() {
            return Ok(());
        }
        // No member list ranges: those stream member updates we'd never show.
        let subscriptions: serde_json::Map<String, serde_json::Value> = guilds
            .iter()
            .map(|id| {
                let options = json!({ "typing": true, "threads": true, "activities": false, "member_updates": false });
                (id.to_string(), options)
            })
            .collect();
        let payload = json!({ "op": opcodes::GUILD_SUBSCRIPTIONS_BULK, "d": { "subscriptions": subscriptions } });
        socket.send(WsMessage::text(payload.to_string())).await?;
        Ok(())
    }
}

/// IDENTIFY, shaped like the desktop web client's.
fn identify_payload(token: &Token) -> serde_json::Value {
    json!({
        "op": opcodes::IDENTIFY,
        "d": {
            "token": token.expose(),
            "capabilities": opcodes::CAPABILITIES,
            "properties": {
                "os": OS,
                "browser": "Chrome",
                "device": "",
                "system_locale": "en-US",
                "browser_user_agent": USER_AGENT,
                "browser_version": BROWSER_VERSION,
                "os_version": "",
                "referrer": "",
                "referring_domain": "",
                "referrer_current": "",
                "referring_domain_current": "",
                "release_channel": "stable",
                "client_event_source": null,
            },
            "presence": { "status": "unknown", "since": 0, "activities": [], "afk": false },
            "compress": false,
            "client_state": { "guild_versions": {} },
        },
    })
}

fn close_outcome(close: Option<&CloseFrame>) -> Result<Outcome, GatewayError> {
    let Some(close) = close else {
        return Ok(Outcome::Reconnect {
            resume: true,
            immediately: false,
        });
    };
    let code = u16::from(close.code);
    info!("gateway closed: {code} {}", close.reason);
    match opcodes::close_action(code) {
        CloseAction::Resume => Ok(Outcome::Reconnect {
            resume: true,
            immediately: false,
        }),
        CloseAction::Reidentify => Ok(Outcome::Reconnect {
            resume: false,
            immediately: false,
        }),
        CloseAction::AuthFailed => Err(GatewayError::AuthenticationFailed),
        CloseAction::Fatal => Err(GatewayError::Fatal {
            code,
            reason: close.reason.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identify_has_token_capabilities_and_properties() {
        let payload = identify_payload(&Token::new("abc"));
        assert_eq!(payload["op"], 2);
        assert_eq!(payload["d"]["token"], "abc");
        assert_eq!(payload["d"]["capabilities"], opcodes::CAPABILITIES);
        assert_eq!(payload["d"]["properties"]["browser_user_agent"], USER_AGENT);
    }

    #[test]
    fn close_codes_decide_what_happens_next() {
        use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
        let frame = |code: u16| CloseFrame {
            code: CloseCode::from(code),
            reason: "".into(),
        };
        assert!(matches!(
            close_outcome(Some(&frame(4004))),
            Err(GatewayError::AuthenticationFailed)
        ));
        assert!(matches!(
            close_outcome(Some(&frame(4014))),
            Err(GatewayError::Fatal { code: 4014, .. })
        ));
        assert!(matches!(
            close_outcome(Some(&frame(4009))),
            Ok(Outcome::Reconnect { resume: false, .. })
        ));
        assert!(matches!(
            close_outcome(Some(&frame(1006))),
            Ok(Outcome::Reconnect { resume: true, .. })
        ));
    }

    #[test]
    fn payloads_borrow_their_data() {
        let payload: Payload = serde_json::from_str(r#"{"op":0,"s":5,"t":"RESUMED","d":{"x":1}}"#).unwrap();
        assert_eq!(payload.t, Some("RESUMED"));
        assert_eq!(payload.d.unwrap().get(), r#"{"x":1}"#);
        let hello: Payload =
            serde_json::from_str(r#"{"op":10,"d":{"heartbeat_interval":41250},"s":null,"t":null}"#).unwrap();
        assert_eq!(hello.op, opcodes::HELLO);
    }

    #[tokio::test]
    async fn handle_is_harmless_after_the_gateway_is_gone() {
        let (events, _) = mpsc::channel(1);
        let handle = Gateway::new(Token::new("x"), events).handle();
        handle.subscribe_channel(Snowflake(1), Snowflake(2)); // must not panic
    }

    #[tokio::test]
    async fn run_returns_once_nobody_listens() {
        let (events, receiver) = mpsc::channel(1);
        drop(receiver);
        let gateway = Gateway::new(Token::new("x"), events);
        // Connecting fails or succeeds; either way it notices the closed
        // channel instead of retrying forever.
        let result = tokio::time::timeout(Duration::from_secs(30), gateway.run()).await;
        assert!(matches!(result, Ok(Ok(()))));
    }
}
