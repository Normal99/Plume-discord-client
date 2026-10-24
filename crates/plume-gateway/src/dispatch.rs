//! Turns raw dispatch JSON into [`plume_core::Event`]s.
//!
//! Shared JSON shapes live in `plume_core::wire`; the ones here only exist
//! in gateway events.

use plume_core::models::User;
use plume_core::wire::{WireChannel, WireGuild, WireMessage, WireUser};
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

pub fn decode(name: &str, data: &RawValue) -> Result<Decoded, serde_json::Error> {
    let json = data.get();
    let event = match name {
        "READY" => {
            let ready: WireReady = serde_json::from_str(json)?;
            return Ok(ready.into_decoded());
        }
        "RESUMED" => Event::Resumed,
        "GUILD_CREATE" => {
            let guild: WireGuild = serde_json::from_str(json)?;
            let (guild, channels) = guild.into_models();
            Event::GuildCreate { guild, channels }
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
}

impl WireReady {
    fn into_decoded(self) -> Decoded {
        let mut guilds = Vec::with_capacity(self.guilds.len());
        let mut channels = Vec::new();
        let mut users: Vec<User> = self.users.into_iter().map(WireUser::into_model).collect();

        for guild in self.guilds.into_iter().filter(|g| !g.unavailable) {
            let (guild, guild_channels) = guild.into_models();
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
            },
            session: Some(ResumeState {
                session_id: self.session_id,
                resume_url: self.resume_gateway_url,
            }),
        }
    }
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
struct WireMessageUpdate {
    id: Snowflake,
    channel_id: Snowflake,
    content: Option<String>,
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
        decode(name, &raw).unwrap()
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
    fn unknown_events_are_not_parsed() {
        let decoded = decode_str("PRESENCE_UPDATE", r#"{"anything": "goes"}"#);
        assert!(matches!(decoded.event, Event::Other(name) if name == "PRESENCE_UPDATE"));
    }
}
