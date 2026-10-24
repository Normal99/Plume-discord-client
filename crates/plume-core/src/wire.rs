//! JSON shapes shared by the gateway and REST, and how they become models.
//!
//! Each struct lists only the fields we use; serde skips the rest without
//! allocating them, which is most of what keeps READY cheap to parse.
//! Everything optional is `#[serde(default)]` because Discord leaves fields
//! out (or sends `null`) more often than its docs suggest.

use serde::Deserialize;

use crate::Snowflake;
use crate::models::{Attachment, Channel, ChannelKind, Guild, Message, User};

#[derive(Debug, Deserialize)]
pub struct WireUser {
    pub id: Snowflake,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub global_name: Option<String>,
}

impl WireUser {
    pub fn into_model(self) -> User {
        User {
            id: self.id,
            username: self.username,
            global_name: self.global_name,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct WireChannel {
    pub id: Snowflake,
    #[serde(rename = "type", default)]
    pub kind: u8,
    #[serde(default)]
    pub guild_id: Option<Snowflake>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub parent_id: Option<Snowflake>,
    #[serde(default)]
    pub position: Option<i32>,
    /// Full user objects (bots, and user accounts without deduplication).
    #[serde(default)]
    pub recipients: Vec<WireUser>,
    /// Just IDs, with the users sent once in READY's `users` list.
    #[serde(default)]
    pub recipient_ids: Vec<Snowflake>,
    #[serde(default)]
    pub last_message_id: Option<Snowflake>,
}

impl WireChannel {
    /// The channel, with `guild_id` filled in from the parent guild when the
    /// JSON doesn't say (channels inside GUILD_CREATE don't).
    pub fn into_model(self, guild_id: Option<Snowflake>) -> Channel {
        self.into_parts(guild_id).0
    }

    /// The channel and the users it carried as recipients.
    pub fn into_parts(self, guild_id: Option<Snowflake>) -> (Channel, Vec<User>) {
        let mut recipients = self.recipient_ids;
        recipients.extend(self.recipients.iter().map(|u| u.id));
        let users = self.recipients.into_iter().map(WireUser::into_model).collect();
        let channel = Channel {
            id: self.id,
            guild_id: self.guild_id.or(guild_id),
            name: self.name,
            kind: ChannelKind::from_type(self.kind),
            parent_id: self.parent_id,
            position: self.position.unwrap_or(0),
            recipients,
            last_message_id: self.last_message_id,
        };
        (channel, users)
    }
}

/// A guild from READY or GUILD_CREATE. User accounts get the guild's own
/// fields under `properties`; bots get them at the top level. Both work.
#[derive(Debug, Deserialize)]
pub struct WireGuild {
    pub id: Snowflake,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub properties: Option<WireGuildProperties>,
    /// Set during outages; such a guild has no other data and arrives
    /// later in a GUILD_CREATE.
    #[serde(default)]
    pub unavailable: bool,
    #[serde(default)]
    pub channels: Vec<WireChannel>,
}

#[derive(Debug, Deserialize)]
pub struct WireGuildProperties {
    #[serde(default)]
    pub name: Option<String>,
}

impl WireGuild {
    pub fn into_models(self) -> (Guild, Vec<Channel>) {
        let id = self.id;
        let name = self
            .properties
            .and_then(|p| p.name)
            .or(self.name)
            .unwrap_or_else(|| "(unnamed server)".to_owned());
        let channels: Vec<Channel> = self.channels.into_iter().map(|c| c.into_model(Some(id))).collect();
        let guild = Guild {
            id,
            name,
            channels: channels.iter().map(|c| c.id).collect(),
        };
        (guild, channels)
    }
}

#[derive(Debug, Deserialize)]
pub struct WireMessage {
    pub id: Snowflake,
    pub channel_id: Snowflake,
    pub author: WireUser,
    #[serde(default)]
    pub content: String,
    /// We only care whether it's there, so it's never parsed as a date.
    #[serde(default)]
    pub edited_timestamp: Option<serde::de::IgnoredAny>,
    #[serde(default)]
    pub message_reference: Option<WireMessageReference>,
    #[serde(default)]
    pub mentions: Vec<WireMention>,
    #[serde(default)]
    pub mention_everyone: bool,
    #[serde(default)]
    pub attachments: Vec<WireAttachment>,
}

#[derive(Debug, Deserialize)]
pub struct WireMessageReference {
    #[serde(default)]
    pub message_id: Option<Snowflake>,
}

/// Mentions are full user objects, but the ID is all we need.
#[derive(Debug, Deserialize)]
pub struct WireMention {
    pub id: Snowflake,
}

#[derive(Debug, Deserialize)]
pub struct WireAttachment {
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub size: u64,
}

impl WireMessage {
    pub fn into_models(self) -> (Message, User) {
        let message = Message {
            id: self.id,
            channel_id: self.channel_id,
            author_id: self.author.id,
            content: self.content,
            edited: self.edited_timestamp.is_some(),
            reply_to: self.message_reference.and_then(|r| r.message_id),
            mentions: self.mentions.into_iter().map(|m| m.id).collect(),
            mention_everyone: self.mention_everyone,
            attachments: self
                .attachments
                .into_iter()
                .map(|a| Attachment {
                    filename: a.filename,
                    url: a.url,
                    size: a.size,
                })
                .collect(),
        };
        (message, self.author.into_model())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_with_reply_attachment_and_edit() {
        let wire: WireMessage = serde_json::from_str(
            r#"{
                "id": "3", "channel_id": "2", "content": "look",
                "author": { "id": "1", "username": "alice", "global_name": null, "avatar": "abc" },
                "edited_timestamp": "2026-01-01T00:00:00+00:00",
                "message_reference": { "message_id": "2", "channel_id": "2" },
                "mentions": [{ "id": "9", "username": "bob" }],
                "attachments": [{ "id": "5", "filename": "a.png", "url": "https://x/a.png", "size": 10 }]
            }"#,
        )
        .unwrap();
        let (message, author) = wire.into_models();
        assert!(message.edited);
        assert_eq!(message.reply_to, Some(Snowflake(2)));
        assert_eq!(message.mentions, [Snowflake(9)]);
        assert_eq!(message.attachments[0].size, 10);
        assert_eq!(author.display_name(), "alice");
    }

    #[test]
    fn deduplicated_dm_recipients() {
        let wire: WireChannel =
            serde_json::from_str(r#"{ "id": "4", "type": 3, "recipient_ids": ["5", "6"] }"#).unwrap();
        let (channel, users) = wire.into_parts(None);
        assert_eq!(channel.kind, ChannelKind::GroupDm);
        assert_eq!(channel.recipients, [Snowflake(5), Snowflake(6)]);
        assert!(users.is_empty());
    }

    #[test]
    fn bot_style_guild_with_top_level_name() {
        let wire: WireGuild = serde_json::from_str(
            r#"{ "id": "1", "name": "Top", "channels": [{ "id": "2", "type": 5, "name": "news", "position": null }] }"#,
        )
        .unwrap();
        let (guild, channels) = wire.into_models();
        assert_eq!(guild.name, "Top");
        assert_eq!(channels[0].kind, ChannelKind::News);
        assert_eq!(channels[0].guild_id, Some(Snowflake(1)));
    }
}
