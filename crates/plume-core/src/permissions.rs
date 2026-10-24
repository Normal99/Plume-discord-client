//! Discord's permission math, just enough to know which channels you can see.
//! See <https://discord.com/developers/docs/topics/permissions>.

use crate::Snowflake;
use crate::models::{Channel, Guild, OverwriteKind};

pub const ADMINISTRATOR: u64 = 1 << 3;
pub const VIEW_CHANNEL: u64 = 1 << 10;
pub const SEND_MESSAGES: u64 = 1 << 11;
pub const READ_MESSAGE_HISTORY: u64 = 1 << 16;
pub const ALL: u64 = u64::MAX;

/// Your permissions in a server channel. `None` if we don't know your
/// roles yet.
pub fn channel_permissions(guild: &Guild, channel: &Channel, me: Snowflake) -> Option<u64> {
    let my_roles = guild.my_roles.as_deref()?;
    if guild.owner_id == Some(me) {
        return Some(ALL);
    }

    // The @everyone role has the same ID as the server.
    let role_permissions = |id: Snowflake| guild.roles.iter().find(|r| r.id == id).map_or(0, |r| r.permissions);
    let mut permissions = role_permissions(guild.id);
    for &role in my_roles {
        permissions |= role_permissions(role);
    }
    if permissions & ADMINISTRATOR != 0 {
        return Some(ALL);
    }

    // Overwrites apply in order: @everyone, then all your roles together,
    // then you personally.
    let overwrites = &channel.overwrites;
    if let Some(everyone) = overwrites.iter().find(|o| o.id == guild.id) {
        permissions = (permissions & !everyone.deny) | everyone.allow;
    }
    let (mut allow, mut deny) = (0, 0);
    for overwrite in overwrites
        .iter()
        .filter(|o| o.kind == OverwriteKind::Role && my_roles.contains(&o.id))
    {
        allow |= overwrite.allow;
        deny |= overwrite.deny;
    }
    permissions = (permissions & !deny) | allow;
    if let Some(mine) = overwrites
        .iter()
        .find(|o| o.kind == OverwriteKind::Member && o.id == me)
    {
        permissions = (permissions & !mine.deny) | mine.allow;
    }
    Some(permissions)
}

/// Whether the channel should be listed. Unknown means yes.
pub fn can_view(guild: &Guild, channel: &Channel, me: Snowflake) -> bool {
    channel_permissions(guild, channel, me).is_none_or(|p| p & VIEW_CHANNEL != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Overwrite, Role};

    const ME: Snowflake = Snowflake(1);
    const GUILD: Snowflake = Snowflake(10);
    const MOD: Snowflake = Snowflake(20);

    fn guild(everyone: u64, my_roles: Option<Vec<Snowflake>>) -> Guild {
        Guild {
            id: GUILD,
            roles: vec![
                Role {
                    id: GUILD,
                    permissions: everyone,
                },
                Role {
                    id: MOD,
                    permissions: VIEW_CHANNEL | SEND_MESSAGES,
                },
            ],
            my_roles,
            ..Guild::default()
        }
    }

    fn channel(overwrites: Vec<Overwrite>) -> Channel {
        Channel {
            id: Snowflake(100),
            guild_id: Some(GUILD),
            overwrites,
            ..Channel::default()
        }
    }

    fn overwrite(id: Snowflake, kind: OverwriteKind, allow: u64, deny: u64) -> Overwrite {
        Overwrite { id, kind, allow, deny }
    }

    #[test]
    fn everyone_role_and_its_overwrite() {
        let g = guild(VIEW_CHANNEL, Some(vec![]));
        assert!(can_view(&g, &channel(vec![]), ME));
        let private = channel(vec![overwrite(GUILD, OverwriteKind::Role, 0, VIEW_CHANNEL)]);
        assert!(!can_view(&g, &private, ME));
    }

    #[test]
    fn role_overwrites_beat_everyone_and_member_overwrites_beat_roles() {
        let staff_only = vec![
            overwrite(GUILD, OverwriteKind::Role, 0, VIEW_CHANNEL),
            overwrite(MOD, OverwriteKind::Role, VIEW_CHANNEL, 0),
        ];
        assert!(!can_view(
            &guild(VIEW_CHANNEL, Some(vec![])),
            &channel(staff_only.clone()),
            ME
        ));
        assert!(can_view(
            &guild(VIEW_CHANNEL, Some(vec![MOD])),
            &channel(staff_only.clone()),
            ME
        ));

        let mut banned = staff_only;
        banned.push(overwrite(ME, OverwriteKind::Member, 0, VIEW_CHANNEL));
        assert!(!can_view(&guild(VIEW_CHANNEL, Some(vec![MOD])), &channel(banned), ME));
    }

    #[test]
    fn owners_and_admins_see_everything() {
        let hidden = channel(vec![overwrite(GUILD, OverwriteKind::Role, 0, VIEW_CHANNEL)]);
        let mut owned = guild(0, Some(vec![]));
        owned.owner_id = Some(ME);
        assert!(can_view(&owned, &hidden, ME));

        let mut admin = guild(0, Some(vec![MOD]));
        admin.roles[1].permissions = ADMINISTRATOR;
        assert!(can_view(&admin, &hidden, ME));
    }

    #[test]
    fn unknown_roles_show_the_channel() {
        let hidden = channel(vec![overwrite(GUILD, OverwriteKind::Role, 0, VIEW_CHANNEL)]);
        assert!(can_view(&guild(0, None), &hidden, ME));
    }
}
