//! The data Plume keeps about Discord. Only the fields some frontend
//! actually uses: everything else is dropped while parsing (see `wire`).

use crate::Snowflake;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: Snowflake,
    pub username: String,
    /// The display name people pick; `None` means "use the username".
    pub global_name: Option<String>,
}

impl User {
    /// What to call this person in the UI.
    pub fn display_name(&self) -> &str {
        match self.global_name.as_deref() {
            Some(name) if !name.is_empty() => name,
            _ => &self.username,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Guild {
    pub id: Snowflake,
    pub name: String,
    /// IDs of every channel in the server, in no particular order.
    pub channels: Vec<Snowflake>,
    pub owner_id: Option<Snowflake>,
    pub roles: Vec<Role>,
    /// Your roles here. `None` if Discord hasn't told us (then every channel
    /// is shown, since hiding ones you can see would be worse).
    pub my_roles: Option<Vec<Snowflake>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Role {
    pub id: Snowflake,
    /// Permission bits (see [`crate::permissions`]).
    pub permissions: u64,
}

/// A channel's permission tweak for one role or one member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Overwrite {
    pub id: Snowflake,
    pub kind: OverwriteKind,
    pub allow: u64,
    pub deny: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverwriteKind {
    Role,
    Member,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ChannelKind {
    #[default]
    Text,
    Dm,
    Voice,
    GroupDm,
    Category,
    /// "Announcement" channels: text channels other servers can follow.
    News,
    Thread,
    Stage,
    Forum,
    /// Types Discord added that we don't know about yet.
    Other,
}

impl ChannelKind {
    pub fn from_type(kind: u8) -> Self {
        match kind {
            0 => ChannelKind::Text,
            1 => ChannelKind::Dm,
            2 => ChannelKind::Voice,
            3 => ChannelKind::GroupDm,
            4 => ChannelKind::Category,
            5 => ChannelKind::News,
            10..=12 => ChannelKind::Thread,
            13 => ChannelKind::Stage,
            15 | 16 => ChannelKind::Forum,
            _ => ChannelKind::Other,
        }
    }

    /// Channels you can read and send messages in from a text list.
    pub fn is_text(self) -> bool {
        matches!(self, ChannelKind::Text | ChannelKind::News | ChannelKind::Thread)
    }

    pub fn is_private(self) -> bool {
        matches!(self, ChannelKind::Dm | ChannelKind::GroupDm)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Channel {
    pub id: Snowflake,
    /// `None` for DMs and group DMs.
    pub guild_id: Option<Snowflake>,
    /// `None` for DMs (they're named after the other person).
    pub name: Option<String>,
    pub kind: ChannelKind,
    /// The category this channel sits in, if any.
    pub parent_id: Option<Snowflake>,
    pub position: i32,
    /// Who's in a DM or group DM (not including you).
    pub recipients: Vec<Snowflake>,
    /// The newest message, used to sort DMs and work out unread state.
    pub last_message_id: Option<Snowflake>,
    pub overwrites: Vec<Overwrite>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Message {
    pub id: Snowflake,
    pub channel_id: Snowflake,
    pub author_id: Snowflake,
    pub content: String,
    pub edited: bool,
    /// The message this one replies to.
    pub reply_to: Option<Snowflake>,
    /// Users mentioned with `<@id>`.
    pub mentions: Vec<Snowflake>,
    pub mention_everyone: bool,
    pub attachments: Vec<Attachment>,
    pub reactions: Vec<Reaction>,
    pub embeds: Vec<Embed>,
}

/// A link preview or a bot's rich message, reduced to its text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Embed {
    pub title: Option<String>,
    pub url: Option<String>,
    /// Cut to [`Embed::MAX_DESCRIPTION`] characters when parsed.
    pub description: Option<String>,
}

impl Embed {
    pub const MAX_DESCRIPTION: usize = 300;
}

/// A Unicode emoji (`id: None`, `name` is the emoji itself) or a custom one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Emoji {
    pub name: String,
    pub id: Option<Snowflake>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reaction {
    pub emoji: Emoji,
    pub count: u32,
    /// You reacted with this one.
    pub me: bool,
}

impl Message {
    /// When it was sent, in Unix milliseconds (read from the ID).
    pub fn timestamp_ms(&self) -> u64 {
        self.id.timestamp_ms()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub filename: String,
    pub url: String,
    /// In bytes.
    pub size: u64,
}

/// How far you've read in a channel, as synced with Discord.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadState {
    pub channel_id: Snowflake,
    /// The newest message you've seen (on any device).
    pub last_read: Snowflake,
    /// Pings since then.
    pub mentions: u32,
}

/// Your notification settings for one server (or for DMs, when `guild_id`
/// is `None`), reduced to what matters for unread state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuildSettings {
    pub guild_id: Option<Snowflake>,
    /// The whole server is muted.
    pub muted: bool,
    /// Channels (or categories) muted on their own.
    pub muted_channels: Vec<Snowflake>,
}

/// Someone in a voice channel or call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VoiceState {
    /// `None` for DM and group calls.
    pub guild_id: Option<Snowflake>,
    pub channel_id: Snowflake,
    pub user_id: Snowflake,
    /// Needed to connect to the voice server (only meaningful for you).
    pub session_id: String,
    pub self_mute: bool,
    pub self_deaf: bool,
    /// Muted or deafened by a moderator.
    pub mute: bool,
    pub deaf: bool,
}

impl VoiceState {
    pub fn is_muted(&self) -> bool {
        self.self_mute || self.mute || self.is_deafened()
    }

    pub fn is_deafened(&self) -> bool {
        self.self_deaf || self.deaf
    }
}
