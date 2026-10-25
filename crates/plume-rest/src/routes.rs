use plume_core::Snowflake;
use reqwest::Method;

/// Every endpoint we call. Keeping them in one enum means the rate limiter
/// always knows which bucket a request belongs to.
#[derive(Debug, Clone)]
pub enum Route {
    CurrentUser,
    GetMessages {
        channel_id: Snowflake,
    },
    CreateMessage {
        channel_id: Snowflake,
    },
    EditMessage {
        channel_id: Snowflake,
        message_id: Snowflake,
    },
    DeleteMessage {
        channel_id: Snowflake,
        message_id: Snowflake,
    },
    TriggerTyping {
        channel_id: Snowflake,
    },
    /// Marks a channel read up to a message, on every device.
    Ack {
        channel_id: Snowflake,
        message_id: Snowflake,
    },
    /// `emoji` is already URL-encoded (see [`encode_emoji`]).
    AddReaction {
        channel_id: Snowflake,
        message_id: Snowflake,
        emoji: String,
    },
    RemoveReaction {
        channel_id: Snowflake,
        message_id: Snowflake,
        emoji: String,
    },
    /// Rings the other people in a DM call.
    RingCall {
        channel_id: Snowflake,
    },
}

impl Route {
    pub fn method(&self) -> Method {
        match self {
            Route::CurrentUser | Route::GetMessages { .. } => Method::GET,
            Route::CreateMessage { .. } | Route::TriggerTyping { .. } | Route::Ack { .. } | Route::RingCall { .. } => {
                Method::POST
            }
            Route::EditMessage { .. } => Method::PATCH,
            Route::AddReaction { .. } => Method::PUT,
            Route::DeleteMessage { .. } | Route::RemoveReaction { .. } => Method::DELETE,
        }
    }

    pub fn path(&self) -> String {
        match self {
            Route::CurrentUser => "/users/@me".to_owned(),
            Route::GetMessages { channel_id } | Route::CreateMessage { channel_id } => {
                format!("/channels/{channel_id}/messages")
            }
            Route::EditMessage { channel_id, message_id } | Route::DeleteMessage { channel_id, message_id } => {
                format!("/channels/{channel_id}/messages/{message_id}")
            }
            Route::TriggerTyping { channel_id } => format!("/channels/{channel_id}/typing"),
            Route::RingCall { channel_id } => format!("/channels/{channel_id}/call/ring"),
            Route::Ack { channel_id, message_id } => format!("/channels/{channel_id}/messages/{message_id}/ack"),
            Route::AddReaction {
                channel_id,
                message_id,
                emoji,
            }
            | Route::RemoveReaction {
                channel_id,
                message_id,
                emoji,
            } => format!("/channels/{channel_id}/messages/{message_id}/reactions/{emoji}/@me"),
        }
    }

    /// Discord rate limits per route *and* per "major parameter" (the
    /// channel or guild), so sending in two channels uses two buckets.
    /// Message IDs aren't major parameters, so they're left out.
    pub fn bucket_key(&self) -> String {
        match self {
            Route::CurrentUser => "GET /users/@me".to_owned(),
            Route::GetMessages { channel_id } => format!("GET /channels/{channel_id}/messages"),
            Route::CreateMessage { channel_id } => format!("POST /channels/{channel_id}/messages"),
            Route::EditMessage { channel_id, .. } => format!("PATCH /channels/{channel_id}/messages/:id"),
            Route::DeleteMessage { channel_id, .. } => format!("DELETE /channels/{channel_id}/messages/:id"),
            Route::TriggerTyping { channel_id } => format!("POST /channels/{channel_id}/typing"),
            Route::RingCall { channel_id } => format!("POST /channels/{channel_id}/call/ring"),
            Route::Ack { channel_id, .. } => format!("POST /channels/{channel_id}/messages/:id/ack"),
            Route::AddReaction { channel_id, .. } | Route::RemoveReaction { channel_id, .. } => {
                format!("/channels/{channel_id}/messages/:id/reactions")
            }
        }
    }
}

/// Percent-encodes an emoji for a reaction URL: a Unicode emoji as its
/// UTF-8 bytes, a custom one as `name:id`.
pub fn encode_emoji(name: &str, id: Option<Snowflake>) -> String {
    let raw = match id {
        Some(id) => format!("{name}:{id}"),
        None => name.to_owned(),
    };
    let mut out = String::with_capacity(raw.len() * 3);
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b':') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emoji_encoding() {
        assert_eq!(encode_emoji("👍", None), "%F0%9F%91%8D");
        assert_eq!(encode_emoji("party_blob", Some(Snowflake(42))), "party_blob:42");
    }

    #[test]
    fn message_ids_share_a_bucket() {
        let a = Route::DeleteMessage {
            channel_id: Snowflake(1),
            message_id: Snowflake(2),
        };
        let b = Route::DeleteMessage {
            channel_id: Snowflake(1),
            message_id: Snowflake(3),
        };
        assert_eq!(a.bucket_key(), b.bucket_key());
        assert_ne!(a.path(), b.path());
        assert_eq!(a.method(), Method::DELETE);
    }
}
