//! The live connection to Discord: login (IDENTIFY), heartbeats, resuming
//! after drops, reconnecting with backoff, and decoding dispatches into
//! [`plume_core::Event`]s.
//!
//! [`Gateway::run`] owns the connection and sends events to a channel;
//! a [`GatewayHandle`] sends requests the other way.

mod compression;
mod connection;
pub mod dispatch;
mod error;
mod heartbeat;
pub mod opcodes;
pub mod resume;

pub use connection::{Gateway, GatewayHandle};
pub use error::GatewayError;
