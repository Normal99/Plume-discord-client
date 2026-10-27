//! Voice gateway messages (version 8): JSON for most things, binary frames
//! for DAVE's MLS messages.

use plume_core::Snowflake;
use serde::Deserialize;
use serde_json::{Value, json};

pub mod op {
    pub const IDENTIFY: u8 = 0;
    pub const SELECT_PROTOCOL: u8 = 1;
    pub const READY: u8 = 2;
    pub const HEARTBEAT: u8 = 3;
    pub const SESSION_DESCRIPTION: u8 = 4;
    pub const SPEAKING: u8 = 5;
    pub const HEARTBEAT_ACK: u8 = 6;
    pub const RESUME: u8 = 7;
    pub const HELLO: u8 = 8;
    pub const RESUMED: u8 = 9;
    pub const CLIENTS_CONNECT: u8 = 11;
    pub const CLIENT_DISCONNECT: u8 = 13;
    pub const DAVE_PREPARE_TRANSITION: u8 = 21;
    pub const DAVE_EXECUTE_TRANSITION: u8 = 22;
    pub const DAVE_TRANSITION_READY: u8 = 23;
    pub const DAVE_PREPARE_EPOCH: u8 = 24;
    pub const DAVE_EXTERNAL_SENDER: u8 = 25;
    pub const DAVE_KEY_PACKAGE: u8 = 26;
    pub const DAVE_PROPOSALS: u8 = 27;
    pub const DAVE_COMMIT_WELCOME: u8 = 28;
    pub const DAVE_ANNOUNCE_COMMIT: u8 = 29;
    pub const DAVE_WELCOME: u8 = 30;
    pub const DAVE_INVALID_COMMIT_WELCOME: u8 = 31;
}

/// Close codes after which reconnecting is pointless.
pub fn is_fatal_close(code: u16) -> bool {
    matches!(
        code,
        4001 // unknown opcode (our bug)
        | 4003 // not authenticated
        | 4004 // authentication failed
        | 4006 // session no longer valid
        | 4011 // server not found
        | 4012 // unknown protocol
        | 4014 // disconnected: kicked, moved, or the channel was deleted
        | 4016 // unknown encryption mode
        | 4017 // E2EE required
        | 4020 // bad request
    )
}

/// A message from the voice server.
#[derive(Debug, PartialEq)]
pub enum Incoming {
    Hello {
        heartbeat_interval_ms: f64,
    },
    Ready(Ready),
    SessionDescription(SessionDescription),
    Speaking {
        user_id: Snowflake,
        ssrc: u32,
        speaking: bool,
    },
    HeartbeatAck,
    Resumed,
    ClientsConnect(Vec<Snowflake>),
    ClientDisconnect(Snowflake),
    PrepareTransition {
        transition_id: u16,
        protocol_version: u16,
    },
    ExecuteTransition {
        transition_id: u16,
    },
    PrepareEpoch {
        epoch: u64,
        protocol_version: u16,
    },
    /// A binary DAVE message: its opcode and payload.
    Dave {
        op: u8,
        payload: Vec<u8>,
    },
    Other(u8),
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct Ready {
    pub ssrc: u32,
    pub ip: String,
    pub port: u16,
    #[serde(default)]
    pub modes: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct SessionDescription {
    pub mode: String,
    pub secret_key: Vec<u8>,
    #[serde(default)]
    pub dave_protocol_version: u16,
}

#[derive(Deserialize)]
struct Envelope {
    op: u8,
    #[serde(default)]
    d: Value,
    #[serde(default)]
    seq: Option<u64>,
}

/// Parses a text frame. Returns the message and its sequence number, if any.
pub fn parse_text(text: &str) -> Result<(Incoming, Option<u64>), serde_json::Error> {
    #[derive(Deserialize)]
    struct Speaking {
        user_id: Snowflake,
        ssrc: u32,
        #[serde(default)]
        speaking: u8,
    }
    #[derive(Deserialize)]
    struct Transition {
        transition_id: u16,
        #[serde(default)]
        protocol_version: u16,
    }
    #[derive(Deserialize)]
    struct Epoch {
        epoch: u64,
        protocol_version: u16,
    }

    let envelope: Envelope = serde_json::from_str(text)?;
    let d = envelope.d;
    let message = match envelope.op {
        op::HELLO => Incoming::Hello {
            heartbeat_interval_ms: d["heartbeat_interval"].as_f64().unwrap_or(13_750.0),
        },
        op::READY => Incoming::Ready(serde_json::from_value(d)?),
        op::SESSION_DESCRIPTION => Incoming::SessionDescription(serde_json::from_value(d)?),
        op::SPEAKING => {
            let speaking: Speaking = serde_json::from_value(d)?;
            Incoming::Speaking {
                user_id: speaking.user_id,
                ssrc: speaking.ssrc,
                speaking: speaking.speaking != 0,
            }
        }
        op::HEARTBEAT_ACK => Incoming::HeartbeatAck,
        op::RESUMED => Incoming::Resumed,
        op::CLIENTS_CONNECT => Incoming::ClientsConnect(serde_json::from_value(d["user_ids"].clone())?),
        op::CLIENT_DISCONNECT => Incoming::ClientDisconnect(serde_json::from_value(d["user_id"].clone())?),
        op::DAVE_PREPARE_TRANSITION => {
            let t: Transition = serde_json::from_value(d)?;
            Incoming::PrepareTransition {
                transition_id: t.transition_id,
                protocol_version: t.protocol_version,
            }
        }
        op::DAVE_EXECUTE_TRANSITION => {
            let t: Transition = serde_json::from_value(d)?;
            Incoming::ExecuteTransition {
                transition_id: t.transition_id,
            }
        }
        op::DAVE_PREPARE_EPOCH => {
            let e: Epoch = serde_json::from_value(d)?;
            Incoming::PrepareEpoch {
                epoch: e.epoch,
                protocol_version: e.protocol_version,
            }
        }
        other => Incoming::Other(other),
    };
    Ok((message, envelope.seq))
}

/// Parses a binary frame from the server: `seq (u16) | op (u8) | payload`.
pub fn parse_binary(frame: &[u8]) -> Option<(Incoming, u64)> {
    let (&[hi, lo, op], payload) = frame.split_first_chunk::<3>()?;
    let seq = u64::from(u16::from_be_bytes([hi, lo]));
    Some((
        Incoming::Dave {
            op,
            payload: payload.to_vec(),
        },
        seq,
    ))
}

/// A binary frame to the server: `op (u8) | payload`.
pub fn binary(op: u8, parts: &[&[u8]]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(1 + parts.iter().map(|p| p.len()).sum::<usize>());
    frame.push(op);
    for part in parts {
        frame.extend_from_slice(part);
    }
    frame
}

pub struct Identify<'a> {
    pub server_id: Snowflake,
    pub user_id: Snowflake,
    pub session_id: &'a str,
    pub token: &'a str,
    pub max_dave_protocol_version: u16,
}

pub fn identify(identify: &Identify) -> String {
    json!({
        "op": op::IDENTIFY,
        "d": {
            "server_id": identify.server_id,
            "user_id": identify.user_id,
            "session_id": identify.session_id,
            "token": identify.token,
            "max_dave_protocol_version": identify.max_dave_protocol_version,
            "video": false,
        }
    })
    .to_string()
}

pub fn resume(server_id: Snowflake, session_id: &str, token: &str, seq_ack: Option<u64>) -> String {
    json!({
        "op": op::RESUME,
        "d": { "server_id": server_id, "session_id": session_id, "token": token, "seq_ack": seq_ack }
    })
    .to_string()
}

pub fn select_protocol(address: &str, port: u16, mode: &str) -> String {
    json!({
        "op": op::SELECT_PROTOCOL,
        "d": { "protocol": "udp", "data": { "address": address, "port": port, "mode": mode } }
    })
    .to_string()
}

pub fn heartbeat(nonce: u64, seq_ack: Option<u64>) -> String {
    json!({ "op": op::HEARTBEAT, "d": { "t": nonce, "seq_ack": seq_ack } }).to_string()
}

pub fn speaking(ssrc: u32, speaking: bool) -> String {
    // 1 = microphone.
    json!({ "op": op::SPEAKING, "d": { "speaking": u8::from(speaking), "delay": 0, "ssrc": ssrc } }).to_string()
}

pub fn transition_ready(transition_id: u16) -> String {
    json!({ "op": op::DAVE_TRANSITION_READY, "d": { "transition_id": transition_id } }).to_string()
}

pub fn invalid_commit_welcome(transition_id: u16) -> String {
    json!({ "op": op::DAVE_INVALID_COMMIT_WELCOME, "d": { "transition_id": transition_id } }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_messages() {
        let (hello, seq) = parse_text(r#"{"op": 8, "d": {"v": 8, "heartbeat_interval": 13750.5}}"#).unwrap();
        assert_eq!(
            (hello, seq),
            (
                Incoming::Hello {
                    heartbeat_interval_ms: 13750.5
                },
                None
            )
        );

        let (ready, _) = parse_text(
            r#"{"op": 2, "d": {"ssrc": 42, "ip": "1.2.3.4", "port": 50000,
                "modes": ["aead_aes256_gcm_rtpsize", "aead_xchacha20_poly1305_rtpsize"], "experiments": []}}"#,
        )
        .unwrap();
        let Incoming::Ready(ready) = ready else { panic!() };
        assert_eq!((ready.ssrc, ready.port, ready.modes.len()), (42, 50000, 2));

        let (description, seq) = parse_text(
            r#"{"op": 4, "seq": 3, "d": {"mode": "aead_aes256_gcm_rtpsize", "secret_key": [1, 2, 3], "dave_protocol_version": 1}}"#,
        )
        .unwrap();
        assert_eq!(seq, Some(3));
        assert!(
            matches!(description, Incoming::SessionDescription(d) if d.secret_key == [1, 2, 3] && d.dave_protocol_version == 1)
        );

        let (speaking, _) =
            parse_text(r#"{"op": 5, "seq": 4, "d": {"user_id": "7", "ssrc": 9, "speaking": 1}}"#).unwrap();
        assert_eq!(
            speaking,
            Incoming::Speaking {
                user_id: Snowflake(7),
                ssrc: 9,
                speaking: true
            }
        );
        let (connect, _) = parse_text(r#"{"op": 11, "d": {"user_ids": ["7", "8"]}}"#).unwrap();
        assert_eq!(connect, Incoming::ClientsConnect(vec![Snowflake(7), Snowflake(8)]));
        let (epoch, _) = parse_text(r#"{"op": 24, "d": {"epoch": 1, "protocol_version": 1}}"#).unwrap();
        assert_eq!(
            epoch,
            Incoming::PrepareEpoch {
                epoch: 1,
                protocol_version: 1
            }
        );
        assert_eq!(parse_text(r#"{"op": 99, "d": null}"#).unwrap().0, Incoming::Other(99));
        assert!(parse_text("not json").is_err());
    }

    #[test]
    fn binary_frames() {
        let (message, seq) = parse_binary(&[0x01, 0x02, 25, 0xAA, 0xBB]).unwrap();
        assert_eq!(seq, 258);
        assert_eq!(
            message,
            Incoming::Dave {
                op: 25,
                payload: vec![0xAA, 0xBB]
            }
        );
        assert!(parse_binary(&[1, 2]).is_none());
        assert_eq!(binary(28, &[&[1, 2], &[3]]), [28, 1, 2, 3]);
    }

    #[test]
    fn client_messages() {
        let text = identify(&Identify {
            server_id: Snowflake(10),
            user_id: Snowflake(1),
            session_id: "s",
            token: "t",
            max_dave_protocol_version: 1,
        });
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["d"]["server_id"], "10");
        assert_eq!(value["d"]["max_dave_protocol_version"], 1);
        let value: Value = serde_json::from_str(&heartbeat(5, Some(9))).unwrap();
        assert_eq!(
            (value["op"].as_u64(), value["d"]["seq_ack"].as_u64()),
            (Some(3), Some(9))
        );
        assert!(is_fatal_close(4014) && !is_fatal_close(4015) && !is_fatal_close(1006));
    }
}
