//! How Plume identifies itself to Discord, over HTTP and on the gateway.
//! Kept in one place so it's easy to update when Discord changes things
//! (see docs/04-discord-protocol.md, "Client identity").

pub const BROWSER_VERSION: &str = "140.0.0.0";

pub const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

/// The `os` we report, matching the user agent above.
pub const OS: &str = "Linux";
