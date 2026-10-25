//! The UDP side: finding our public address (IP discovery).

use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::timeout;

const REQUEST: u16 = 1;
const RESPONSE: u16 = 2;
const PACKET_LEN: usize = 74;

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("UDP error: {0}")]
    Io(#[from] std::io::Error),
    #[error("the voice server didn't answer IP discovery")]
    Timeout,
    #[error("bad IP discovery reply")]
    BadReply,
}

/// An IP discovery request for our SSRC.
pub fn request(ssrc: u32) -> [u8; PACKET_LEN] {
    let mut packet = [0; PACKET_LEN];
    packet[0..2].copy_from_slice(&REQUEST.to_be_bytes());
    packet[2..4].copy_from_slice(&70u16.to_be_bytes());
    packet[4..8].copy_from_slice(&ssrc.to_be_bytes());
    packet
}

/// The address and port the server saw us at.
pub fn parse_reply(packet: &[u8]) -> Option<(String, u16)> {
    if packet.len() < PACKET_LEN || packet[0..2] != RESPONSE.to_be_bytes() {
        return None;
    }
    let address = &packet[8..72];
    let end = address.iter().position(|&b| b == 0).unwrap_or(address.len());
    let address = std::str::from_utf8(&address[..end]).ok()?.to_owned();
    let port = u16::from_be_bytes([packet[72], packet[73]]);
    (!address.is_empty()).then_some((address, port))
}

/// Asks the voice server what our public address is, retrying a few times
/// (it's UDP: packets get lost).
pub async fn discover(socket: &UdpSocket, server: SocketAddr, ssrc: u32) -> Result<(String, u16), DiscoveryError> {
    let mut buffer = [0; 1500];
    for _ in 0..5 {
        socket.send_to(&request(ssrc), server).await?;
        let Ok(received) = timeout(Duration::from_secs(1), socket.recv_from(&mut buffer)).await else {
            continue;
        };
        let (len, from) = received?;
        if from == server {
            return parse_reply(&buffer[..len]).ok_or(DiscoveryError::BadReply);
        }
    }
    Err(DiscoveryError::Timeout)
}

/// What a voice server answers, for tests and the fake server.
pub fn reply(ssrc: u32, address: &str, port: u16) -> [u8; PACKET_LEN] {
    let mut packet = [0; PACKET_LEN];
    packet[0..2].copy_from_slice(&RESPONSE.to_be_bytes());
    packet[2..4].copy_from_slice(&70u16.to_be_bytes());
    packet[4..8].copy_from_slice(&ssrc.to_be_bytes());
    let bytes = address.as_bytes();
    packet[8..8 + bytes.len().min(63)].copy_from_slice(&bytes[..bytes.len().min(63)]);
    packet[72..74].copy_from_slice(&port.to_be_bytes());
    packet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_have_discords_layout() {
        let packet = request(0x0102_0304);
        assert_eq!(packet[..8], [0, 1, 0, 70, 1, 2, 3, 4]);
        assert_eq!(
            parse_reply(&reply(9, "203.0.113.5", 50_000)),
            Some(("203.0.113.5".into(), 50_000))
        );
        assert_eq!(parse_reply(&packet), None, "a request isn't a reply");
        assert_eq!(parse_reply(&reply(9, "x", 1)[..20]), None);
    }

    #[tokio::test]
    async fn discovers_through_a_fake_server() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buffer = [0; 100];
            let (_, from) = server.recv_from(&mut buffer).await.unwrap();
            let ssrc = u32::from_be_bytes(buffer[4..8].try_into().unwrap());
            server
                .send_to(&reply(ssrc, "198.51.100.7", from.port()), from)
                .await
                .unwrap();
        });
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = client.local_addr().unwrap().port();
        let found = discover(&client, server_addr, 77).await.unwrap();
        assert_eq!(found, ("198.51.100.7".into(), port));
    }
}
