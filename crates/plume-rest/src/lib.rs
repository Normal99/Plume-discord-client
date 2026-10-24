//! Discord's HTTP API: message history, sending messages, and so on.
//!
//! Every request goes through a rate limiter, so callers never have to
//! worry about getting the account flagged for spamming the API.

mod client;
mod errors;
mod ratelimit;
mod routes;

pub use client::{RestClient, build_http_client};
pub use errors::RestError;
pub use plume_core::identity::USER_AGENT;
