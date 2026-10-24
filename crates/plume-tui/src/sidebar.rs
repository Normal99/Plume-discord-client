use std::collections::HashSet;

use plume_core::models::{Channel, ChannelKind, Guild};
use plume_core::{Snowflake, Store};

/// One row in the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    DmHeader,
    ServerHeader,
    Dm(Snowflake),
    Guild(Snowflake),
    Category(Snowflake),
    Channel(Snowflake),
}

/// The tree of DMs, servers and channels, flattened into rows.
///
/// Rebuilding is cheap (it's just sorting IDs), so instead of patching the
/// list on every change we mark it dirty and rebuild before the next draw.
pub struct Sidebar {
    entries: Vec<Entry>,
    selected: usize,
    expanded: HashSet<Snowflake>,
    dms_collapsed: bool,
    /// List channels we can't read too (with a lock), like Discord's "show all channels".
    show_locked: bool,
    dirty: bool,
    /// Entry to select after the next rebuild (it may not exist yet).
    pending_select: Option<Entry>,
}

/// DMs beyond this many are hidden; use the quick switcher (Ctrl+K) for older ones.
const MAX_DMS: usize = 40;

impl Default for Sidebar {
    fn default() -> Self {
        Sidebar {
            entries: Vec::new(),
            selected: 0,
            expanded: HashSet::new(),
            dms_collapsed: false,
            show_locked: false,
            dirty: true,
            pending_select: None,
        }
    }
}

impl Sidebar {
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn selected(&self) -> Option<Entry> {
        self.entries.get(self.selected).copied()
    }

    pub fn is_expanded(&self, guild_id: Snowflake) -> bool {
        self.expanded.contains(&guild_id)
    }

    pub fn dms_collapsed(&self) -> bool {
        self.dms_collapsed
    }

    pub fn shows_locked(&self) -> bool {
        self.show_locked
    }

    pub fn set_show_locked(&mut self, show: bool) {
        self.show_locked = show;
        self.dirty = true;
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() - 1;
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    pub fn select_last(&mut self) {
        self.selected = self.entries.len().saturating_sub(1);
    }

    pub fn toggle_dms(&mut self) {
        self.dms_collapsed = !self.dms_collapsed;
        self.dirty = true;
    }

    pub fn toggle_guild(&mut self, guild_id: Snowflake) {
        if !self.expanded.remove(&guild_id) {
            self.expanded.insert(guild_id);
        }
        self.dirty = true;
    }

    pub fn collapse_guild(&mut self, guild_id: Snowflake) {
        self.expanded.remove(&guild_id);
        self.dirty = true;
        self.pending_select = Some(Entry::Guild(guild_id));
    }

    /// Selects a channel, expanding its server if needed.
    pub fn reveal_channel(&mut self, store: &Store, channel_id: Snowflake) {
        let entry = match store.channel(channel_id) {
            Some(channel) if is_dm(channel) => {
                self.dms_collapsed = false;
                Entry::Dm(channel_id)
            }
            Some(channel) => {
                if let Some(guild_id) = channel.guild_id {
                    self.expanded.insert(guild_id);
                }
                Entry::Channel(channel_id)
            }
            None => return,
        };
        self.dirty = true;
        self.pending_select = Some(entry);
    }

    pub fn rebuild_if_dirty(&mut self, store: &Store) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let keep = self.pending_select.take().or_else(|| self.selected());

        self.entries.clear();
        self.entries.push(Entry::DmHeader);
        if !self.dms_collapsed {
            self.entries
                .extend(sorted_dms(store).into_iter().take(MAX_DMS).map(Entry::Dm));
        }
        self.entries.push(Entry::ServerHeader);
        for guild in sorted_guilds(store) {
            self.entries.push(Entry::Guild(guild.id));
            if self.expanded.contains(&guild.id) {
                self.entries.extend(guild_channels(store, guild, self.show_locked));
            }
        }

        self.selected = keep
            .and_then(|entry| self.entries.iter().position(|e| *e == entry))
            .unwrap_or(self.selected)
            .min(self.entries.len().saturating_sub(1));
    }
}

pub fn is_dm(channel: &Channel) -> bool {
    matches!(channel.kind, ChannelKind::Dm | ChannelKind::GroupDm)
}

/// DMs, most recently active first.
pub fn sorted_dms(store: &Store) -> Vec<Snowflake> {
    let mut dms: Vec<&Channel> = store.channels().filter(|c| is_dm(c)).collect();
    dms.sort_by_key(|c| std::cmp::Reverse(c.last_message_id.unwrap_or(c.id)));
    dms.into_iter().map(|c| c.id).collect()
}

pub fn sorted_guilds(store: &Store) -> Vec<&Guild> {
    let mut guilds: Vec<&Guild> = store.guilds().collect();
    guilds.sort_by_cached_key(|g| (g.name.to_lowercase(), g.id));
    guilds
}

/// A server's text channels in Discord's order: uncategorised ones first,
/// then each category followed by its channels, each channel followed by
/// its active threads (busiest first). Empty categories are skipped.
/// Channels we can't read are left out unless `show_locked` is set.
pub fn guild_channels(store: &Store, guild: &Guild, show_locked: bool) -> Vec<Entry> {
    let channels: Vec<&Channel> = guild
        .channels
        .iter()
        .filter_map(|&id| store.channel(id))
        .filter(|c| show_locked || c.kind == ChannelKind::Category || store.can_view(c.id))
        .collect();
    let order = |c: &&Channel| (c.position, c.id);
    // Forums hold their posts as threads, so they're listed like channels.
    let listed = |c: &Channel| (c.kind.is_text() || c.kind == ChannelKind::Forum) && c.kind != ChannelKind::Thread;
    let threads_of = |parent: Snowflake| {
        let mut threads: Vec<&Channel> = channels
            .iter()
            .copied()
            .filter(|c| c.kind == ChannelKind::Thread && c.parent_id == Some(parent))
            .collect();
        threads.sort_by_key(|c| std::cmp::Reverse(c.last_message_id.unwrap_or(c.id)));
        threads.into_iter().map(|c| Entry::Channel(c.id))
    };
    let with_threads = |list: Vec<&Channel>, entries: &mut Vec<Entry>| {
        for channel in list {
            entries.push(Entry::Channel(channel.id));
            entries.extend(threads_of(channel.id));
        }
    };

    let mut top: Vec<&Channel> = channels
        .iter()
        .copied()
        .filter(|c| listed(c) && c.parent_id.is_none())
        .collect();
    top.sort_by_key(order);

    let mut categories: Vec<&Channel> = channels
        .iter()
        .copied()
        .filter(|c| c.kind == ChannelKind::Category)
        .collect();
    categories.sort_by_key(order);

    let mut entries = Vec::new();
    with_threads(top, &mut entries);
    for category in categories {
        let mut children: Vec<&Channel> = channels
            .iter()
            .copied()
            .filter(|c| listed(c) && c.parent_id == Some(category.id))
            .collect();
        if children.is_empty() {
            continue;
        }
        children.sort_by_key(order);
        entries.push(Entry::Category(category.id));
        with_threads(children, &mut entries);
    }
    entries
}

/// "Alice" for a DM, the group's name, or its members for an unnamed group.
pub fn dm_name(store: &Store, channel: &Channel) -> String {
    if let Some(name) = channel.name.as_deref().filter(|n| !n.is_empty()) {
        return name.to_owned();
    }
    let names: Vec<&str> = channel
        .recipients
        .iter()
        .filter_map(|&id| store.user(id))
        .map(|u| u.display_name())
        .collect();
    if names.is_empty() {
        "(unknown)".to_owned()
    } else {
        names.join(", ")
    }
}

/// A short name for any channel: "#general" or "@Alice".
pub fn channel_label(store: &Store, channel_id: Snowflake) -> String {
    match store.channel(channel_id) {
        Some(channel) if is_dm(channel) => format!("@{}", dm_name(store, channel)),
        Some(channel) => format!("#{}", channel.name.as_deref().unwrap_or("?")),
        None => "#unknown".to_owned(),
    }
}
