//! The single source of truth every frontend reads from.
//!
//! The store is plain data with no locking: the UI loop owns it and applies
//! [`Event`]s from the gateway one at a time. Memory is bounded by the
//! [`Profile`]: only a few channels keep messages, and each keeps a capped
//! number of the newest ones.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};

use crate::models::{Channel, Guild, Message, User};
use crate::{Event, Profile, Snowflake};

/// Unread state for a channel (or, summed, for a whole server).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Unread {
    pub messages: u32,
    /// Messages that pinged you: `@you`, `@everyone`, or anything in a DM.
    pub mentions: u32,
}

/// One channel's cached messages, oldest first.
#[derive(Debug, Default)]
struct History {
    messages: VecDeque<Message>,
    /// The newest page has been fetched. Until then `messages` only holds
    /// what arrived live while the fetch was running.
    loaded: bool,
}

pub struct Store {
    profile: Profile,
    me: Option<User>,
    guilds: HashMap<Snowflake, Guild>,
    channels: HashMap<Snowflake, Channel>,
    users: HashMap<Snowflake, User>,
    histories: HashMap<Snowflake, History>,
    /// Channels with a history, most recently opened first.
    recent: VecDeque<Snowflake>,
    active: Option<Snowflake>,
    unread: HashMap<Snowflake, Unread>,
}

impl Store {
    pub fn new(profile: Profile) -> Self {
        Store {
            profile,
            me: None,
            guilds: HashMap::new(),
            channels: HashMap::new(),
            users: HashMap::new(),
            histories: HashMap::new(),
            recent: VecDeque::new(),
            active: None,
            unread: HashMap::new(),
        }
    }

    // ---- Reading ------------------------------------------------------------

    pub fn profile(&self) -> Profile {
        self.profile
    }

    pub fn me(&self) -> Option<&User> {
        self.me.as_ref()
    }

    pub fn user(&self, id: Snowflake) -> Option<&User> {
        self.users.get(&id)
    }

    pub fn guild(&self, id: Snowflake) -> Option<&Guild> {
        self.guilds.get(&id)
    }

    pub fn guilds(&self) -> impl Iterator<Item = &Guild> {
        self.guilds.values()
    }

    pub fn channel(&self, id: Snowflake) -> Option<&Channel> {
        self.channels.get(&id)
    }

    pub fn channels(&self) -> impl Iterator<Item = &Channel> {
        self.channels.values()
    }

    pub fn active_channel(&self) -> Option<Snowflake> {
        self.active
    }

    /// A channel's cached messages, oldest first. Empty if none are cached.
    pub fn messages(&self, channel_id: Snowflake) -> impl DoubleEndedIterator<Item = &Message> + ExactSizeIterator {
        self.histories
            .get(&channel_id)
            .map(|h| h.messages.iter())
            .unwrap_or_default()
    }

    /// One cached message.
    pub fn message(&self, channel_id: Snowflake, id: Snowflake) -> Option<&Message> {
        let messages = &self.histories.get(&channel_id)?.messages;
        let index = messages.binary_search_by_key(&id, |m| m.id).ok()?;
        messages.get(index)
    }

    /// Whether the newest messages still need fetching for this channel.
    pub fn needs_history(&self, channel_id: Snowflake) -> bool {
        !self.histories.get(&channel_id).is_some_and(|h| h.loaded)
    }

    pub fn unread(&self, channel_id: Snowflake) -> Unread {
        self.unread.get(&channel_id).copied().unwrap_or_default()
    }

    /// Unread state summed over a server's channels.
    pub fn guild_unread(&self, guild_id: Snowflake) -> Unread {
        let Some(guild) = self.guilds.get(&guild_id) else {
            return Unread::default();
        };
        guild
            .channels
            .iter()
            .filter_map(|id| self.unread.get(id))
            .fold(Unread::default(), |sum, u| Unread {
                messages: sum.messages + u.messages,
                mentions: sum.mentions + u.mentions,
            })
    }

    // ---- Changing -----------------------------------------------------------

    /// Makes a channel the one being looked at and marks it read. Returns
    /// true if its messages need fetching.
    pub fn open_channel(&mut self, channel_id: Snowflake) -> bool {
        self.active = Some(channel_id);
        self.unread.remove(&channel_id);

        self.recent.retain(|&id| id != channel_id);
        self.recent.push_front(channel_id);
        while self.recent.len() > self.profile.channels_in_memory.max(1) {
            if let Some(evicted) = self.recent.pop_back() {
                self.histories.remove(&evicted);
            }
        }
        !self.histories.entry(channel_id).or_default().loaded
    }

    /// Stores a page of fetched messages (any order), merged with whatever
    /// arrived live in the meantime.
    pub fn insert_history(&mut self, channel_id: Snowflake, page: Vec<(Message, User)>) {
        let limit = self.profile.messages_per_channel.max(1);
        let mut incoming = Vec::with_capacity(page.len());
        for (message, author) in page {
            self.users.insert(author.id, author);
            incoming.push(message);
        }
        // A channel closed (and evicted) while loading isn't worth keeping.
        if !self.recent.contains(&channel_id) {
            return;
        }
        let history = self.histories.entry(channel_id).or_default();
        history.loaded = true;

        let mut merged: Vec<Message> = std::mem::take(&mut history.messages).into();
        merged.extend(incoming);
        merged.sort_by_key(|m| m.id);
        // Keeps the first copy of duplicates. Live copies were stored first,
        // and they're at least as fresh as the fetched ones.
        merged.dedup_by_key(|m| m.id);
        let excess = merged.len().saturating_sub(limit);
        history.messages = merged.into_iter().skip(excess).collect();

        if let Some(newest) = history.messages.back().map(|m| m.id)
            && let Some(channel) = self.channels.get_mut(&channel_id)
        {
            channel.last_message_id = channel.last_message_id.max(Some(newest));
        }
        self.prune_users();
    }

    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Ready {
                me,
                guilds,
                channels,
                users,
            } => {
                self.guilds = guilds.into_iter().map(|g| (g.id, g)).collect();
                self.channels = channels.into_iter().map(|c| (c.id, c)).collect();
                for user in users {
                    self.users.insert(user.id, user);
                }
                self.users.insert(me.id, me.clone());
                self.me = Some(me);
                // We may have missed messages while disconnected.
                for history in self.histories.values_mut() {
                    history.loaded = false;
                }
                let channels = &self.channels;
                self.unread.retain(|id, _| channels.contains_key(id));
            }
            Event::Resumed | Event::TypingStart { .. } | Event::Other(_) => {}
            Event::GuildCreate { guild, channels } => {
                if let Some(old) = self.guilds.remove(&guild.id) {
                    for id in old.channels {
                        self.channels.remove(&id);
                    }
                }
                for channel in channels {
                    self.channels.insert(channel.id, channel);
                }
                self.guilds.insert(guild.id, guild);
            }
            Event::GuildDelete { id, unavailable } => {
                // During an outage the server is still there; keep showing it.
                if !unavailable && let Some(guild) = self.guilds.remove(&id) {
                    for channel_id in guild.channels {
                        self.forget_channel(channel_id);
                    }
                }
            }
            Event::ChannelUpsert(mut channel) => {
                if let Some(guild_id) = channel.guild_id
                    && let Some(guild) = self.guilds.get_mut(&guild_id)
                    && !guild.channels.contains(&channel.id)
                {
                    guild.channels.push(channel.id);
                }
                match self.channels.entry(channel.id) {
                    Entry::Occupied(mut slot) => {
                        channel.last_message_id = channel.last_message_id.max(slot.get().last_message_id);
                        slot.insert(channel);
                    }
                    Entry::Vacant(slot) => {
                        slot.insert(channel);
                    }
                }
            }
            Event::ChannelDelete { id } => {
                if let Some(guild_id) = self.channels.get(&id).and_then(|c| c.guild_id)
                    && let Some(guild) = self.guilds.get_mut(&guild_id)
                {
                    guild.channels.retain(|&c| c != id);
                }
                self.forget_channel(id);
            }
            Event::MessageCreate { message, author } => self.add_message(message, author),
            Event::MessageUpdate {
                id,
                channel_id,
                content,
            } => {
                if let Some(content) = content
                    && let Some(message) = self.message_mut(channel_id, id)
                    && message.content != content
                {
                    message.content = content;
                    message.edited = true;
                }
            }
            Event::MessageDelete { id, channel_id } => {
                if let Some(history) = self.histories.get_mut(&channel_id)
                    && let Ok(index) = history.messages.binary_search_by_key(&id, |m| m.id)
                {
                    history.messages.remove(index);
                }
            }
        }
    }

    fn add_message(&mut self, message: Message, author: User) {
        let channel_id = message.channel_id;
        let me = self.me.as_ref().map(|u| u.id);
        let from_me = Some(message.author_id) == me;

        let is_dm = match self.channels.get_mut(&channel_id) {
            Some(channel) => {
                channel.last_message_id = channel.last_message_id.max(Some(message.id));
                channel.kind.is_private()
            }
            None => false,
        };

        if from_me {
            // Sent from here or another device: either way, it's been read.
            self.unread.remove(&channel_id);
        } else if Some(channel_id) != self.active {
            let pings_me = is_dm || message.mention_everyone || me.is_some_and(|me| message.mentions.contains(&me));
            let unread = self.unread.entry(channel_id).or_default();
            unread.messages = unread.messages.saturating_add(1);
            if pings_me {
                unread.mentions = unread.mentions.saturating_add(1);
            }
        }

        let Some(history) = self.histories.get_mut(&channel_id) else {
            // Not cached, so there's nothing to add it to. It'll be fetched
            // with the rest when the channel is opened.
            return;
        };
        self.users.insert(author.id, author);
        let messages = &mut history.messages;
        match messages.back() {
            None => messages.push_back(message),
            Some(last) if last.id < message.id => messages.push_back(message),
            // Out of order (rare): put it in its place, unless it's a duplicate
            // (our own sent message coming back from the gateway).
            Some(_) => {
                if let Err(index) = messages.binary_search_by_key(&message.id, |m| m.id) {
                    messages.insert(index, message);
                }
            }
        }
        if messages.len() > self.profile.messages_per_channel.max(1) {
            messages.pop_front();
        }
    }

    fn message_mut(&mut self, channel_id: Snowflake, id: Snowflake) -> Option<&mut Message> {
        let messages = &mut self.histories.get_mut(&channel_id)?.messages;
        let index = messages.binary_search_by_key(&id, |m| m.id).ok()?;
        messages.get_mut(index)
    }

    fn forget_channel(&mut self, id: Snowflake) {
        self.channels.remove(&id);
        self.histories.remove(&id);
        self.unread.remove(&id);
        self.recent.retain(|&c| c != id);
        if self.active == Some(id) {
            self.active = None;
        }
    }

    /// Message authors pile up as channels are opened and closed. When the
    /// map has grown well past what the cached messages need, drop the
    /// people nothing refers to any more.
    fn prune_users(&mut self) {
        let cached: usize = self.histories.values().map(|h| h.messages.len()).sum();
        let floor = 512 + 4 * cached;
        if self.users.len() <= floor {
            return;
        }
        let mut keep: std::collections::HashSet<Snowflake> = self
            .channels
            .values()
            .flat_map(|c| c.recipients.iter().copied())
            .collect();
        for history in self.histories.values() {
            for message in &history.messages {
                keep.insert(message.author_id);
                keep.extend(message.mentions.iter().copied());
            }
        }
        if let Some(me) = &self.me {
            keep.insert(me.id);
        }
        self.users.retain(|id, _| keep.contains(id));
        self.users.shrink_to_fit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ChannelKind;

    fn user(id: u64) -> User {
        User {
            id: Snowflake(id),
            username: format!("user{id}"),
            global_name: None,
        }
    }

    fn channel(id: u64, guild: Option<u64>, kind: ChannelKind) -> Channel {
        Channel {
            id: Snowflake(id),
            guild_id: guild.map(Snowflake),
            name: Some(format!("c{id}")),
            kind,
            parent_id: None,
            position: 0,
            recipients: Vec::new(),
            last_message_id: None,
        }
    }

    fn message(id: u64, channel: u64, author: u64) -> Message {
        Message {
            id: Snowflake(id),
            channel_id: Snowflake(channel),
            author_id: Snowflake(author),
            content: format!("message {id}"),
            edited: false,
            reply_to: None,
            mentions: Vec::new(),
            mention_everyone: false,
            attachments: Vec::new(),
        }
    }

    fn ready_store(profile: Profile) -> Store {
        let mut store = Store::new(profile);
        store.apply(Event::Ready {
            me: user(1),
            guilds: vec![Guild {
                id: Snowflake(10),
                name: "g".into(),
                channels: vec![Snowflake(100), Snowflake(101), Snowflake(102)],
            }],
            channels: vec![
                channel(100, Some(10), ChannelKind::Text),
                channel(101, Some(10), ChannelKind::Text),
                channel(102, Some(10), ChannelKind::Text),
                channel(200, None, ChannelKind::Dm),
            ],
            users: vec![user(2)],
        });
        store
    }

    fn create(store: &mut Store, message: Message) {
        let author = user(message.author_id.0);
        store.apply(Event::MessageCreate { message, author });
    }

    fn ids(store: &Store, channel: u64) -> Vec<u64> {
        store.messages(Snowflake(channel)).map(|m| m.id.0).collect()
    }

    #[test]
    fn live_messages_during_a_fetch_are_merged_without_duplicates() {
        let mut store = ready_store(Profile::STANDARD);
        assert!(store.open_channel(Snowflake(100)), "needs history");
        create(&mut store, message(5, 100, 2)); // arrives while fetching
        let page = (1..=5).map(|id| (message(id, 100, 2), user(2))).rev().collect();
        store.insert_history(Snowflake(100), page);

        assert_eq!(ids(&store, 100), [1, 2, 3, 4, 5]);
        assert!(!store.needs_history(Snowflake(100)));
        assert_eq!(
            store.message(Snowflake(100), Snowflake(3)).unwrap().content,
            "message 3"
        );
        assert_eq!(
            store.channel(Snowflake(100)).unwrap().last_message_id,
            Some(Snowflake(5))
        );
    }

    #[test]
    fn our_own_message_is_not_added_twice() {
        let mut store = ready_store(Profile::STANDARD);
        store.open_channel(Snowflake(100));
        store.insert_history(Snowflake(100), vec![(message(1, 100, 2), user(2))]);
        create(&mut store, message(7, 100, 1)); // REST reply
        create(&mut store, message(6, 100, 2)); // out of order
        create(&mut store, message(7, 100, 1)); // gateway echo
        assert_eq!(ids(&store, 100), [1, 6, 7]);
    }

    #[test]
    fn unread_and_mentions_outside_the_open_channel() {
        let mut store = ready_store(Profile::STANDARD);
        store.open_channel(Snowflake(100));
        create(&mut store, message(1, 100, 2)); // open channel: stays read
        create(&mut store, message(2, 101, 2));
        create(
            &mut store,
            Message {
                mentions: vec![Snowflake(1)],
                ..message(3, 101, 2)
            },
        );
        create(
            &mut store,
            Message {
                mention_everyone: true,
                ..message(4, 102, 2)
            },
        );
        create(&mut store, message(5, 200, 2)); // DMs always ping

        assert_eq!(store.unread(Snowflake(100)), Unread::default());
        assert_eq!(
            store.unread(Snowflake(101)),
            Unread {
                messages: 2,
                mentions: 1
            }
        );
        assert_eq!(
            store.guild_unread(Snowflake(10)),
            Unread {
                messages: 3,
                mentions: 2
            }
        );
        assert_eq!(store.unread(Snowflake(200)).mentions, 1);

        // Writing in a channel (from any device) means you've read it.
        create(&mut store, message(6, 101, 1));
        assert_eq!(store.unread(Snowflake(101)), Unread::default());
        // Opening it does too.
        store.open_channel(Snowflake(102));
        assert_eq!(store.guild_unread(Snowflake(10)), Unread::default());
    }

    #[test]
    fn memory_is_bounded_by_the_profile() {
        let mut store = ready_store(Profile::FEATHER); // 2 channels × 50
        store.open_channel(Snowflake(100));
        store.insert_history(
            Snowflake(100),
            (1..=80).map(|id| (message(id, 100, 2), user(2))).collect(),
        );
        assert_eq!(store.messages(Snowflake(100)).len(), 50);
        assert_eq!(ids(&store, 100).first(), Some(&31), "the newest are kept");
        for id in 81..=100 {
            create(&mut store, message(id, 100, 2));
        }
        assert_eq!(store.messages(Snowflake(100)).len(), 50);

        store.open_channel(Snowflake(101));
        store.open_channel(Snowflake(102)); // 100 is now the oldest of three: evicted
        assert_eq!(store.messages(Snowflake(100)).len(), 0);
        assert!(store.open_channel(Snowflake(100)), "evicted channels fetch again");
    }

    #[test]
    fn history_for_an_evicted_channel_is_dropped() {
        let mut store = ready_store(Profile::FEATHER);
        store.open_channel(Snowflake(100));
        store.open_channel(Snowflake(101));
        store.open_channel(Snowflake(102)); // evicts 100 before its fetch returns
        store.insert_history(Snowflake(100), vec![(message(1, 100, 2), user(2))]);
        assert_eq!(store.messages(Snowflake(100)).len(), 0);
    }

    #[test]
    fn edits_and_deletes() {
        let mut store = ready_store(Profile::STANDARD);
        store.open_channel(Snowflake(100));
        store.insert_history(
            Snowflake(100),
            (1..=3).map(|id| (message(id, 100, 2), user(2))).collect(),
        );

        let update = |content: Option<&str>| Event::MessageUpdate {
            id: Snowflake(2),
            channel_id: Snowflake(100),
            content: content.map(str::to_owned),
        };
        store.apply(update(None)); // an embed loading isn't an edit
        assert!(!store.message(Snowflake(100), Snowflake(2)).unwrap().edited);
        store.apply(update(Some("changed")));
        let edited = store.message(Snowflake(100), Snowflake(2)).unwrap();
        assert_eq!((edited.content.as_str(), edited.edited), ("changed", true));

        store.apply(Event::MessageDelete {
            id: Snowflake(2),
            channel_id: Snowflake(100),
        });
        store.apply(Event::MessageDelete {
            id: Snowflake(99),
            channel_id: Snowflake(100),
        }); // unknown: ignored
        assert_eq!(ids(&store, 100), [1, 3]);
    }

    #[test]
    fn channel_and_guild_changes() {
        let mut store = ready_store(Profile::STANDARD);
        store.apply(Event::ChannelUpsert(channel(103, Some(10), ChannelKind::Text)));
        assert!(store.guild(Snowflake(10)).unwrap().channels.contains(&Snowflake(103)));

        store.open_channel(Snowflake(101));
        store.apply(Event::ChannelDelete { id: Snowflake(101) });
        assert!(store.channel(Snowflake(101)).is_none());
        assert_eq!(store.active_channel(), None, "a deleted channel can't stay open");
        assert!(!store.guild(Snowflake(10)).unwrap().channels.contains(&Snowflake(101)));

        store.apply(Event::GuildDelete {
            id: Snowflake(10),
            unavailable: true,
        });
        assert!(store.guild(Snowflake(10)).is_some(), "outages don't remove servers");
        store.apply(Event::GuildDelete {
            id: Snowflake(10),
            unavailable: false,
        });
        assert!(store.guild(Snowflake(10)).is_none());
        assert!(store.channel(Snowflake(100)).is_none());
        assert!(store.channel(Snowflake(200)).is_some(), "DMs aren't part of a server");
    }

    #[test]
    fn a_new_session_refetches_cached_channels() {
        let mut store = ready_store(Profile::STANDARD);
        store.open_channel(Snowflake(100));
        store.insert_history(Snowflake(100), vec![(message(1, 100, 2), user(2))]);
        assert!(!store.needs_history(Snowflake(100)));
        store.apply(Event::Ready {
            me: user(1),
            guilds: Vec::new(),
            channels: Vec::new(),
            users: Vec::new(),
        });
        assert!(store.needs_history(Snowflake(100)));
        assert_eq!(ids(&store, 100), [1], "old messages stay visible until the refetch");
    }
}
