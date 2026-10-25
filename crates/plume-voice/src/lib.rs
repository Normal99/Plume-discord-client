//! Discord voice: the voice gateway, UDP transport, encryption and audio.

#[cfg(feature = "audio")]
pub mod audio;
mod connection;
pub mod crypto;
pub mod dave;
pub mod protocol;
pub mod rtp;
pub mod udp;

pub use connection::{ConnectInfo, VoiceEvent, VoiceHandle, connect};
