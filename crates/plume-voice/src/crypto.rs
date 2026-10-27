//! Transport encryption for voice packets: the `*_rtpsize` AEAD modes.
//!
//! An encrypted packet is `header | ciphertext | tag (16) | nonce (4)`.
//! The RTP header (including CSRCs and the 4-byte extension header, if
//! any) is the associated data; the extension's body is encrypted along
//! with the audio. The 4-byte nonce is a counter, padded with zeros to
//! the cipher's nonce size.

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::XChaCha20Poly1305;

use crate::rtp;

const TAG_LEN: usize = 16;
const NONCE_LEN: usize = 4;

/// What we offer, best first. Discord requires every client to support
/// XChaCha20; AES-GCM is faster where the CPU has AES instructions.
pub const SUPPORTED_MODES: [&str; 2] = [Mode::AES_GCM, Mode::XCHACHA];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Aes256Gcm,
    XChaCha20Poly1305,
}

impl Mode {
    const AES_GCM: &str = "aead_aes256_gcm_rtpsize";
    const XCHACHA: &str = "aead_xchacha20_poly1305_rtpsize";

    pub fn name(self) -> &'static str {
        match self {
            Mode::Aes256Gcm => Self::AES_GCM,
            Mode::XChaCha20Poly1305 => Self::XCHACHA,
        }
    }

    pub fn from_name(name: &str) -> Option<Mode> {
        match name {
            Self::AES_GCM => Some(Mode::Aes256Gcm),
            Self::XCHACHA => Some(Mode::XChaCha20Poly1305),
            _ => None,
        }
    }

    /// The best mode the server offers that we support.
    pub fn choose<'a>(offered: impl IntoIterator<Item = &'a str>) -> Option<Mode> {
        let offered: Vec<&str> = offered.into_iter().collect();
        SUPPORTED_MODES
            .iter()
            .find(|mode| offered.contains(mode))
            .and_then(|name| Mode::from_name(name))
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("the voice key isn't 32 bytes")]
    BadKey,
    #[error("packet too short or malformed")]
    Malformed,
    #[error("packet failed authentication")]
    Forged,
}

enum Cipher {
    Aes(Box<Aes256Gcm>),
    XChaCha(Box<XChaCha20Poly1305>),
}

/// Encrypts our packets and decrypts everyone else's.
pub struct Crypto {
    cipher: Cipher,
    counter: u32,
}

impl Crypto {
    pub fn new(mode: Mode, key: &[u8]) -> Result<Self, CryptoError> {
        let cipher = match mode {
            Mode::Aes256Gcm => Cipher::Aes(Box::new(
                Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError::BadKey)?,
            )),
            Mode::XChaCha20Poly1305 => Cipher::XChaCha(Box::new(
                XChaCha20Poly1305::new_from_slice(key).map_err(|_| CryptoError::BadKey)?,
            )),
        };
        Ok(Crypto { cipher, counter: 0 })
    }

    /// Encrypts `payload` behind `header` into `out` (which is cleared first).
    pub fn encrypt(&mut self, header: &[u8], payload: &[u8], out: &mut Vec<u8>) -> Result<(), CryptoError> {
        let nonce = self.counter.to_be_bytes();
        self.counter = self.counter.wrapping_add(1);

        out.clear();
        out.extend_from_slice(header);
        out.extend_from_slice(payload);
        let (aad, body) = out.split_at_mut(header.len());
        let tag = self.seal(&nonce, aad, body)?;
        out.extend_from_slice(&tag);
        out.extend_from_slice(&nonce);
        Ok(())
    }

    /// Decrypts a packet in place and returns the audio payload (with any
    /// RTP header extension stripped off).
    pub fn decrypt<'a>(&self, packet: &'a mut [u8]) -> Result<&'a [u8], CryptoError> {
        let header = rtp::Header::parse(packet).ok_or(CryptoError::Malformed)?;
        let aad_len = header.aad_len();
        if packet.len() < aad_len + TAG_LEN + NONCE_LEN {
            return Err(CryptoError::Malformed);
        }
        let (rest, nonce) = packet.split_at_mut(packet.len() - NONCE_LEN);
        let nonce: [u8; NONCE_LEN] = (&*nonce).try_into().expect("4 bytes");
        let (rest, tag) = rest.split_at_mut(rest.len() - TAG_LEN);
        let (aad, body) = rest.split_at_mut(aad_len);
        self.open(&nonce, aad, body, tag)?;
        let extension = header.extension_body_len();
        body.get(extension..).ok_or(CryptoError::Malformed)
    }

    fn seal(&self, counter: &[u8; 4], aad: &[u8], body: &mut [u8]) -> Result<[u8; TAG_LEN], CryptoError> {
        let tag = match &self.cipher {
            Cipher::Aes(cipher) => cipher.encrypt_in_place_detached(&pad::<12>(counter).into(), aad, body),
            Cipher::XChaCha(cipher) => cipher.encrypt_in_place_detached(&pad::<24>(counter).into(), aad, body),
        };
        tag.map(Into::into).map_err(|_| CryptoError::Malformed)
    }

    fn open(&self, counter: &[u8; 4], aad: &[u8], body: &mut [u8], tag: &[u8]) -> Result<(), CryptoError> {
        let result = match &self.cipher {
            Cipher::Aes(cipher) => cipher.decrypt_in_place_detached(&pad::<12>(counter).into(), aad, body, tag.into()),
            Cipher::XChaCha(cipher) => {
                cipher.decrypt_in_place_detached(&pad::<24>(counter).into(), aad, body, tag.into())
            }
        };
        result.map_err(|_| CryptoError::Forged)
    }
}

/// The 4-byte counter followed by zeros.
fn pad<const N: usize>(counter: &[u8; 4]) -> [u8; N] {
    let mut nonce = [0; N];
    nonce[..4].copy_from_slice(counter);
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7; 32];

    #[test]
    fn picks_the_best_offered_mode() {
        let offered = ["xsalsa20_poly1305", Mode::XCHACHA, Mode::AES_GCM];
        assert_eq!(Mode::choose(offered), Some(Mode::Aes256Gcm));
        assert_eq!(Mode::choose([Mode::XCHACHA]), Some(Mode::XChaCha20Poly1305));
        assert_eq!(Mode::choose(["xsalsa20_poly1305"]), None);
    }

    #[test]
    fn round_trips_and_counts_nonces() {
        for mode in [Mode::Aes256Gcm, Mode::XChaCha20Poly1305] {
            let mut sender = Crypto::new(mode, &KEY).unwrap();
            let receiver = Crypto::new(mode, &KEY).unwrap();
            let header = rtp::Header::outgoing(1, 960, 1234).to_bytes();
            let mut packet = Vec::new();
            for counter in 0..3u32 {
                sender.encrypt(&header, b"opus frame", &mut packet).unwrap();
                assert_eq!(packet.len(), 12 + 10 + 16 + 4);
                assert_eq!(&packet[..12], &header, "header stays readable");
                assert_eq!(packet[packet.len() - 4..], counter.to_be_bytes());
                assert_ne!(&packet[12..22], b"opus frame");
                assert_eq!(receiver.decrypt(&mut packet.clone()).unwrap(), b"opus frame");
            }
        }
        assert!(Crypto::new(Mode::Aes256Gcm, &[0; 16]).is_err());
    }

    #[test]
    fn rejects_tampering_and_garbage() {
        let mut sender = Crypto::new(Mode::Aes256Gcm, &KEY).unwrap();
        let receiver = Crypto::new(Mode::Aes256Gcm, &KEY).unwrap();
        let header = rtp::Header::outgoing(1, 960, 1234).to_bytes();
        let mut packet = Vec::new();
        sender.encrypt(&header, b"opus frame", &mut packet).unwrap();

        let mut bad_header = packet.clone();
        bad_header[2] ^= 1; // the sequence number is authenticated
        assert_eq!(receiver.decrypt(&mut bad_header), Err(CryptoError::Forged));
        let mut wrong_key = packet.clone();
        assert!(
            Crypto::new(Mode::Aes256Gcm, &[8; 32])
                .unwrap()
                .decrypt(&mut wrong_key)
                .is_err()
        );
        assert_eq!(
            receiver.decrypt(&mut packet[..20].to_vec()),
            Err(CryptoError::Malformed)
        );
        assert_eq!(receiver.decrypt(&mut [0u8; 3]), Err(CryptoError::Malformed));
    }

    #[test]
    fn strips_the_encrypted_header_extension() {
        // What Discord sends: X bit set, a one-word extension (header in the
        // AAD, body encrypted with the audio).
        let mut sender = Crypto::new(Mode::XChaCha20Poly1305, &KEY).unwrap();
        let receiver = Crypto::new(Mode::XChaCha20Poly1305, &KEY).unwrap();
        let mut header = rtp::Header::outgoing(5, 4800, 99).to_bytes().to_vec();
        header[0] |= 0x10;
        header.extend_from_slice(&[0xBE, 0xDE, 0x00, 0x01]);
        let mut payload = vec![0x10, 0xAA, 0, 0]; // extension body
        payload.extend_from_slice(b"audio");
        let mut packet = Vec::new();
        sender.encrypt(&header, &payload, &mut packet).unwrap();
        assert_eq!(receiver.decrypt(&mut packet).unwrap(), b"audio");
    }
}
