//! Turns raw dispatch JSON into [`plume_core::Event`]s.
//!
//! Shared JSON shapes live in `plume_core::wire`; the ones here only exist
//! in gateway events.

use std::time::{SystemTime, UNIX_EPOCH};

use plume_core::models::User;
use plume_core::wire::{
    WireChannel, WireEmbed, WireEmoji, WireGuild, WireGuildProperties, WireGuildSettings, WireList, WireMember,
    WireMessage, WireReadState, WireRole, WireUser,
};
use plume_core::{Event, Snowflake};
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::resume::ResumeState;

/// A decoded dispatch, plus session info if it was READY.
pub struct Decoded {
    pub event: Event,
    pub session: Option<ResumeState>,
}

impl From<Event> for Decoded {
    fn from(event: Event) -> Self {
        Decoded { event, session: None }
    }
}

/// Decodes one dispatch. `me` is the logged-in user (known after READY),
/// used to find your own roles in GUILD_CREATE.
pub fn decode(name: &str, data: &RawValue, me: Option<Snowflake>) -> Result<Decoded, serde_json::Error> {
    let json = data.get();
    let event = match name {
        "READY" => {
            let ready: WireReady = serde_json::from_str(json)?;
            return Ok(ready.into_decoded());
        }
        "RESUMED" => Event::Resumed,
        "GUILD_CREATE" => {
            let guild: WireGuild = serde_json::from_str(json)?;
            let (guild, channels) = guild.into_models(me);
            Event::GuildCreate { guild, channels }
        }
        "GUILD_UPDATE" => {
            let update: WireGuildUpdate = serde_json::from_str(json)?;
            let (name, owner_id) = match update.properties {
                Some(p) => (p.name.or(update.name), p.owner_id.or(update.owner_id)),
                None => (update.name, update.owner_id),
            };
            Event::GuildUpdate {
                id: update.id,
                name,
                owner_id,
                roles: update
                    .roles
                    .map(|roles| roles.into_iter().map(WireRole::into_model).collect()),
            }
        }
        "GUILD_MEMBER_UPDATE" => {
            let update: WireMemberUpdate = serde_json::from_str(json)?;
            match update.member.user_id() {
                Some(user_id) => Event::MemberUpdate {
                    guild_id: update.guild_id,
                    user_id,
                    roles: update.member.roles,
                },
                None => Event::Other(name.to_owned()),
            }
        }
        "GUILD_ROLE_CREATE" | "GUILD_ROLE_UPDATE" => {
            let update: WireRoleUpdate = serde_json::from_str(json)?;
            Event::RoleUpsert {
                guild_id: update.guild_id,
                role: update.role.into_model(),
            }
        }
        "GUILD_ROLE_DELETE" => {
            let gone: WireRoleDelete = serde_json::from_str(json)?;
            Event::RoleDelete {
                guild_id: gone.guild_id,
                role_id: gone.role_id,
            }
        }
        "MESSAGE_REACTION_ADD" | "MESSAGE_REACTION_REMOVE" => {
            let reaction: WireReactionEvent = serde_json::from_str(json)?;
            let (channel_id, message_id, emoji) =
                (reaction.channel_id, reaction.message_id, reaction.emoji.into_model());
            let user_id = reaction.user_id;
            if name == "MESSAGE_REACTION_ADD" {
                Event::ReactionAdd {
                    channel_id,
                    message_id,
                    user_id,
                    emoji,
                }
            } else {
                Event::ReactionRemove {
                    channel_id,
                    message_id,
                    user_id,
                    emoji,
                }
            }
        }
        "MESSAGE_REACTION_REMOVE_ALL" | "MESSAGE_REACTION_REMOVE_EMOJI" => {
            let clear: WireReactionClear = serde_json::from_str(json)?;
            Event::ReactionClear {
                channel_id: clear.channel_id,
                message_id: clear.message_id,
                emoji: clear.emoji.map(WireEmoji::into_model),
            }
        }
        "MESSAGE_ACK" => {
            let ack: WireAck = serde_json::from_str(json)?;
            Event::MessageAck {
                channel_id: ack.channel_id,
                message_id: ack.message_id,
            }
        }
        "USER_GUILD_SETTINGS_UPDATE" => {
            let settings: WireGuildSettings = serde_json::from_str(json)?;
            Event::GuildSettingsUpdate(settings.into_model(now_unix()))
        }
        "GUILD_DELETE" => {
            let gone: WireGuildDelete = serde_json::from_str(json)?;
            Event::GuildDelete {
                id: gone.id,
                unavailable: gone.unavailable,
            }
        }
        "CHANNEL_CREATE" | "CHANNEL_UPDATE" => {
            let channel: WireChannel = serde_json::from_str(json)?;
            Event::ChannelUpsert(channel.into_model(None))
        }
        "THREAD_CREATE" | "THREAD_UPDATE" => {
            let thread: WireChannel = serde_json::from_str(json)?;
            if thread.is_archived() {
                Event::ChannelDelete { id: thread.id }
            } else {
                Event::ChannelUpsert(thread.into_model(None))
            }
        }
        "THREAD_DELETE" => {
            let thread: WireId = serde_json::from_str(json)?;
            Event::ChannelDelete { id: thread.id }
        }
        "THREAD_LIST_SYNC" => {
            let sync: WireThreadListSync = serde_json::from_str(json)?;
            Event::ThreadsSync {
                guild_id: sync.guild_id,
                threads: sync
                    .threads
                    .into_iter()
                    .filter(|t| !t.is_archived())
                    .map(|t| t.into_model(Some(sync.guild_id)))
                    .collect(),
            }
        }
        "CHANNEL_DELETE" => {
            let channel: WireId = serde_json::from_str(json)?;
            Event::ChannelDelete { id: channel.id }
        }
        "MESSAGE_CREATE" => {
            let message: WireMessage = serde_json::from_str(json)?;
            let (message, author) = message.into_models();
            Event::MessageCreate { message, author }
        }
        "MESSAGE_UPDATE" => {
            let update: WireMessageUpdate = serde_json::from_str(json)?;
            Event::MessageUpdate {
                id: update.id,
                channel_id: update.channel_id,
                content: update.content,
                embeds: update
                    .embeds
                    .map(|embeds| embeds.into_iter().filter_map(WireEmbed::into_model).collect()),
            }
        }
        "MESSAGE_DELETE" => {
            let gone: WireMessageDelete = serde_json::from_str(json)?;
            Event::MessageDelete {
                id: gone.id,
                channel_id: gone.channel_id,
            }
        }
        "TYPING_START" => {
            let typing: WireTypingStart = serde_json::from_str(json)?;
            Event::TypingStart {
                channel_id: typing.channel_id,
                user_id: typing.user_id,
            }
        }
        // Everything else isn't parsed at all, which costs nothing.
        other => Event::Other(other.to_owned()),
    };
    Ok(event.into())
}

#[derive(Deserialize)]
struct WireReady {
    user: WireUser,
    session_id: String,
    resume_gateway_url: String,
    #[serde(default)]
    guilds: Vec<WireGuild>,
    #[serde(default)]
    private_channels: Vec<WireChannel>,
    #[serde(default)]
    users: Vec<WireUser>,
    /// Your member object in each guild, in the same order as `guilds`.
    #[serde(default)]
    merged_members: Vec<Vec<WireMember>>,
    #[serde(default)]
    read_state: WireList<WireReadState>,
    #[serde(default)]
    user_guild_settings: WireList<WireGuildSettings>,
}

impl WireReady {
    fn into_decoded(self) -> Decoded {
        let me = self.user.id;
        let mut guilds = Vec::with_capacity(self.guilds.len());
        let mut channels = Vec::new();
        let mut users: Vec<User> = self.users.into_iter().map(WireUser::into_model).collect();

        let mut merged_members = self.merged_members.into_iter();
        for guild in self.guilds {
            // Keep the two lists in step, even for guilds we skip.
            let members = merged_members.next().unwrap_or_default();
            if guild.unavailable {
                continue;
            }
            let (mut guild, guild_channels) = guild.into_models(Some(me));
            if guild.my_roles.is_none() {
                guild.my_roles = members.into_iter().find(|m| m.user_id() == Some(me)).map(|m| m.roles);
            }
            guilds.push(guild);
            channels.extend(guild_channels);
        }
        for channel in self.private_channels {
            let (channel, recipients) = channel.into_parts(None);
            channels.push(channel);
            users.extend(recipients);
        }

        Decoded {
            event: Event::Ready {
                me: self.user.into_model(),
                guilds,
                channels,
                users,
                read_states: self
                    .read_state
                    .into_vec()
                    .into_iter()
                    .filter_map(WireReadState::into_model)
                    .collect(),
                settings: self
                    .user_guild_settings
                    .into_vec()
                    .into_iter()
                    .map(|s| s.into_model(now_unix()))
                    .collect(),
            },
            session: Some(ResumeState {
                session_id: self.session_id,
                resume_url: self.resume_gateway_url,
            }),
        }
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[derive(Deserialize)]
struct WireGuildUpdate {
    id: Snowflake,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    owner_id: Option<Snowflake>,
    #[serde(default)]
    properties: Option<WireGuildProperties>,
    #[serde(default)]
    roles: Option<Vec<WireRole>>,
}

#[derive(Deserialize)]
struct WireMemberUpdate {
    guild_id: Snowflake,
    #[serde(flatten)]
    member: WireMember,
}

#[derive(Deserialize)]
struct WireRoleUpdate {
    guild_id: Snowflake,
    role: WireRole,
}

#[derive(Deserialize)]
struct WireRoleDelete {
    guild_id: Snowflake,
    role_id: Snowflake,
}

#[derive(Deserialize)]
struct WireReactionEvent {
    channel_id: Snowflake,
    message_id: Snowflake,
    user_id: Snowflake,
    emoji: WireEmoji,
}

#[derive(Deserialize)]
struct WireReactionClear {
    channel_id: Snowflake,
    message_id: Snowflake,
    #[serde(default)]
    emoji: Option<WireEmoji>,
}

#[derive(Deserialize)]
struct WireAck {
    channel_id: Snowflake,
    message_id: Snowflake,
}

#[derive(Deserialize)]
struct WireGuildDelete {
    id: Snowflake,
    #[serde(default)]
    unavailable: bool,
}

#[derive(Deserialize)]
struct WireId {
    id: Snowflake,
}

#[derive(Deserialize)]
struct WireThreadListSync {
    guild_id: Snowflake,
    #[serde(default)]
    threads: Vec<WireChannel>,
}

#[derive(Deserialize)]
struct WireMessageUpdate {
    id: Snowflake,
    channel_id: Snowflake,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    embeds: Option<Vec<WireEmbed>>,
}

#[derive(Deserialize)]
struct WireMessageDelete {
    id: Snowflake,
    channel_id: Snowflake,
}

#[derive(Deserialize)]
struct WireTypingStart {
    channel_id: Snowflake,
    user_id: Snowflake,
}

#[cfg(test)]
mod tests {
    use super::*;
    use plume_core::models::ChannelKind;

    fn decode_str(name: &str, json: &str) -> Decoded {
        let raw = RawValue::from_string(json.to_owned()).unwrap();
        decode(name, &raw, Some(Snowflake(1))).unwrap()
    }

    #[test]
    fn decodes_thread_events() {
        let thread = |archived: bool| {
            format!(
                r#"{{"id": "50", "type": 11, "guild_id": "10", "parent_id": "100", "name": "bug hunt",
                    "thread_metadata": {{"archived": {archived}, "locked": false}}}}"#
            )
        };
        let Event::ChannelUpsert(channel) = decode_str("THREAD_CREATE", &thread(false)).event else {
            panic!("wrong event");
        };
        assert_eq!(channel.kind, plume_core::models::ChannelKind::Thread);
        assert_eq!(
            (channel.guild_id, channel.parent_id),
            (Some(Snowflake(10)), Some(Snowflake(100)))
        );
        assert!(matches!(
            decode_str("THREAD_UPDATE", &thread(true)).event,
            Event::ChannelDelete { id: Snowflake(50) }
        ));
        assert!(matches!(
            decode_str(
                "THREAD_DELETE",
                r#"{"id": "50", "guild_id": "10", "parent_id": "100", "type": 11}"#
            )
            .event,
            Event::ChannelDelete { id: Snowflake(50) }
        ));
        let sync = format!(
            r#"{{"guild_id": "10", "threads": [{}, {}], "members": []}}"#,
            thread(false),
            thread(true)
        );
        let Event::ThreadsSync { guild_id, threads } = decode_str("THREAD_LIST_SYNC", &sync).event else {
            panic!("wrong event");
        };
        assert_eq!(
            (guild_id, threads.len()),
            (Snowflake(10), 1),
            "archived threads are dropped"
        );
    }

    #[test]
    fn decodes_message_create_and_ignores_unknown_fields() {
        let decoded = decode_str(
            "MESSAGE_CREATE",
            r#"{
                "id": "3", "channel_id": "2", "content": "hi",
                "edited_timestamp": null, "some_new_field": [1, 2, 3],
                "author": { "id": "1", "username": "alice", "global_name": "Alice" }
            }"#,
        );
        let Event::MessageCreate { message, author } = decoded.event else {
            panic!("wrong event");
        };
        assert_eq!(message.content, "hi");
        assert_eq!(message.author_id, Snowflake(1));
        assert!(!message.edited);
        assert_eq!(author.display_name(), "Alice");
    }

    #[test]
    fn decodes_ready_with_session_and_guild_channels() {
        let decoded = decode_str(
            "READY",
            r#"{
                "user": { "id": "1", "username": "me" },
                "session_id": "abc",
                "resume_gateway_url": "wss://resume.example",
                "guilds": [{
                    "id": "10",
                    "properties": { "name": "My Server" },
                    "channels": [{ "id": "100", "type": 0, "name": "general", "position": 1 }]
                }],
                "private_channels": [{
                    "id": "200", "type": 1,
                    "recipients": [{ "id": "5", "username": "friend" }]
                }]
            }"#,
        );

        let session = decoded.session.expect("READY carries a session");
        assert_eq!(session.session_id, "abc");

        let Event::Ready {
            me,
            guilds,
            channels,
            users,
            ..
        } = decoded.event
        else {
            panic!("wrong event");
        };
        assert_eq!(me.username, "me");
        assert_eq!(guilds[0].name, "My Server");
        assert_eq!(guilds[0].channels, [Snowflake(100)]);
        // The guild channel learned its guild ID from the parent.
        assert_eq!(channels[0].guild_id, Some(Snowflake(10)));
        assert_eq!(channels[1].kind, ChannelKind::Dm);
        assert_eq!(channels[1].recipients, [Snowflake(5)]);
        assert_eq!(users[0].username, "friend");
    }

    #[test]
    fn ready_roles_read_states_and_mutes() {
        let decoded = decode_str(
            "READY",
            r#"{
                "user": { "id": "1", "username": "me" },
                "session_id": "abc", "resume_gateway_url": "wss://r",
                "guilds": [
                    { "id": "9", "unavailable": true },
                    { "id": "10", "properties": { "name": "S" }, "roles": [{ "id": "10", "permissions": "1024" }], "channels": [] }
                ],
                "merged_members": [[], [{ "user_id": "1", "roles": ["30"] }]],
                "read_state": { "version": 1, "partial": false, "entries": [{ "id": "100", "last_message_id": "5", "mention_count": 1 }] },
                "user_guild_settings": { "version": 1, "partial": false, "entries": [{ "guild_id": "10", "muted": true }] }
            }"#,
        );
        let Event::Ready {
            guilds,
            read_states,
            settings,
            ..
        } = decoded.event
        else {
            panic!("wrong event");
        };
        assert_eq!(guilds.len(), 1);
        assert_eq!(
            guilds[0].my_roles,
            Some(vec![Snowflake(30)]),
            "merged_members lines up with guilds"
        );
        assert_eq!(read_states[0].mentions, 1);
        assert!(settings[0].muted);
    }

    #[test]
    fn member_role_and_ack_events() {
        let member = decode_str(
            "GUILD_MEMBER_UPDATE",
            r#"{ "guild_id": "10", "user": { "id": "1" }, "roles": ["2"], "nick": null }"#,
        );
        assert_eq!(
            member.event,
            Event::MemberUpdate {
                guild_id: Snowflake(10),
                user_id: Snowflake(1),
                roles: vec![Snowflake(2)]
            }
        );
        let role = decode_str(
            "GUILD_ROLE_UPDATE",
            r#"{ "guild_id": "10", "role": { "id": "2", "permissions": "8" } }"#,
        );
        assert!(matches!(role.event, Event::RoleUpsert { role, .. } if role.permissions == 8));
        let ack = decode_str(
            "MESSAGE_ACK",
            r#"{ "channel_id": "100", "message_id": "5", "version": 3 }"#,
        );
        assert_eq!(
            ack.event,
            Event::MessageAck {
                channel_id: Snowflake(100),
                message_id: Snowflake(5)
            }
        );
    }

    #[test]
    fn unknown_events_are_not_parsed() {
        let decoded = decode_str("PRESENCE_UPDATE", r#"{"anything": "goes"}"#);
        assert!(matches!(decoded.event, Event::Other(name) if name == "PRESENCE_UPDATE"));
    }
}
