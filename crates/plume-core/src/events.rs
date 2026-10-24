use crate::Snowflake;
use crate::models::{Channel, Guild, Message, User};

/// Something that happened on Discord, already decoded into models.
/// The gateway produces these; [`crate::Store::apply`] consumes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A fresh session: everything we know is replaced by this.
    Ready {
        me: User,
        guilds: Vec<Guild>,
        /// Channels of every guild above, plus DMs and group DMs.
        channels: Vec<Channel>,
        users: Vec<User>,
    },
    /// A dropped connection was picked up again without losing events.
    Resumed,
    GuildCreate {
        guild: Guild,
        channels: Vec<Channel>,
    },
    GuildDelete {
        id: Snowflake,
        /// True during an outage: the server still exists, it's just not reachable.
        unavailable: bool,
    },
    /// A channel was created or changed.
    ChannelUpsert(Channel),
    ChannelDelete {
        id: Snowflake,
    },
    MessageCreate {
        message: Message,
        author: User,
    },
    MessageUpdate {
        id: Snowflake,
        channel_id: Snowflake,
        /// `None` when only something else changed (an embed loaded, say).
        content: Option<String>,
    },
    MessageDelete {
        id: Snowflake,
        channel_id: Snowflake,
    },
    TypingStart {
        channel_id: Snowflake,
        user_id: Snowflake,
    },
    /// A dispatch we don't handle, by name. Its data was never parsed.
    Other(String),
}
