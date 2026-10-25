//! A voice connection against a fake voice server on localhost: the
//! handshake, IP discovery, audio both ways, heartbeats, DAVE's key
//! package, reconnecting and being kicked.

use std::net::SocketAddr;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use plume_core::Snowflake;
use plume_voice::crypto::{Crypto, Mode};
use plume_voice::{ConnectInfo, VoiceEvent, connect, rtp, udp};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

const WAIT: Duration = Duration::from_secs(10);
const KEY: [u8; 32] = [3; 32];
const CLIENT_SSRC: u32 = 42;
const FRIEND_SSRC: u32 = 99;

struct Server {
    listener: TcpListener,
    url: String,
    udp_port: u16,
    /// RTP packets the client sent (IP discovery is answered automatically).
    packets: mpsc::UnboundedReceiver<(Vec<u8>, SocketAddr)>,
    udp: std::sync::Arc<UdpSocket>,
}

impl Server {
    async fn start() -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let udp = std::sync::Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let udp_port = udp.local_addr().unwrap().port();
        let (tx, packets) = mpsc::unbounded_channel();
        let socket = udp.clone();
        tokio::spawn(async move {
            let mut buffer = [0u8; 2048];
            while let Ok((len, from)) = socket.recv_from(&mut buffer).await {
                let packet = &buffer[..len];
                if packet.len() == 74 && packet[..2] == [0, 1] {
                    let ssrc = u32::from_be_bytes(packet[4..8].try_into().unwrap());
                    let _ = socket.send_to(&udp::reply(ssrc, "127.0.0.1", from.port()), from).await;
                } else {
                    let _ = tx.send((packet.to_vec(), from));
                }
            }
        });
        Server {
            listener,
            url,
            udp_port,
            packets,
            udp,
        }
    }

    async fn accept(&self) -> Client {
        let (stream, _) = timeout(WAIT, self.listener.accept()).await.unwrap().unwrap();
        Client {
            socket: tokio_tungstenite::accept_async(stream).await.unwrap(),
            seq: 0,
        }
    }

    async fn next_packet(&mut self) -> (Vec<u8>, SocketAddr) {
        timeout(WAIT, self.packets.recv()).await.expect("a packet").unwrap()
    }
}

struct Client {
    socket: WebSocketStream<TcpStream>,
    seq: u64,
}

impl Client {
    async fn send(&mut self, op: u8, d: Value) {
        self.seq += 1;
        let text = json!({ "op": op, "d": d, "seq": self.seq }).to_string();
        self.socket.send(Message::text(text)).await.unwrap();
    }

    /// The next message from the client, answering heartbeats on the way.
    async fn recv(&mut self) -> Message {
        loop {
            let message = timeout(WAIT, self.socket.next())
                .await
                .expect("a message")
                .unwrap()
                .unwrap();
            if let Message::Text(text) = &message {
                let value: Value = serde_json::from_str(text).unwrap();
                if value["op"] == 3 {
                    self.send(6, json!({ "t": value["d"]["t"] })).await;
                    continue;
                }
            }
            return message;
        }
    }

    async fn recv_json(&mut self) -> Value {
        match self.recv().await {
            Message::Text(text) => serde_json::from_str(&text).unwrap(),
            other => panic!("expected JSON, got {other:?}"),
        }
    }

    /// HELLO, READY and the protocol handshake, ending with a session
    /// description for the given DAVE version.
    async fn handshake(&mut self, udp_port: u16, dave_version: u16) -> Value {
        let identify = self.recv_json().await;
        assert_eq!(identify["op"], 0);
        self.send(8, json!({ "heartbeat_interval": 1000.0 })).await;
        self.send(
            2,
            json!({ "ssrc": CLIENT_SSRC, "ip": "127.0.0.1", "port": udp_port,
                    "modes": ["xsalsa20_poly1305", "aead_xchacha20_poly1305_rtpsize"] }),
        )
        .await;
        let select = self.recv_json().await;
        assert_eq!(select["op"], 1);
        assert_eq!(select["d"]["data"]["mode"], "aead_xchacha20_poly1305_rtpsize");
        assert_eq!(select["d"]["data"]["address"], "127.0.0.1");
        self.send(
            4,
            json!({ "mode": "aead_xchacha20_poly1305_rtpsize", "secret_key": KEY, "dave_protocol_version": dave_version }),
        )
        .await;
        identify
    }
}

fn info(url: &str) -> ConnectInfo {
    ConnectInfo {
        endpoint: url.to_owned(),
        server_id: Snowflake(10),
        channel_id: Snowflake(150),
        user_id: Snowflake(1),
        session_id: "voice-session".into(),
        token: "voice-token".into(),
    }
}

async fn next_event(events: &mut mpsc::Receiver<VoiceEvent>) -> VoiceEvent {
    timeout(WAIT, events.recv())
        .await
        .expect("an event")
        .expect("still running")
}

#[tokio::test]
async fn handshake_audio_both_ways_and_kick() {
    let mut server = Server::start().await;
    let (events_tx, mut events) = mpsc::channel(64);
    let handle = connect(info(&server.url), events_tx);

    let mut client = server.accept().await;
    let identify = client.handshake(server.udp_port, 0).await;
    assert_eq!(identify["d"]["server_id"], "10");
    assert_eq!(identify["d"]["session_id"], "voice-session");
    assert_eq!(identify["d"]["token"], "voice-token");
    assert_eq!(identify["d"]["max_dave_protocol_version"], 1);
    assert_eq!(
        next_event(&mut events).await,
        VoiceEvent::Connected { encrypted: false }
    );

    // A friend starts talking: their SSRC is mapped to them, and their
    // audio arrives decrypted (with Discord's header extension removed).
    client
        .send(5, json!({ "user_id": "7", "ssrc": FRIEND_SSRC, "speaking": 1 }))
        .await;
    assert_eq!(
        next_event(&mut events).await,
        VoiceEvent::Speaking {
            user_id: Snowflake(7),
            speaking: true
        }
    );
    let (_, client_addr) = {
        // Learn the client's UDP address from its first keepalive or audio.
        handle.send_opus(b"hello".to_vec());
        server.next_packet().await
    };
    let mut header = rtp::Header::outgoing(1, 960, FRIEND_SSRC).to_bytes().to_vec();
    header[0] |= 0x10;
    header.extend_from_slice(&[0xBE, 0xDE, 0, 1]);
    let mut payload = vec![0x10, 0x01, 0, 0];
    payload.extend_from_slice(b"from friend");
    let mut packet = Vec::new();
    Crypto::new(Mode::XChaCha20Poly1305, &KEY)
        .unwrap()
        .encrypt(&header, &payload, &mut packet)
        .unwrap();
    server.udp.send_to(&packet, client_addr).await.unwrap();
    assert_eq!(
        next_event(&mut events).await,
        VoiceEvent::Audio {
            user_id: Snowflake(7),
            opus: b"from friend".to_vec()
        }
    );

    // Our audio: SPEAKING first, then encrypted RTP from our SSRC.
    let speaking = client.recv_json().await;
    assert_eq!(
        (speaking["op"].as_u64(), speaking["d"]["speaking"].as_u64()),
        (Some(5), Some(1))
    );
    assert_eq!(speaking["d"]["ssrc"], CLIENT_SSRC);
    handle.send_opus(b"second".to_vec());
    let receiver = Crypto::new(Mode::XChaCha20Poly1305, &KEY).unwrap();
    let (mut packet, _) = server.next_packet().await;
    let header = rtp::Header::parse(&packet).unwrap();
    assert_eq!((header.ssrc, header.sequence, header.timestamp), (CLIENT_SSRC, 1, 960));
    assert_eq!(receiver.decrypt(&mut packet).unwrap(), b"second");

    // Stopping sends SPEAKING 0 and a few silence frames.
    handle.stop_speaking();
    let stopped = client.recv_json().await;
    assert_eq!(stopped["d"]["speaking"], 0);
    for _ in 0..5 {
        let (mut packet, _) = server.next_packet().await;
        assert_eq!(receiver.decrypt(&mut packet).unwrap(), [0xF8, 0xFF, 0xFE]);
    }

    // Kicked from the channel (4014): no reconnect, just an event.
    client
        .socket
        .close(Some(CloseFrame {
            code: CloseCode::from(4014),
            reason: "Disconnected".into(),
        }))
        .await
        .unwrap();
    assert!(matches!(next_event(&mut events).await, VoiceEvent::Disconnected { reason } if reason.contains("4014")));
}

#[tokio::test]
async fn dave_sends_a_key_package_and_drops_reconnect() {
    let server = Server::start().await;
    let (events_tx, mut events) = mpsc::channel(64);
    let handle = connect(info(&server.url), events_tx);

    let mut client = server.accept().await;
    client.handshake(server.udp_port, 1).await;
    assert_eq!(next_event(&mut events).await, VoiceEvent::Connected { encrypted: true });
    let Message::Binary(frame) = client.recv().await else {
        panic!("expected the MLS key package");
    };
    assert_eq!(frame[0], 26);

    // A transition is acknowledged.
    client
        .send(21, json!({ "transition_id": 3, "protocol_version": 1 }))
        .await;
    let ready = client.recv_json().await;
    assert_eq!(
        (ready["op"].as_u64(), ready["d"]["transition_id"].as_u64()),
        (Some(23), Some(3))
    );

    // The server vanishes: the client comes back and identifies again.
    drop(client);
    let mut client = server.accept().await;
    client.handshake(server.udp_port, 0).await;
    assert_eq!(
        next_event(&mut events).await,
        VoiceEvent::Connected { encrypted: false }
    );

    // Leaving closes the socket cleanly.
    handle.disconnect();
    let closed = timeout(WAIT, client.socket.next()).await.unwrap();
    assert!(matches!(closed, Some(Ok(Message::Close(_))) | None));
}

#[tokio::test]
async fn heartbeats_carry_the_sequence_ack() {
    let server = Server::start().await;
    let (events_tx, mut events) = mpsc::channel(64);
    let _handle = connect(info(&server.url), events_tx);
    let mut client = server.accept().await;
    client.handshake(server.udp_port, 0).await;
    next_event(&mut events).await;
    // Read raw (without auto-answering) until the first heartbeat.
    loop {
        let message = timeout(WAIT, client.socket.next()).await.unwrap().unwrap().unwrap();
        let Message::Text(text) = message else { continue };
        let value: Value = serde_json::from_str(&text).unwrap();
        if value["op"] == 3 {
            assert_eq!(value["d"]["seq_ack"], 3, "HELLO, READY, SESSION_DESCRIPTION");
            assert!(value["d"]["t"].as_u64().unwrap() > 0);
            break;
        }
    }
}
