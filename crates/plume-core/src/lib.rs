//! Plume's toolkit-free heart: Discord's data types, the in-memory
//! [`Store`] that every frontend reads from, and the usage [`Profile`]s.
//!
//! Nothing in here does I/O. The gateway and REST crates turn network data
//! into [`Event`]s and models; the store applies them.

pub mod events;
pub mod identity;
pub mod ids;
pub mod models;
pub mod profile;
pub mod store;
pub mod token;
pub mod wire;

pub use events::Event;
pub use ids::Snowflake;
pub use profile::{Profile, Tier};
pub use store::{Store, Unread};
pub use token::Token;
