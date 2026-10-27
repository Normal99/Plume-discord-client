//! RTP headers, as far as Discord voice needs them.

/// Discord's payload type for Opus.
pub const OPUS_PAYLOAD_TYPE: u8 = 0x78;
/// 20 ms of 48 kHz audio: the timestamp step per packet.
pub const SAMPLES_PER_FRAME: u32 = 960;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    csrc_count: u8,
    /// Length of the extension body in 32-bit words, if there is one.
    extension_words: Option<u16>,
}

impl Header {
    /// The header of a packet we send.
    pub fn outgoing(sequence: u16, timestamp: u32, ssrc: u32) -> Self {
        Header {
            sequence,
            timestamp,
            ssrc,
            csrc_count: 0,
            extension_words: None,
        }
    }

    pub fn to_bytes(self) -> [u8; 12] {
        let mut bytes = [0; 12];
        bytes[0] = 0x80;
        bytes[1] = OPUS_PAYLOAD_TYPE;
        bytes[2..4].copy_from_slice(&self.sequence.to_be_bytes());
        bytes[4..8].copy_from_slice(&self.timestamp.to_be_bytes());
        bytes[8..12].copy_from_slice(&self.ssrc.to_be_bytes());
        bytes
    }

    /// Reads a header; `None` if it isn't RTP version 2 or is cut short.
    pub fn parse(packet: &[u8]) -> Option<Header> {
        if packet.len() < 12 || packet[0] >> 6 != 2 {
            return None;
        }
        let csrc_count = packet[0] & 0x0F;
        let mut header = Header {
            sequence: u16::from_be_bytes([packet[2], packet[3]]),
            timestamp: u32::from_be_bytes(packet[4..8].try_into().ok()?),
            ssrc: u32::from_be_bytes(packet[8..12].try_into().ok()?),
            csrc_count,
            extension_words: None,
        };
        if packet[0] & 0x10 != 0 {
            let at = 12 + 4 * usize::from(csrc_count);
            let words = packet.get(at + 2..at + 4)?;
            header.extension_words = Some(u16::from_be_bytes([words[0], words[1]]));
        }
        (packet.len() >= header.aad_len()).then_some(header)
    }

    /// Bytes covered as associated data: the fixed header, CSRCs and the
    /// extension's 4-byte header.
    pub fn aad_len(&self) -> usize {
        12 + 4 * usize::from(self.csrc_count) + if self.extension_words.is_some() { 4 } else { 0 }
    }

    /// Bytes of extension data at the start of the decrypted payload.
    pub fn extension_body_len(&self) -> usize {
        4 * usize::from(self.extension_words.unwrap_or(0))
    }

    /// Whether this is an RTCP packet (Discord mixes them into the stream).
    pub fn is_rtcp(packet: &[u8]) -> bool {
        packet.len() >= 2 && (200..=204).contains(&packet[1])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_reads_headers() {
        let header = Header::outgoing(65535, 0xDEAD_BEEF, 42);
        let bytes = header.to_bytes();
        assert_eq!(bytes[..2], [0x80, 0x78]);
        assert_eq!(Header::parse(&bytes), Some(header));
        assert_eq!(header.aad_len(), 12);
        assert!(Header::parse(&bytes[..11]).is_none());
        assert!(Header::parse(&[0x40; 12]).is_none(), "version 1");
    }

    #[test]
    fn understands_csrcs_and_extensions() {
        let mut packet = Header::outgoing(1, 2, 3).to_bytes().to_vec();
        packet[0] = 0x80 | 0x10 | 0x01; // extension + one CSRC
        packet.extend_from_slice(&[0, 0, 0, 9]); // CSRC
        packet.extend_from_slice(&[0xBE, 0xDE, 0, 2]); // extension: 2 words
        let header = Header::parse(&packet).unwrap();
        assert_eq!((header.aad_len(), header.extension_body_len()), (20, 8));
        assert!(Header::parse(&packet[..18]).is_none());
        assert!(Header::is_rtcp(&[0x81, 200, 0, 6]) && !Header::is_rtcp(&packet));
    }
}
