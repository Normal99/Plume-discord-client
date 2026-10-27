//! Gateway opcodes and close codes. Kept in one place because the user API
//! is undocumented and these are what changes.

pub const DISPATCH: u8 = 0;
pub const HEARTBEAT: u8 = 1;
pub const IDENTIFY: u8 = 2;
pub const VOICE_STATE_UPDATE: u8 = 4;
pub const RESUME: u8 = 6;
pub const RECONNECT: u8 = 7;
pub const INVALID_SESSION: u8 = 9;
pub const HELLO: u8 = 10;
pub const HEARTBEAT_ACK: u8 = 11;
/// What discord.com sends to get typing (and more) for the servers you look at.
pub const GUILD_SUBSCRIPTIONS_BULK: u8 = 37;

pub const GATEWAY_URL: &str = "wss://gateway.discord.gg";
/// Appended to the gateway URL (and the resume URL).
pub const GATEWAY_QUERY: &str = "/?v=9&encoding=json&compress=zlib-stream";

/// Capability flags sent in IDENTIFY. Each one makes READY smaller or
/// simpler for us:
/// - `LAZY_USER_NOTES`: no notes about other users.
/// - `NO_AFFINE_USER_IDS`: no list of users Discord thinks you know.
/// - `VERSIONED_READ_STATES` / `VERSIONED_USER_GUILD_SETTINGS`: compact forms.
/// - `DEDUPE_USER_OBJECTS`: each user is sent once (DMs list just IDs).
/// - `PRIORITIZED_READY_PAYLOAD`: the less urgent parts (presences, merged
///   members) move to READY_SUPPLEMENTAL, which we don't parse at all.
/// - `USER_SETTINGS_PROTO`: settings as one small protobuf blob, not JSON.
/// - `CLIENT_STATE_V2`: guild fields grouped under `properties`.
pub const CAPABILITIES: u32 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5) | (1 << 9) | (1 << 10);

/// What a close code means for reconnecting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseAction {
    /// Reconnect and resume the session.
    Resume,
    /// Reconnect, but the session is gone: IDENTIFY again.
    Reidentify,
    /// The token is bad. Stop.
    AuthFailed,
    /// Reconnecting won't help (bad request on our side). Stop.
    Fatal,
}

pub fn close_action(code: u16) -> CloseAction {
    match code {
        4004 => CloseAction::AuthFailed,
        // Invalid sequence, session timed out.
        4007 | 4009 => CloseAction::Reidentify,
        // Invalid shard, sharding required, invalid API version, invalid or
        // disallowed intents.
        4010..=4014 => CloseAction::Fatal,
        _ => CloseAction::Resume,
    }
}
