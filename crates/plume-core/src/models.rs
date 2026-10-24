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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guild {
    pub id: Snowflake,
    pub name: String,
    /// IDs of every channel in the server, in no particular order.
    pub channels: Vec<Snowflake>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelKind {
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
        matches!(self, ChannelKind::Text | ChannelKind::News)
    }

    pub fn is_private(self) -> bool {
        matches!(self, ChannelKind::Dm | ChannelKind::GroupDm)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
