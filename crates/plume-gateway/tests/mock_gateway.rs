//! The gateway against a fake Discord on localhost: login, zlib-stream,
//! heartbeats, reconnect + resume, invalid sessions and bad tokens.

use std::time::Duration;

use flate2::{Compress, Compression, FlushCompress};
use futures_util::{SinkExt, StreamExt};
use plume_core::{Event, Snowflake, Token};
use plume_gateway::{Gateway, GatewayError};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

const WAIT: Duration = Duration::from_secs(10);

/// One client connection to the fake Discord, speaking zlib-stream.
struct Client {
    socket: WebSocketStream<TcpStream>,
    zlib: Compress,
}

impl Client {
    async fn accept(listener: &TcpListener) -> Client {
        let (stream, _) = timeout(WAIT, listener.accept())
            .await
            .expect("client connects")
            .unwrap();
        let socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        Client {
            socket,
            zlib: Compress::new(Compression::fast(), true),
        }
    }

    /// Sends a payload compressed, split over two frames to test reassembly.
    async fn send(&mut self, payload: Value) {
        let text = payload.to_string();
        let start = self.zlib.total_in();
        let mut out = Vec::with_capacity(text.len() + 64);
        loop {
            let done = (self.zlib.total_in() - start) as usize;
            self.zlib
                .compress_vec(&text.as_bytes()[done..], &mut out, FlushCompress::Sync)
                .unwrap();
            if (self.zlib.total_in() - start) as usize == text.len() && out.len() < out.capacity() {
                break;
            }
            out.reserve(1024);
        }
        let (a, b) = out.split_at(out.len() / 2);
        self.socket.send(Message::binary(a.to_vec())).await.unwrap();
        self.socket.send(Message::binary(b.to_vec())).await.unwrap();
    }

    async fn hello(&mut self, interval_ms: u64) {
        self.send(json!({ "op": 10, "d": { "heartbeat_interval": interval_ms } }))
            .await;
    }

    /// The next JSON payload from the client, skipping heartbeats unless asked for.
    async fn recv(&mut self, skip_heartbeats: bool) -> Value {
        loop {
            let message = timeout(WAIT, self.socket.next())
                .await
                .expect("client sends")
                .unwrap()
                .unwrap();
            if let Message::Text(text) = message {
                let value: Value = serde_json::from_str(&text).unwrap();
                if skip_heartbeats && value["op"] == 1 {
                    continue;
                }
                return value;
            }
        }
    }

    async fn dispatch(&mut self, seq: u64, name: &str, data: Value) {
        self.send(json!({ "op": 0, "s": seq, "t": name, "d": data })).await;
    }

    async fn close(&mut self, code: u16) {
        let frame = CloseFrame {
            code: CloseCode::from(code),
            reason: "test".into(),
        };
        let _ = self.socket.close(Some(frame)).await;
    }
}

fn ready(resume_url: &str) -> Value {
    json!({
        "v": 9,
        "user": { "id": "1", "username": "me" },
        "session_id": "session-1",
        "resume_gateway_url": resume_url,
        "guilds": [
            { "id": "10", "properties": { "name": "Server" }, "channels": [{ "id": "100", "type": 0, "name": "general" }] },
            { "id": "11", "unavailable": true }
        ],
        "private_channels": [{ "id": "200", "type": 1, "recipient_ids": ["2"] }],
        "users": [{ "id": "2", "username": "friend" }],
        "read_state": { "entries": [], "version": 1 },
        "some_field_we_dont_know": { "deeply": ["nested", 1, null] }
    })
}

async fn next_event(events: &mut mpsc::Receiver<Event>) -> Event {
    timeout(WAIT, events.recv())
        .await
        .expect("an event arrives")
        .expect("gateway still running")
}

async fn setup() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    (listener, url)
}

#[tokio::test]
async fn identify_ready_messages_and_resume_after_reconnect() {
    let (listener, url) = setup().await;
    let (events_tx, mut events) = mpsc::channel(16);
    let gateway = Gateway::new(Token::new("secret"), events_tx).with_url(&url);
    let handle = gateway.handle();
    let task = tokio::spawn(gateway.run());

    // First connection: HELLO → IDENTIFY → READY.
    let mut client = Client::accept(&listener).await;
    client.hello(45_000).await;
    let identify = client.recv(true).await;
    assert_eq!(identify["op"], 2);
    assert_eq!(identify["d"]["token"], "secret");
    client.dispatch(1, "READY", ready(&url)).await;

    let Event::Ready {
        me,
        guilds,
        channels,
        users,
        ..
    } = next_event(&mut events).await
    else {
        panic!("expected READY");
    };
    assert_eq!(me.username, "me");
    assert_eq!(guilds.len(), 1, "unavailable guilds wait for GUILD_CREATE");
    assert_eq!(channels.len(), 2);
    assert_eq!(users[0].username, "friend");

    // Subscribing asks Discord for typing in that server.
    handle.subscribe_channel(Snowflake(10), Snowflake(100));
    let subscribe = client.recv(true).await;
    assert_eq!(subscribe["op"], 37);
    assert_eq!(subscribe["d"]["subscriptions"]["10"]["typing"], true);

    client
        .dispatch(
            2,
            "MESSAGE_CREATE",
            json!({ "id": "5", "channel_id": "100", "content": "hi", "author": { "id": "2", "username": "friend" } }),
        )
        .await;
    assert!(matches!(next_event(&mut events).await, Event::MessageCreate { message, .. } if message.content == "hi"));

    // Discord asks for a reconnect: the client resumes with the last sequence.
    client.send(json!({ "op": 7, "d": null })).await;
    let mut client = Client::accept(&listener).await;
    client.hello(45_000).await;
    let resume = client.recv(true).await;
    assert_eq!(resume["op"], 6);
    assert_eq!(resume["d"]["session_id"], "session-1");
    assert_eq!(resume["d"]["seq"], 2);
    client.dispatch(3, "RESUMED", json!({})).await;
    assert_eq!(next_event(&mut events).await, Event::Disconnected);
    assert_eq!(next_event(&mut events).await, Event::Resumed);

    // Unknown events and broken event data don't break the connection.
    client.dispatch(4, "SOMETHING_NEW", json!({ "x": 1 })).await;
    assert_eq!(next_event(&mut events).await, Event::Other("SOMETHING_NEW".into()));
    client.dispatch(5, "MESSAGE_CREATE", json!({ "nonsense": true })).await;
    client
        .dispatch(6, "MESSAGE_DELETE", json!({ "id": "5", "channel_id": "100" }))
        .await;
    assert!(matches!(next_event(&mut events).await, Event::MessageDelete { .. }));

    // Dropping the receiver stops the gateway cleanly.
    drop(events);
    let result = timeout(WAIT, task).await.expect("gateway stops").unwrap();
    assert!(result.is_ok());
}

#[tokio::test]
async fn heartbeats_and_zombie_connections() {
    let (listener, url) = setup().await;
    let (events_tx, _events) = mpsc::channel(16);
    let _task = tokio::spawn(Gateway::new(Token::new("t"), events_tx).with_url(&url).run());

    let mut client = Client::accept(&listener).await;
    client.hello(1_000).await;
    assert_eq!(client.recv(true).await["op"], 2);
    client.dispatch(1, "READY", ready(&url)).await;

    // A heartbeat arrives within one interval. The first one may beat READY
    // (the first beat is at a random point), so its sequence may be null.
    let beat = client.recv(false).await;
    assert_eq!(beat["op"], 1);
    assert!(beat["d"] == json!(1) || beat["d"].is_null(), "{beat}");
    client.send(json!({ "op": 11 })).await;
    assert_eq!(
        client.recv(false).await,
        json!({ "op": 1, "d": 1 }),
        "later beats carry the sequence"
    );

    // Discord can ask for a beat right away.
    client.send(json!({ "op": 11 })).await;
    client.send(json!({ "op": 1, "d": null })).await;
    assert_eq!(client.recv(false).await["op"], 1);

    // Never ACK again: the client gives up on this connection and resumes on a new one.
    let mut client = Client::accept(&listener).await;
    client.hello(45_000).await;
    assert_eq!(client.recv(true).await["op"], 6);
}

#[tokio::test]
async fn invalid_session_starts_over_with_identify() {
    let (listener, url) = setup().await;
    let (events_tx, mut events) = mpsc::channel(16);
    let _task = tokio::spawn(Gateway::new(Token::new("t"), events_tx).with_url(&url).run());

    let mut client = Client::accept(&listener).await;
    client.hello(45_000).await;
    client.recv(true).await;
    client.dispatch(1, "READY", ready(&url)).await;
    next_event(&mut events).await;

    client.send(json!({ "op": 9, "d": false })).await;
    let mut client = Client::accept(&listener).await;
    client.hello(45_000).await;
    assert_eq!(client.recv(true).await["op"], 2, "not resumable: identify again");
}

#[tokio::test]
async fn rejected_token_stops_the_gateway() {
    let (listener, url) = setup().await;
    let (events_tx, _events) = mpsc::channel(16);
    let task = tokio::spawn(Gateway::new(Token::new("bad"), events_tx).with_url(&url).run());

    let mut client = Client::accept(&listener).await;
    client.hello(45_000).await;
    client.recv(true).await;
    client.close(4004).await;

    let result = timeout(WAIT, task).await.expect("gateway stops").unwrap();
    assert!(matches!(result, Err(GatewayError::AuthenticationFailed)));
}

#[tokio::test]
async fn dropped_connection_resumes_after_a_short_wait() {
    let (listener, url) = setup().await;
    let (events_tx, mut events) = mpsc::channel(16);
    let _task = tokio::spawn(Gateway::new(Token::new("t"), events_tx).with_url(&url).run());

    let mut client = Client::accept(&listener).await;
    client.hello(45_000).await;
    client.recv(true).await;
    client.dispatch(1, "READY", ready(&url)).await;
    next_event(&mut events).await;

    drop(client); // network drop, no close frame
    let mut client = Client::accept(&listener).await;
    client.hello(45_000).await;
    assert_eq!(client.recv(true).await["op"], 6);
}
