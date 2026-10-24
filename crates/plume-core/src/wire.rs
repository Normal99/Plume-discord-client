//! JSON shapes shared by the gateway and REST, and how they become models.
//!
//! Each struct lists only the fields we use; serde skips the rest without
//! allocating them, which is most of what keeps READY cheap to parse.
//! Everything optional is `#[serde(default)]` because Discord leaves fields
//! out (or sends `null`) more often than its docs suggest.

use serde::Deserialize;

use crate::Snowflake;
use crate::models::{
    Attachment, Channel, ChannelKind, Embed, Emoji, Guild, GuildSettings, Message, Overwrite, OverwriteKind, Reaction,
    ReadState, Role, User,
};

/// Permission bitfields come as strings ("1071698660929") because they
/// outgrew JavaScript numbers. Accepts plain numbers too.
pub fn permission_bits<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Bits {
        Number(u64),
        Text(String),
    }
    match Bits::deserialize(deserializer)? {
        Bits::Number(n) => Ok(n),
        Bits::Text(text) => text.parse().map_err(serde::de::Error::custom),
    }
}

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
    #[serde(default)]
    pub permission_overwrites: Vec<WireOverwrite>,
}

#[derive(Debug, Deserialize)]
pub struct WireOverwrite {
    pub id: Snowflake,
    /// 0 = role, 1 = member.
    #[serde(rename = "type", default)]
    pub kind: u8,
    #[serde(default, deserialize_with = "permission_bits")]
    pub allow: u64,
    #[serde(default, deserialize_with = "permission_bits")]
    pub deny: u64,
}

impl WireOverwrite {
    pub fn into_model(self) -> Overwrite {
        Overwrite {
            id: self.id,
            kind: if self.kind == 1 {
                OverwriteKind::Member
            } else {
                OverwriteKind::Role
            },
            allow: self.allow,
            deny: self.deny,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct WireRole {
    pub id: Snowflake,
    #[serde(default, deserialize_with = "permission_bits")]
    pub permissions: u64,
}

impl WireRole {
    pub fn into_model(self) -> Role {
        Role {
            id: self.id,
            permissions: self.permissions,
        }
    }
}

/// A guild member: who, and which roles they have.
#[derive(Debug, Deserialize)]
pub struct WireMember {
    /// Deduplicated members (READY's `merged_members`) carry just the ID...
    #[serde(default)]
    pub user_id: Option<Snowflake>,
    /// ...others carry the whole user.
    #[serde(default)]
    pub user: Option<WireMention>,
    #[serde(default)]
    pub roles: Vec<Snowflake>,
}

impl WireMember {
    pub fn user_id(&self) -> Option<Snowflake> {
        self.user_id.or(self.user.as_ref().map(|u| u.id))
    }
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
            overwrites: self
                .permission_overwrites
                .into_iter()
                .map(WireOverwrite::into_model)
                .collect(),
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
    pub owner_id: Option<Snowflake>,
    #[serde(default)]
    pub channels: Vec<WireChannel>,
    #[serde(default)]
    pub roles: Vec<WireRole>,
    /// A few members (usually including you) in GUILD_CREATE.
    #[serde(default)]
    pub members: Vec<WireMember>,
}

#[derive(Debug, Deserialize)]
pub struct WireGuildProperties {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub owner_id: Option<Snowflake>,
}

impl WireGuild {
    /// The guild and its channels. Your roles are filled in if `me` is
    /// among the members it carried.
    pub fn into_models(self, me: Option<Snowflake>) -> (Guild, Vec<Channel>) {
        let id = self.id;
        let (name, owner_id) = match self.properties {
            Some(p) => (p.name.or(self.name), p.owner_id.or(self.owner_id)),
            None => (self.name, self.owner_id),
        };
        let my_roles = me.and_then(|me| {
            self.members
                .into_iter()
                .find(|m| m.user_id() == Some(me))
                .map(|m| m.roles)
        });
        let channels: Vec<Channel> = self.channels.into_iter().map(|c| c.into_model(Some(id))).collect();
        let guild = Guild {
            id,
            name: name.unwrap_or_else(|| "(unnamed server)".to_owned()),
            channels: channels.iter().map(|c| c.id).collect(),
            owner_id,
            roles: self.roles.into_iter().map(WireRole::into_model).collect(),
            my_roles,
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
    #[serde(default)]
    pub reactions: Vec<WireReaction>,
    #[serde(default)]
    pub embeds: Vec<WireEmbed>,
}

#[derive(Debug, Deserialize)]
pub struct WireEmbed {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

impl WireEmbed {
    /// `None` for embeds with no text at all (a bare image preview).
    pub fn into_model(self) -> Option<Embed> {
        let description = self.description.filter(|d| !d.trim().is_empty()).map(|d| {
            if d.chars().count() > Embed::MAX_DESCRIPTION {
                let mut cut: String = d.chars().take(Embed::MAX_DESCRIPTION).collect();
                cut.push('…');
                cut
            } else {
                d
            }
        });
        let embed = Embed {
            title: self.title.filter(|t| !t.trim().is_empty()),
            url: self.url,
            description,
        };
        (embed.title.is_some() || embed.description.is_some()).then_some(embed)
    }
}

#[derive(Debug, Deserialize)]
pub struct WireEmoji {
    #[serde(default)]
    pub id: Option<Snowflake>,
    /// `None` for custom emoji that were deleted.
    #[serde(default)]
    pub name: Option<String>,
}

impl WireEmoji {
    pub fn into_model(self) -> Emoji {
        Emoji {
            name: self.name.unwrap_or_else(|| "?".to_owned()),
            id: self.id,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct WireReaction {
    pub emoji: WireEmoji,
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub me: bool,
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
            reactions: self
                .reactions
                .into_iter()
                .map(|r| Reaction {
                    emoji: r.emoji.into_model(),
                    count: r.count,
                    me: r.me,
                })
                .collect(),
            embeds: self.embeds.into_iter().filter_map(WireEmbed::into_model).collect(),
        };
        (message, self.author.into_model())
    }
}

/// Lists in READY come either bare or wrapped as `{"entries": [...]}`
/// (with the versioned capabilities). Accepts both.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum WireList<T> {
    Versioned { entries: Vec<T> },
    Bare(Vec<T>),
}

impl<T> Default for WireList<T> {
    fn default() -> Self {
        WireList::Bare(Vec::new())
    }
}

impl<T> WireList<T> {
    pub fn into_vec(self) -> Vec<T> {
        match self {
            WireList::Versioned { entries } | WireList::Bare(entries) => entries,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct WireReadState {
    /// A channel ID for normal read states.
    pub id: Snowflake,
    #[serde(default)]
    pub last_message_id: Option<Snowflake>,
    #[serde(default)]
    pub mention_count: u32,
    /// 0 is a channel; other types track things we don't show.
    #[serde(default)]
    pub read_state_type: u8,
}

impl WireReadState {
    pub fn into_model(self) -> Option<ReadState> {
        (self.read_state_type == 0).then(|| ReadState {
            channel_id: self.id,
            last_read: self.last_message_id.unwrap_or_default(),
            mentions: self.mention_count,
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct WireGuildSettings {
    #[serde(default)]
    pub guild_id: Option<Snowflake>,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub mute_config: Option<WireMuteConfig>,
    #[serde(default)]
    pub channel_overrides: Vec<WireChannelOverride>,
}

#[derive(Debug, Deserialize)]
pub struct WireChannelOverride {
    pub channel_id: Snowflake,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub mute_config: Option<WireMuteConfig>,
}

#[derive(Debug, Deserialize)]
pub struct WireMuteConfig {
    /// When a temporary mute ends (RFC 3339). `None` means "until I unmute".
    #[serde(default)]
    pub end_time: Option<String>,
}

/// A mute is on if it's set and hasn't run out.
fn still_muted(muted: bool, config: Option<&WireMuteConfig>, now_unix: i64) -> bool {
    let end = config.and_then(|c| c.end_time.as_deref()).and_then(parse_rfc3339);
    muted && end.is_none_or(|end| end > now_unix)
}

impl WireGuildSettings {
    pub fn into_model(self, now_unix: i64) -> GuildSettings {
        GuildSettings {
            guild_id: self.guild_id,
            muted: still_muted(self.muted, self.mute_config.as_ref(), now_unix),
            muted_channels: self
                .channel_overrides
                .into_iter()
                .filter(|o| still_muted(o.muted, o.mute_config.as_ref(), now_unix))
                .map(|o| o.channel_id)
                .collect(),
        }
    }
}

/// Seconds since the Unix epoch for `2026-01-02T03:04:05[.fff][Z|+hh:mm]`.
/// Only what Discord sends; anything else is `None`.
pub fn parse_rfc3339(text: &str) -> Option<i64> {
    let number = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Skip fractional seconds, then read the offset.
    let rest = text
        .get(19..)?
        .trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = match rest.as_bytes().first() {
        None | Some(b'Z' | b'z') => 0,
        Some(sign @ (b'+' | b'-')) => {
            let hours: i64 = rest.get(1..3)?.parse().ok()?;
            let minutes: i64 = rest.get(4..6)?.parse().ok()?;
            let seconds = hours * 3600 + minutes * 60;
            if *sign == b'+' { seconds } else { -seconds }
        }
        Some(_) => return None,
    };
    // Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
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
    fn embeds_keep_text_and_drop_bare_images() {
        let embeds: Vec<WireEmbed> = serde_json::from_str(&format!(
            r#"[{{ "type": "image", "url": "https://x/y.png" }},
                {{ "type": "rich", "title": "T", "description": "{}" }}]"#,
            "a".repeat(400)
        ))
        .unwrap();
        let embeds: Vec<Embed> = embeds.into_iter().filter_map(WireEmbed::into_model).collect();
        assert_eq!(embeds.len(), 1);
        assert_eq!(
            embeds[0].description.as_ref().unwrap().chars().count(),
            Embed::MAX_DESCRIPTION + 1
        );
    }

    #[test]
    fn guild_roles_overwrites_and_my_member() {
        let wire: WireGuild = serde_json::from_str(
            r#"{
                "id": "10",
                "properties": { "name": "S", "owner_id": "99" },
                "roles": [{ "id": "10", "permissions": "1024" }, { "id": "20", "permissions": 8 }],
                "members": [{ "user": { "id": "1" }, "roles": ["20"] }],
                "channels": [{
                    "id": "100", "type": 0,
                    "permission_overwrites": [{ "id": "1", "type": 1, "allow": "0", "deny": "1024" }]
                }]
            }"#,
        )
        .unwrap();
        let (guild, channels) = wire.into_models(Some(Snowflake(1)));
        assert_eq!(guild.owner_id, Some(Snowflake(99)));
        assert_eq!(guild.roles[0].permissions, 1024);
        assert_eq!(guild.roles[1].permissions, 8);
        assert_eq!(guild.my_roles, Some(vec![Snowflake(20)]));
        assert_eq!(channels[0].overwrites[0].kind, OverwriteKind::Member);
        assert_eq!(channels[0].overwrites[0].deny, 1024);
    }

    #[test]
    fn rfc3339_dates() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-10-24T00:00:00.123456+00:00"), Some(1_792_800_000));
        assert_eq!(parse_rfc3339("2026-10-24T02:00:00+02:00"), Some(1_792_800_000));
        assert_eq!(parse_rfc3339("nonsense"), None);
    }

    #[test]
    fn expired_mutes_are_off() {
        let settings: WireGuildSettings = serde_json::from_str(
            r#"{
                "guild_id": "10", "muted": true, "mute_config": { "end_time": "2000-01-01T00:00:00Z" },
                "channel_overrides": [
                    { "channel_id": "100", "muted": true, "mute_config": null },
                    { "channel_id": "101", "muted": false },
                    { "channel_id": "102", "muted": true, "mute_config": { "end_time": "2999-01-01T00:00:00Z" } }
                ]
            }"#,
        )
        .unwrap();
        let settings = settings.into_model(1_792_800_000);
        assert!(!settings.muted);
        assert_eq!(settings.muted_channels, [Snowflake(100), Snowflake(102)]);
    }

    #[test]
    fn read_states_bare_or_versioned() {
        let versioned: WireList<WireReadState> = serde_json::from_str(
            r#"{ "version": 3, "partial": false, "entries": [
                { "id": "100", "last_message_id": "55", "mention_count": 2 },
                { "id": "7", "read_state_type": 1, "last_message_id": 0 }
            ] }"#,
        )
        .unwrap();
        let states: Vec<ReadState> = versioned
            .into_vec()
            .into_iter()
            .filter_map(WireReadState::into_model)
            .collect();
        assert_eq!(
            states,
            [ReadState {
                channel_id: Snowflake(100),
                last_read: Snowflake(55),
                mentions: 2
            }]
        );

        let bare: WireList<WireReadState> =
            serde_json::from_str(r#"[{ "id": "1", "last_message_id": null }]"#).unwrap();
        assert_eq!(bare.into_vec().len(), 1);
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
        let (guild, channels) = wire.into_models(None);
        assert_eq!(guild.name, "Top");
        assert_eq!(channels[0].kind, ChannelKind::News);
        assert_eq!(channels[0].guild_id, Some(Snowflake(1)));
    }
}
