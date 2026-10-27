use crate::Snowflake;
use crate::models::{Channel, Embed, Emoji, Guild, GuildSettings, Message, ReadState, Role, User, VoiceState};

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
        read_states: Vec<ReadState>,
        settings: Vec<GuildSettings>,
        /// Everyone in a voice channel in your servers.
        voice_states: Vec<VoiceState>,
    },
    /// A dropped connection was picked up again without losing events.
    Resumed,
    /// The connection dropped; the gateway is reconnecting.
    Disconnected,
    GuildCreate {
        guild: Guild,
        channels: Vec<Channel>,
        voice_states: Vec<VoiceState>,
    },
    /// The server's name, owner or roles changed. `None` means unchanged.
    GuildUpdate {
        id: Snowflake,
        name: Option<String>,
        owner_id: Option<Snowflake>,
        roles: Option<Vec<Role>>,
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
        /// `None` when the embeds didn't change.
        embeds: Option<Vec<Embed>>,
    },
    MessageDelete {
        id: Snowflake,
        channel_id: Snowflake,
    },
    /// Someone's roles changed (we only care when it's you).
    MemberUpdate {
        guild_id: Snowflake,
        user_id: Snowflake,
        roles: Vec<Snowflake>,
    },
    RoleUpsert {
        guild_id: Snowflake,
        role: Role,
    },
    RoleDelete {
        guild_id: Snowflake,
        role_id: Snowflake,
    },
    ReactionAdd {
        channel_id: Snowflake,
        message_id: Snowflake,
        user_id: Snowflake,
        emoji: Emoji,
    },
    ReactionRemove {
        channel_id: Snowflake,
        message_id: Snowflake,
        user_id: Snowflake,
        emoji: Emoji,
    },
    /// All reactions on a message were removed, or all of one emoji.
    ReactionClear {
        channel_id: Snowflake,
        message_id: Snowflake,
        emoji: Option<Emoji>,
    },
    /// You read a channel up to `message_id`, here or on another device.
    MessageAck {
        channel_id: Snowflake,
        message_id: Snowflake,
    },
    /// Your mute settings for a server changed.
    GuildSettingsUpdate(GuildSettings),
    /// Someone joined, moved or left voice (`left`), or (un)muted.
    VoiceStateUpdate {
        state: VoiceState,
        left: bool,
    },
    /// Where to connect for the voice channel you asked to join.
    VoiceServerUpdate {
        guild_id: Option<Snowflake>,
        channel_id: Option<Snowflake>,
        /// `None` while Discord finds a new server; wait for another update.
        endpoint: Option<String>,
        token: String,
    },
    /// The active threads in (part of) a server, after you gain access.
    ThreadsSync {
        guild_id: Snowflake,
        threads: Vec<Channel>,
    },
    TypingStart {
        channel_id: Snowflake,
        user_id: Snowflake,
    },
    /// A dispatch we don't handle, by name. Its data was never parsed.
    Other(String),
}
