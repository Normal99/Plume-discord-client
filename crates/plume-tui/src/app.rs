use std::collections::HashMap;
use std::error::Error;
use std::io::stdout;
use std::time::{Duration, Instant};

use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange, Event as TermEvent,
    EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use futures_util::StreamExt;
use plume_core::models::{ChannelKind, Emoji, Message, User};
use plume_core::{Event, Snowflake, Store, Tier, Token};
use plume_gateway::{Gateway, GatewayError, GatewayHandle};
use plume_rest::{RestClient, RestError};
use ratatui::widgets::ListState;
use tokio::sync::mpsc;
use tracing::warn;

use crate::Options;
use crate::input::Input;
use crate::render;
use crate::sidebar::{self, Entry, Sidebar};
use crate::text;

/// Discord's limit for one message (without Nitro).
const MAX_MESSAGE_LENGTH: usize = 2000;
/// Discord shows "is typing" for about 10 seconds per trigger.
const TYPING_TIMEOUT: Duration = Duration::from_secs(10);
/// So we don't re-send "typing" on every key press.
const TYPING_RESEND: Duration = Duration::from_secs(8);
const ERROR_TIMEOUT: Duration = Duration::from_secs(6);
/// Read acks for the open channel are batched at most this often.
const ACK_INTERVAL: Duration = Duration::from_secs(3);
/// Messages fetched per page when scrolling up.
const OLDER_PAGE: u8 = 50;
/// At most one notification per channel this often, so a busy DM doesn't spam.
const NOTIFY_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Input,
    /// Picking a message to reply to, edit, delete or react to.
    Messages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connection {
    Connecting,
    Online,
    Reconnecting,
}

/// What Enter in the input box does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compose {
    Message,
    Reply(Snowflake),
    Edit(Snowflake),
    /// The box holds an emoji to react with.
    React(Snowflake),
}

/// What a cached [`text::ChannelView`] was built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewKey {
    pub channel: Snowflake,
    pub width: u16,
    pub revision: u64,
    pub minute: i64,
}

/// The Ctrl+K popup.
pub struct Switcher {
    pub input: Input,
    /// (channel, label) pairs matching the query, best first.
    pub matches: Vec<(Snowflake, String)>,
    pub selected: usize,
    /// Every channel we could jump to: (channel, label, lowercase label).
    all: Vec<(Snowflake, String, String)>,
}

/// Results of background work (REST calls), sent back to the UI loop.
enum TaskResult {
    History {
        channel_id: Snowflake,
        result: Result<Vec<(Message, User)>, RestError>,
    },
    Older {
        channel_id: Snowflake,
        result: Result<Vec<(Message, User)>, RestError>,
    },
    Sent {
        text: String,
        result: Result<(Message, User), RestError>,
    },
    Edited {
        text: String,
        result: Result<(Message, User), RestError>,
    },
    /// Something with nothing to show on success (delete, react).
    Done {
        action: &'static str,
        result: Result<(), RestError>,
    },
    /// Something failed before reaching Discord (e.g. reading a file).
    Failed { text: String, error: String },
    /// There's no desktop notification service; ask the terminal instead.
    NotifyInTerminal { title: String, body: String },
}

/// Everything the UI shows and remembers. `render.rs` only reads it, apart
/// from the scroll position and a few measurements.
pub struct App {
    pub store: Store,
    pub sidebar: Sidebar,
    pub sidebar_state: ListState,
    pub focus: Focus,
    pub input: Input,
    pub compose: Compose,
    /// The highlighted message while `focus` is `Messages`.
    pub selected: Option<Snowflake>,
    /// Waiting for y/n before deleting this message.
    pub confirm_delete: Option<Snowflake>,
    /// Lines scrolled up from the newest message.
    pub scroll: usize,
    /// Set by the renderer so PgUp/PgDn can move by half a screen.
    pub messages_height: u16,
    /// Set by the renderer: the oldest loaded message is on screen.
    pub at_top: bool,
    pub loading_history: bool,
    pub loading_older: bool,
    pub connection: Connection,
    pub error: Option<(String, Instant)>,
    pub memory_kb: Option<u64>,
    pub options_tier: Tier,
    pub switcher: Option<Switcher>,
    /// The open channel's rendered lines, reused until something changes.
    pub view_cache: Option<(ViewKey, text::ChannelView)>,

    rest: RestClient,
    gateway: GatewayHandle,
    tasks: mpsc::UnboundedSender<TaskResult>,
    /// Who's typing where, and when we last heard about it.
    typing: HashMap<Snowflake, Vec<(Snowflake, Instant)>>,
    last_typing_sent: Option<(Snowflake, Instant)>,
    last_ack: Option<Instant>,
    /// The terminal window has focus (if the terminal reports it). Channels
    /// aren't marked read while you're looking elsewhere.
    terminal_focused: bool,
    silent_typing: bool,
    notifications: bool,
    last_notified: HashMap<Snowflake, Instant>,
    history_limit: u8,
    ticks: u32,
    quit: bool,
    /// Something changed, so redraw before waiting for the next event.
    dirty: bool,
}

pub async fn run(token: Token, options: Options) -> Result<(), Box<dyn Error>> {
    let rest = RestClient::new(&token)?;
    let (events_tx, mut gateway_events) = mpsc::channel(256);
    let gateway = Gateway::new(token, events_tx);
    let handle = gateway.handle();
    let mut gateway_task = tokio::spawn(gateway.run());
    let (tasks_tx, mut task_results) = mpsc::unbounded_channel();

    let mut app = App::new(rest, handle, tasks_tx, &options);

    // From here on the terminal is ours; `ratatui::restore` must always run.
    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(stdout(), EnableBracketedPaste, EnableFocusChange);
    let mut terminal_events = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_secs(1));

    let result: Result<(), Box<dyn Error>> = loop {
        if app.dirty {
            app.dirty = false;
            app.sidebar.rebuild_if_dirty(&app.store);
            // Reading /proc is a few microseconds, so the number shown is always current.
            app.memory_kb = resident_memory_kb();
            if let Err(error) = terminal.draw(|frame| render::draw(frame, &mut app)) {
                break Err(error.into());
            }
        }

        tokio::select! {
            Some(event) = terminal_events.next() => match event {
                Ok(event) => app.on_terminal(event),
                Err(error) => break Err(error.into()),
            },
            Some(event) = gateway_events.recv() => app.on_gateway(event),
            Some(result) = task_results.recv() => app.on_task(result),
            _ = ticker.tick() => app.on_tick(),
            finished = &mut gateway_task => break match finished {
                Ok(Ok(())) => Ok(()),
                Ok(Err(GatewayError::AuthenticationFailed)) => {
                    Err("Discord rejected the token. Run `plume-tui logout`, then `plume-tui login`.".into())
                }
                Ok(Err(error)) => Err(error.into()),
                Err(error) => Err(error.into()),
            },
        }

        if app.quit {
            break Ok(());
        }
    };

    let _ = crossterm::execute!(stdout(), DisableBracketedPaste, DisableFocusChange);
    ratatui::restore();
    result
}

impl App {
    fn new(
        rest: RestClient,
        gateway: GatewayHandle,
        tasks: mpsc::UnboundedSender<TaskResult>,
        options: &Options,
    ) -> Self {
        App {
            store: Store::new(options.profile),
            sidebar: {
                let mut sidebar = Sidebar::default();
                sidebar.set_show_locked(options.show_locked);
                sidebar
            },
            sidebar_state: ListState::default(),
            focus: Focus::Sidebar,
            input: Input::default(),
            compose: Compose::Message,
            selected: None,
            confirm_delete: None,
            scroll: 0,
            messages_height: 0,
            at_top: false,
            loading_history: false,
            loading_older: false,
            connection: Connection::Connecting,
            error: None,
            memory_kb: resident_memory_kb(),
            options_tier: options.profile.tier,
            switcher: None,
            view_cache: None,
            rest,
            gateway,
            tasks,
            typing: HashMap::new(),
            last_typing_sent: None,
            last_ack: None,
            terminal_focused: true,
            silent_typing: options.silent_typing,
            notifications: options.notifications,
            last_notified: HashMap::new(),
            history_limit: options.profile.messages_per_channel.min(100) as u8,
            ticks: 0,
            quit: false,
            dirty: true,
        }
    }

    // ---- Events -------------------------------------------------------------

    fn on_terminal(&mut self, event: TermEvent) {
        match event {
            TermEvent::Key(key) if key.kind == KeyEventKind::Press => self.on_key(key),
            // A paste arrives in one piece, so its newlines can't trigger "send".
            TermEvent::Paste(text) => {
                let target = match &mut self.switcher {
                    Some(switcher) => &mut switcher.input,
                    None => {
                        self.focus = Focus::Input;
                        &mut self.input
                    }
                };
                for c in text.chars() {
                    target.insert(if c == '\n' || c == '\r' { ' ' } else { c });
                }
                if self.switcher.is_some() {
                    self.refilter_switcher();
                }
            }
            TermEvent::FocusGained => {
                self.terminal_focused = true;
                self.ack_active(true);
            }
            TermEvent::FocusLost => self.terminal_focused = false,
            TermEvent::Resize(..) => {}
            _ => return,
        }
        self.dirty = true;
    }

    fn on_gateway(&mut self, event: Event) {
        match &event {
            Event::Ready { .. } => {
                self.connection = Connection::Online;
                self.sidebar.mark_dirty();
                self.store.apply(event);
                // READY's JSON was just freed: hand that memory back to the OS.
                trim_heap();
                // A new session may have missed messages in the open channel.
                if let Some(channel_id) = self.store.active_channel()
                    && self.store.needs_history(channel_id)
                {
                    self.fetch_history(channel_id);
                }
                self.dirty = true;
                return;
            }
            Event::Resumed => {
                self.connection = Connection::Online;
                self.sidebar.mark_dirty();
            }
            Event::Disconnected => {
                // Before the first READY it's still "connecting".
                if self.connection == Connection::Online {
                    self.connection = Connection::Reconnecting;
                }
            }
            Event::GuildCreate { .. }
            | Event::GuildDelete { .. }
            | Event::ChannelUpsert(_)
            | Event::ChannelDelete { .. }
            | Event::GuildUpdate { .. }
            | Event::MemberUpdate { .. }
            | Event::RoleUpsert { .. }
            | Event::RoleDelete { .. }
            | Event::MessageAck { .. }
            | Event::GuildSettingsUpdate(_)
            | Event::ThreadsSync { .. }
            | Event::VoiceStateUpdate { .. } => self.sidebar.mark_dirty(),
            Event::VoiceServerUpdate { .. } => return,
            Event::MessageCreate { message, author } => {
                self.maybe_notify(message, author);
                // They sent it, so they're not typing any more.
                if let Some(typers) = self.typing.get_mut(&message.channel_id) {
                    typers.retain(|(user, _)| *user != message.author_id);
                }
                // DMs and threads re-sort by activity.
                if self
                    .store
                    .channel(message.channel_id)
                    .is_some_and(|c| sidebar::is_dm(c) || c.kind == ChannelKind::Thread)
                {
                    self.sidebar.mark_dirty();
                }
            }
            Event::TypingStart { channel_id, user_id } => {
                if Some(*user_id) == self.store.me().map(|u| u.id) {
                    return;
                }
                let typers = self.typing.entry(*channel_id).or_default();
                typers.retain(|(user, _)| user != user_id);
                typers.push((*user_id, Instant::now()));
                if Some(*channel_id) != self.store.active_channel() {
                    return; // nothing visible changed
                }
            }
            Event::MessageUpdate { channel_id, .. }
            | Event::MessageDelete { channel_id, .. }
            | Event::ReactionAdd { channel_id, .. }
            | Event::ReactionRemove { channel_id, .. }
            | Event::ReactionClear { channel_id, .. } => {
                if Some(*channel_id) != self.store.active_channel() {
                    self.store.apply(event);
                    return;
                }
            }
            // Presence updates and the like: nothing to redraw.
            Event::Other(_) => return,
        }
        self.store.apply(event);
        self.dirty = true;
    }

    fn on_task(&mut self, result: TaskResult) {
        match result {
            TaskResult::History { channel_id, result } => {
                if Some(channel_id) == self.store.active_channel() {
                    self.loading_history = false;
                }
                match result {
                    Ok(history) => self.store.insert_history(channel_id, history),
                    Err(RestError::Forbidden) => self.show_error("You don't have access to that channel"),
                    Err(error) => self.show_error(format!("Couldn't load messages: {error}")),
                }
            }
            TaskResult::Older { channel_id, result } => {
                self.loading_older = false;
                match result {
                    Ok(page) => self.store.insert_older(channel_id, page, usize::from(OLDER_PAGE)),
                    Err(error) => self.show_error(format!("Couldn't load older messages: {error}")),
                }
            }
            TaskResult::Sent { text, result } => match result {
                // Store it now; the gateway's copy of the same message is ignored.
                Ok((message, author)) => {
                    if self.error.as_ref().is_some_and(|(e, _)| e.starts_with("Uploading")) {
                        self.error = None;
                    }
                    self.store.apply(Event::MessageCreate { message, author });
                }
                Err(error) => {
                    self.show_error(format!("Couldn't send: {error}"));
                    if self.input.is_empty() {
                        self.input.set(text); // don't lose what they typed
                    }
                }
            },
            TaskResult::Failed { text, error } => {
                self.show_error(error);
                if self.input.is_empty() {
                    self.input.set(text);
                }
            }
            TaskResult::Edited { text, result } => match result {
                Ok((message, _)) => self.store.apply(Event::MessageUpdate {
                    id: message.id,
                    channel_id: message.channel_id,
                    content: Some(message.content),
                    embeds: Some(message.embeds),
                }),
                Err(error) => {
                    self.show_error(format!("Couldn't edit: {error}"));
                    if self.input.is_empty() {
                        self.input.set(text);
                    }
                }
            },
            TaskResult::NotifyInTerminal { title, body } => {
                terminal_notification(&title, &body);
                return;
            }
            TaskResult::Done { action, result } => {
                if let Err(error) = result {
                    self.show_error(match error {
                        RestError::Forbidden => format!("You're not allowed to {action} that"),
                        error => format!("Couldn't {action}: {error}"),
                    });
                }
            }
        }
        self.dirty = true;
    }

    fn on_tick(&mut self) {
        self.ticks += 1;
        let now = Instant::now();

        let active = self.store.active_channel();
        for (channel_id, typers) in &mut self.typing {
            let before = typers.len();
            typers.retain(|(_, since)| now - *since < TYPING_TIMEOUT);
            if typers.len() != before && Some(*channel_id) == active {
                self.dirty = true;
            }
        }
        self.typing.retain(|_, typers| !typers.is_empty());

        if self
            .error
            .as_ref()
            .is_some_and(|(_, since)| now - *since > ERROR_TIMEOUT)
        {
            self.error = None;
            self.dirty = true;
        }

        self.ack_active(false);

        // While idle, check every 2 s and redraw if the shown value (0.1 MB) changed.
        if self.ticks.is_multiple_of(2) && shown_mb(resident_memory_kb()) != shown_mb(self.memory_kb) {
            self.dirty = true;
        }
    }

    // ---- Keys ---------------------------------------------------------------

    fn on_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c' | 'q') if ctrl => {
                self.quit = true;
                return;
            }
            KeyCode::Char('k') if ctrl => {
                self.toggle_switcher();
                return;
            }
            _ => {}
        }
        if self.switcher.is_some() {
            self.switcher_key(key);
            return;
        }
        if let Some(message_id) = self.confirm_delete.take() {
            if matches!(key.code, KeyCode::Char('y' | 'Y') | KeyCode::Enter) {
                self.delete_message(message_id);
            }
            return;
        }

        let half_page = usize::from(self.messages_height / 2).max(1);
        match key.code {
            KeyCode::PageUp => {
                self.scroll += half_page;
                if self.at_top {
                    self.load_older();
                }
            }
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(half_page),
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::Input,
                    Focus::Input | Focus::Messages => Focus::Sidebar,
                };
                self.selected = None;
            }
            _ => match self.focus {
                Focus::Sidebar => self.sidebar_key(key),
                Focus::Input => self.input_key(key),
                Focus::Messages => self.messages_key(key),
            },
        }
    }

    fn sidebar_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.sidebar.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.sidebar.move_by(1),
            KeyCode::Home | KeyCode::Char('g') => self.sidebar.select_first(),
            KeyCode::End | KeyCode::Char('G') => self.sidebar.select_last(),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if let Some(entry) = self.sidebar.selected() {
                    self.activate(entry);
                }
            }
            KeyCode::Left | KeyCode::Char('h') => match self.sidebar.selected() {
                Some(Entry::Guild(id)) if self.sidebar.is_expanded(id) => self.sidebar.toggle_guild(id),
                Some(Entry::Channel(id) | Entry::Category(id)) => {
                    if let Some(guild_id) = self.store.channel(id).and_then(|c| c.guild_id) {
                        self.sidebar.collapse_guild(guild_id);
                    }
                }
                _ => {}
            },
            KeyCode::Char('i' | '/') => self.focus = Focus::Input,
            KeyCode::Char('L') => {
                let show = !self.sidebar.shows_locked();
                self.sidebar.set_show_locked(show);
                self.show_error(if show {
                    "Showing channels you can't read"
                } else {
                    "Hiding channels you can't read"
                });
            }
            _ => {}
        }
    }

    fn activate(&mut self, entry: Entry) {
        match entry {
            Entry::DmHeader => self.sidebar.toggle_dms(),
            Entry::Guild(id) => self.sidebar.toggle_guild(id),
            Entry::Channel(id) if self.store.channel(id).is_some_and(|c| c.kind == ChannelKind::Forum) => {
                self.show_error("Forums are made of posts: pick one below it");
            }
            Entry::Dm(id) | Entry::Channel(id) => self.open_channel(id),
            Entry::ServerHeader | Entry::Category(_) => {}
        }
    }

    fn input_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc if self.compose != Compose::Message => self.cancel_compose(),
            KeyCode::Esc => self.focus = Focus::Sidebar,
            KeyCode::Enter => self.submit(),
            // With nothing typed, Up picks a message (to reply, edit, react…).
            KeyCode::Up if self.input.is_empty() && self.compose == Compose::Message => self.select_newest(),
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Left => self.input.left(),
            KeyCode::Right => self.input.right(),
            KeyCode::Home => self.input.home(),
            // With nothing typed, End jumps back to the newest messages.
            KeyCode::End if self.input.is_empty() => self.scroll = 0,
            KeyCode::End => self.input.end(),
            KeyCode::Char('u') if ctrl => {
                self.input.take();
            }
            KeyCode::Char('w') if ctrl => self.input.delete_word(),
            KeyCode::Char(c) if !ctrl && !alt => {
                self.input.insert(c);
                if !matches!(self.compose, Compose::React(_)) {
                    self.notify_typing();
                }
            }
            _ => {}
        }
    }

    fn messages_key(&mut self, key: KeyEvent) {
        let Some(selected) = self.selected_message().map(|m| (m.id, m.author_id)) else {
            // The message went away (deleted, or the channel changed).
            self.leave_selection();
            return;
        };
        let (message_id, author_id) = selected;
        let mine = Some(author_id) == self.store.me().map(|u| u.id);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Home | KeyCode::Char('g') => {
                let oldest = self.active_messages().next().map(|m| m.id);
                self.selected = oldest;
                self.load_older();
            }
            KeyCode::End | KeyCode::Char('G') => self.select_newest(),
            KeyCode::Esc | KeyCode::Char('i') => self.leave_selection(),
            KeyCode::Char('r') | KeyCode::Enter => {
                self.compose = Compose::Reply(message_id);
                self.leave_selection();
            }
            KeyCode::Char('e') if mine => {
                let content = self.selected_message().map(|m| m.content.clone()).unwrap_or_default();
                self.compose = Compose::Edit(message_id);
                self.input.set(content);
                self.leave_selection();
            }
            KeyCode::Char('e') => self.show_error("You can only edit your own messages"),
            KeyCode::Char('d') | KeyCode::Delete => self.confirm_delete = Some(message_id),
            KeyCode::Char('+' | 'a') => {
                self.compose = Compose::React(message_id);
                self.input.take();
                self.leave_selection();
            }
            KeyCode::Char(digit @ '1'..='9') => {
                let index = digit as usize - '1' as usize;
                let emoji = self
                    .selected_message()
                    .and_then(|m| m.reactions.get(index))
                    .map(|r| r.emoji.clone());
                match emoji {
                    Some(emoji) => self.toggle_reaction(message_id, emoji),
                    None => self.show_error(format!("This message has no reaction #{digit}")),
                }
            }
            KeyCode::Char('o') => self.open_link(),
            _ => {}
        }
    }

    // ---- Message selection --------------------------------------------------

    fn active_messages(&self) -> impl DoubleEndedIterator<Item = &Message> {
        let channel = self.store.active_channel().unwrap_or_default();
        self.store.messages(channel)
    }

    pub fn selected_message(&self) -> Option<&Message> {
        let channel = self.store.active_channel()?;
        self.store.message(channel, self.selected?)
    }

    fn select_newest(&mut self) {
        let Some(newest) = self.active_messages().next_back().map(|m| m.id) else {
            return;
        };
        self.selected = Some(newest);
        self.focus = Focus::Messages;
    }

    fn move_selection(&mut self, delta: isize) {
        let Some(selected) = self.selected else { return };
        let ids: Vec<Snowflake> = self.active_messages().map(|m| m.id).collect();
        let Some(index) = ids.iter().position(|&id| id == selected) else {
            return;
        };
        match index.checked_add_signed(delta) {
            Some(next) if next < ids.len() => self.selected = Some(ids[next]),
            // Past the newest message: back to typing.
            Some(_) => self.leave_selection(),
            // Past the oldest: fetch more.
            None => self.load_older(),
        }
    }

    fn leave_selection(&mut self) {
        self.selected = None;
        self.focus = Focus::Input;
        self.scroll = 0;
    }

    fn cancel_compose(&mut self) {
        if matches!(self.compose, Compose::Edit(_) | Compose::React(_)) {
            self.input.take();
        }
        self.compose = Compose::Message;
    }

    // ---- Actions ------------------------------------------------------------

    fn open_channel(&mut self, channel_id: Snowflake) {
        self.scroll = 0;
        self.focus = Focus::Input;
        self.selected = None;
        self.confirm_delete = None;
        self.cancel_compose();
        self.loading_older = false;
        let needs_history = self.store.open_channel(channel_id);
        self.sidebar.reveal_channel(&self.store, channel_id);
        // A locked channel shows a notice; Discord would refuse everything else.
        if !self.store.can_view(channel_id) {
            return;
        }
        self.ack_active(true);

        if let Some(guild_id) = self.store.channel(channel_id).and_then(|c| c.guild_id) {
            self.gateway.subscribe_channel(guild_id, channel_id);
        }
        if needs_history {
            self.fetch_history(channel_id);
        }
    }

    fn fetch_history(&mut self, channel_id: Snowflake) {
        self.loading_history = true;
        let rest = self.rest.clone();
        let tasks = self.tasks.clone();
        let limit = self.history_limit;
        tokio::spawn(async move {
            let result = rest.messages(channel_id, limit).await;
            let _ = tasks.send(TaskResult::History { channel_id, result });
        });
    }

    /// Fetches the page before the oldest cached message, if there is one.
    fn load_older(&mut self) {
        let Some(channel_id) = self.store.active_channel() else {
            return;
        };
        if self.loading_older || self.loading_history || !self.store.has_older(channel_id) {
            return;
        }
        let Some(oldest) = self.store.messages(channel_id).next().map(|m| m.id) else {
            return;
        };
        self.loading_older = true;
        let rest = self.rest.clone();
        let tasks = self.tasks.clone();
        tokio::spawn(async move {
            let result = rest.messages_before(channel_id, Some(oldest), OLDER_PAGE).await;
            let _ = tasks.send(TaskResult::Older { channel_id, result });
        });
    }

    /// Tells Discord you've read the open channel (so other devices agree),
    /// when there's something new. Batched unless `now`.
    fn ack_active(&mut self, now: bool) {
        if !self.terminal_focused {
            return;
        }
        if !now && self.last_ack.is_some_and(|at| at.elapsed() < ACK_INTERVAL) {
            return;
        }
        let Some(channel_id) = self.store.active_channel() else {
            return;
        };
        let Some(message_id) = self.store.mark_read(channel_id) else {
            return;
        };
        self.last_ack = Some(Instant::now());
        self.sidebar.mark_dirty();
        let rest = self.rest.clone();
        tokio::spawn(async move {
            if let Err(error) = rest.ack(channel_id, message_id).await {
                warn!("read ack failed: {error}");
            }
        });
    }

    /// Enter in the input box: send, reply, save an edit, or react.
    fn submit(&mut self) {
        let Some(channel_id) = self.store.active_channel() else {
            self.show_error("Open a channel first");
            return;
        };
        if !self.store.can_view(channel_id) {
            self.show_error("You don't have permission to send messages here");
            return;
        }
        let text = self.input.text().trim().to_owned();
        if let Compose::React(message_id) = self.compose {
            if !text.is_empty() {
                let emoji = parse_emoji(&text, self.store.message(channel_id, message_id));
                self.toggle_reaction(message_id, emoji);
            }
            self.input.take();
            self.compose = Compose::Message;
            return;
        }
        if let Some(upload) = crate::upload::parse(&text) {
            self.upload(channel_id, text.clone(), upload);
            return;
        }
        if text.is_empty() {
            if let Compose::Edit(_) = self.compose {
                self.show_error("Empty message: press Esc to cancel, or pick it with ↑ and press d to delete");
            }
            return;
        }
        let text = plume_markdown::replace_shortcodes(&text);
        let length = text.chars().count();
        if length > MAX_MESSAGE_LENGTH {
            self.show_error(format!("Message is too long ({length}/{MAX_MESSAGE_LENGTH})"));
            return;
        }

        self.input.take();
        self.scroll = 0;
        self.last_typing_sent = None;
        let compose = std::mem::replace(&mut self.compose, Compose::Message);
        let rest = self.rest.clone();
        let tasks = self.tasks.clone();
        tokio::spawn(async move {
            let task = match compose {
                Compose::Edit(message_id) => {
                    let result = rest.edit_message(channel_id, message_id, &text).await;
                    TaskResult::Edited { text, result }
                }
                Compose::Reply(message_id) => {
                    let result = rest.send_message(channel_id, &text, Some(message_id)).await;
                    TaskResult::Sent { text, result }
                }
                Compose::Message | Compose::React(_) => {
                    let result = rest.send_message(channel_id, &text, None).await;
                    TaskResult::Sent { text, result }
                }
            };
            let _ = tasks.send(task);
        });
    }

    fn upload(
        &mut self,
        channel_id: Snowflake,
        text: String,
        parsed: Result<(std::path::PathBuf, String), &'static str>,
    ) {
        let (path, caption) = match parsed {
            Ok(parsed) => parsed,
            Err(error) => return self.show_error(error),
        };
        let reply_to = match self.compose {
            Compose::Edit(_) => return self.show_error("Files can't be added to an edit"),
            Compose::Reply(message_id) => Some(message_id),
            Compose::Message | Compose::React(_) => None,
        };
        let caption = plume_markdown::replace_shortcodes(&caption);
        self.input.take();
        self.compose = Compose::Message;
        self.scroll = 0;
        self.show_error(format!("Uploading {}…", path.display()));
        let rest = self.rest.clone();
        let tasks = self.tasks.clone();
        tokio::spawn(async move {
            let file = tokio::task::spawn_blocking(move || crate::upload::read(&path)).await;
            let task = match file {
                Ok(Ok((name, data))) => {
                    let result = rest.upload(channel_id, &name, data, &caption, reply_to).await;
                    TaskResult::Sent { text, result }
                }
                Ok(Err(error)) => TaskResult::Failed { text, error },
                Err(error) => TaskResult::Failed {
                    text,
                    error: format!("Upload failed: {error}"),
                },
            };
            let _ = tasks.send(task);
        });
    }

    fn delete_message(&mut self, message_id: Snowflake) {
        let Some(channel_id) = self.store.active_channel() else {
            return;
        };
        // Move the highlight off the message that's about to disappear.
        if self.selected == Some(message_id) {
            self.move_selection(-1);
            if self.selected == Some(message_id) {
                self.leave_selection();
            }
        }
        let rest = self.rest.clone();
        let tasks = self.tasks.clone();
        tokio::spawn(async move {
            let result = rest.delete_message(channel_id, message_id).await;
            let _ = tasks.send(TaskResult::Done {
                action: "delete",
                result,
            });
        });
    }

    /// Adds your reaction, or removes it if it's already there.
    fn toggle_reaction(&mut self, message_id: Snowflake, emoji: Emoji) {
        let Some(channel_id) = self.store.active_channel() else {
            return;
        };
        let add = !self
            .store
            .message(channel_id, message_id)
            .is_some_and(|m| m.reactions.iter().any(|r| r.emoji == emoji && r.me));
        let rest = self.rest.clone();
        let tasks = self.tasks.clone();
        tokio::spawn(async move {
            let result = rest.react(channel_id, message_id, &emoji.name, emoji.id, add).await;
            let _ = tasks.send(TaskResult::Done {
                action: "react to",
                result,
            });
        });
    }

    /// Opens the selected message's first link or attachment in the browser.
    fn open_link(&mut self) {
        let Some(message) = self.selected_message() else { return };
        let url = message
            .attachments
            .first()
            .map(|a| a.url.clone())
            .or_else(|| first_link(&message.content));
        match url {
            Some(url) => {
                if let Err(error) = crate::open::url(&url) {
                    self.show_error(format!("Couldn't open the link: {error}"));
                }
            }
            None => self.show_error("No link or attachment in that message"),
        }
    }

    /// A desktop notification for pings you're not looking at: DMs and
    /// mentions outside the open channel (or anywhere, while the terminal
    /// is in the background). Muted channels stay quiet.
    fn maybe_notify(&mut self, message: &Message, author: &User) {
        let channel_id = message.channel_id;
        let looking = self.terminal_focused && Some(channel_id) == self.store.active_channel();
        if !self.notifications || looking || !self.store.pings_me(message) || self.store.is_muted(channel_id) {
            return;
        }
        let now = Instant::now();
        if self
            .last_notified
            .get(&channel_id)
            .is_some_and(|at| now - *at < NOTIFY_INTERVAL)
        {
            return;
        }
        self.last_notified.insert(channel_id, now);

        let in_dm = self
            .store
            .channel(channel_id)
            .is_some_and(|c| c.kind == plume_core::models::ChannelKind::Dm);
        let title = if in_dm {
            author.display_name().to_owned()
        } else {
            format!(
                "{} in {}",
                author.display_name(),
                sidebar::channel_label(&self.store, channel_id)
            )
        };
        let body = if message.content.is_empty() && !message.attachments.is_empty() {
            "sent an attachment".to_owned()
        } else {
            plume_notify::shorten(&message.content, 200)
        };
        let tasks = self.tasks.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(error) = plume_notify::send(&title, &body) {
                tracing::debug!("desktop notification failed ({error}); using the terminal");
                let _ = tasks.send(TaskResult::NotifyInTerminal { title, body });
            }
        });
    }

    /// Tells the channel we're typing, at most once every few seconds.
    fn notify_typing(&mut self) {
        if self.silent_typing {
            return;
        }
        let Some(channel_id) = self.store.active_channel() else {
            return;
        };
        let recently = self
            .last_typing_sent
            .is_some_and(|(channel, at)| channel == channel_id && at.elapsed() < TYPING_RESEND);
        if recently {
            return;
        }
        self.last_typing_sent = Some((channel_id, Instant::now()));
        let rest = self.rest.clone();
        tokio::spawn(async move {
            if let Err(error) = rest.trigger_typing(channel_id).await {
                warn!("typing indicator failed: {error}");
            }
        });
    }

    fn show_error(&mut self, message: impl Into<String>) {
        self.error = Some((message.into(), Instant::now()));
    }

    /// "alice is typing…" for the open channel, if anyone is.
    pub fn typing_text(&self) -> Option<String> {
        let channel_id = self.store.active_channel()?;
        let names: Vec<&str> = self
            .typing
            .get(&channel_id)?
            .iter()
            .filter_map(|(user, _)| self.store.user(*user))
            .map(|u| u.display_name())
            .collect();
        match names.as_slice() {
            [] => None,
            [one] => Some(format!("{one} is typing…")),
            [a, b] => Some(format!("{a} and {b} are typing…")),
            _ => Some("Several people are typing…".to_owned()),
        }
    }

    /// The name of whoever wrote a message in the open channel.
    pub fn author_name(&self, message_id: Snowflake) -> String {
        self.store
            .active_channel()
            .and_then(|channel| self.store.message(channel, message_id))
            .and_then(|m| self.store.user(m.author_id))
            .map_or_else(|| "a message".to_owned(), |u| u.display_name().to_owned())
    }

    // ---- Quick switcher -----------------------------------------------------

    fn toggle_switcher(&mut self) {
        if self.switcher.take().is_some() {
            return;
        }
        let mut all = Vec::new();
        for channel_id in sidebar::sorted_dms(&self.store) {
            all.push((channel_id, sidebar::channel_label(&self.store, channel_id)));
        }
        for guild in sidebar::sorted_guilds(&self.store) {
            for entry in sidebar::guild_channels(&self.store, guild, self.sidebar.shows_locked()) {
                if let Entry::Channel(id) = entry {
                    let label = format!("{} · {}", sidebar::channel_label(&self.store, id), guild.name);
                    all.push((id, label));
                }
            }
        }
        let all = all.into_iter().map(|(id, label)| {
            let lower = label.to_lowercase();
            (id, label, lower)
        });
        self.switcher = Some(Switcher {
            input: Input::default(),
            matches: Vec::new(),
            selected: 0,
            all: all.collect(),
        });
        self.refilter_switcher();
    }

    fn switcher_key(&mut self, key: KeyEvent) {
        let Some(switcher) = &mut self.switcher else { return };
        match key.code {
            KeyCode::Esc => self.switcher = None,
            KeyCode::Enter => {
                let target = switcher.matches.get(switcher.selected).map(|(id, _)| *id);
                self.switcher = None;
                if let Some(channel_id) = target {
                    self.open_channel(channel_id);
                }
            }
            KeyCode::Up => switcher.selected = switcher.selected.saturating_sub(1),
            KeyCode::Down => {
                switcher.selected = (switcher.selected + 1).min(switcher.matches.len().saturating_sub(1));
            }
            KeyCode::Backspace => {
                switcher.input.backspace();
                self.refilter_switcher();
            }
            KeyCode::Left => switcher.input.left(),
            KeyCode::Right => switcher.input.right(),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                switcher.input.insert(c);
                self.refilter_switcher();
            }
            _ => {}
        }
    }

    /// Every word typed must appear in the label; labels starting with the
    /// first word come first.
    fn refilter_switcher(&mut self) {
        let Some(switcher) = &mut self.switcher else { return };
        let query = switcher.input.text().to_lowercase();
        let words: Vec<&str> = query.split_whitespace().collect();
        let first = words.first().copied().unwrap_or("");

        let mut matches: Vec<(bool, &(Snowflake, String, String))> = switcher
            .all
            .iter()
            .filter(|(_, _, lower)| words.iter().all(|w| lower.contains(w)))
            .map(|entry| {
                // Skip the "#" or "@" when checking "starts with".
                let starts = entry.2.get(1..).is_some_and(|rest| rest.starts_with(first));
                (!starts, entry)
            })
            .collect();
        matches.sort_by_key(|(not_prefix, _)| *not_prefix); // stable: keeps sidebar order
        switcher.matches = matches
            .into_iter()
            .take(200)
            .map(|(_, (id, label, _))| (*id, label.clone()))
            .collect();
        switcher.selected = 0;
    }
}

/// Memory rounded the way the status bar shows it (tenths of a MB).
fn shown_mb(kb: Option<u64>) -> Option<u64> {
    kb.map(|kb| (kb as f64 / 1024.0 * 10.0).round() as u64)
}

/// This process's resident memory in KB (Linux only), to show in the status bar.
fn resident_memory_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// Returns freed heap memory to the OS. glibc keeps it otherwise, so a
/// multi-MB READY would stay in the RSS for the whole session.
fn trim_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim only releases free memory; it has no preconditions.
    unsafe {
        unsafe extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        malloc_trim(0);
    }
}

/// Asks the terminal for a notification (OSC 9, understood by Windows
/// Terminal, iTerm2, kitty, WezTerm, foot, Ghostty…) and rings the bell for
/// the rest. Control characters are removed so a message can't inject
/// escape sequences.
fn terminal_notification(title: &str, body: &str) {
    use std::io::Write;
    let clean = |text: &str| text.chars().filter(|c| !c.is_control()).collect::<String>();
    let mut out = stdout();
    let _ = write!(out, "\x1b]9;{}: {}\x07\x07", clean(title), clean(body));
    let _ = out.flush();
}

/// What the user typed in the react prompt, as an emoji: `:name:` matches a
/// reaction already on the message (custom ones included) or a known
/// shortcode; anything else is taken as the emoji itself.
fn parse_emoji(text: &str, message: Option<&Message>) -> Emoji {
    if let Some(name) = text.strip_prefix(':').and_then(|t| t.strip_suffix(':')) {
        if let Some(existing) = message.and_then(|m| m.reactions.iter().find(|r| r.emoji.name == name)) {
            return existing.emoji.clone();
        }
        if let Some(emoji) = plume_markdown::shortcode(name) {
            return Emoji {
                name: emoji.to_owned(),
                id: None,
            };
        }
    }
    Emoji {
        name: text.to_owned(),
        id: None,
    }
}

fn first_link(content: &str) -> Option<String> {
    plume_markdown::flatten(&plume_markdown::parse(content))
        .into_iter()
        .find_map(|span| match span.piece {
            plume_markdown::Piece::Link(url) => Some(url.to_owned()),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use plume_core::models::{Attachment, Channel, ChannelKind, Guild};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn user(id: u64, name: &str) -> User {
        User {
            id: Snowflake(id),
            username: name.to_lowercase(),
            global_name: Some(name.to_owned()),
        }
    }

    fn channel(id: u64, guild: Option<u64>, name: Option<&str>, kind: ChannelKind, parent: Option<u64>) -> Channel {
        Channel {
            id: Snowflake(id),
            guild_id: guild.map(Snowflake),
            name: name.map(str::to_owned),
            kind,
            parent_id: parent.map(Snowflake),
            position: 0,
            recipients: if guild.is_none() {
                vec![Snowflake(3)]
            } else {
                Vec::new()
            },
            last_message_id: None,
            overwrites: Vec::new(),
        }
    }

    fn message(seconds: u64, author: u64, content: &str) -> Message {
        let id = Snowflake::from_timestamp_ms(1_760_000_000_000 + seconds * 1000);
        Message {
            id,
            channel_id: Snowflake(100),
            author_id: Snowflake(author),
            content: content.to_owned(),
            edited: false,
            reply_to: None,
            mentions: Vec::new(),
            mention_everyone: false,
            attachments: Vec::new(),
            reactions: Vec::new(),
            embeds: Vec::new(),
        }
    }

    fn demo_app_ready() -> Event {
        Event::Ready {
            me: user(1, "Me"),
            guilds: vec![Guild {
                id: Snowflake(10),
                name: "Rust Hangout".into(),
                channels: vec![Snowflake(99), Snowflake(100), Snowflake(101)],
                ..Guild::default()
            }],
            channels: vec![
                channel(99, Some(10), Some("Text Channels"), ChannelKind::Category, None),
                channel(100, Some(10), Some("general"), ChannelKind::Text, Some(99)),
                channel(101, Some(10), Some("help"), ChannelKind::Text, Some(99)),
                channel(200, None, None, ChannelKind::Dm, None),
            ],
            users: vec![user(2, "Alice"), user(3, "Bob")],
            read_states: Vec::new(),
            settings: Vec::new(),
            voice_states: Vec::new(),
        }
    }

    /// An app with one server, a DM, and a few messages, without any network.
    fn demo_app() -> App {
        let options = Options {
            profile: plume_core::Profile::FEATHER,
            silent_typing: true,
            notifications: false,
            show_locked: false,
        };
        let token = Token::new("test");
        let (events, _) = mpsc::channel(1);
        let handle = Gateway::new(token.clone(), events).handle();
        let (tasks, _) = mpsc::unbounded_channel();
        let mut app = App::new(RestClient::new(&token).unwrap(), handle, tasks, &options);

        app.on_gateway(demo_app_ready());

        app.store.open_channel(Snowflake(100));
        app.sidebar.reveal_channel(&app.store, Snowflake(100));
        let mut with_file = message(30, 2, "and a picture");
        with_file.attachments.push(Attachment {
            filename: "crab.png".into(),
            url: "https://cdn.example/crab.png".into(),
            size: 48_000,
        });
        let mut ping = message(90, 3, "hey <@1>, try `cargo run` ||it works||");
        ping.edited = true;
        app.store.insert_history(
            Snowflake(100),
            vec![
                (
                    message(
                        0,
                        2,
                        "**hello** everyone, this is a *long* message that should wrap onto the next line nicely",
                    ),
                    user(2, "Alice"),
                ),
                (with_file, user(2, "Alice")),
                (ping, user(3, "Bob")),
                (message(95, 1, "my own words"), user(1, "Me")),
            ],
        );
        app.on_gateway(Event::MessageCreate {
            message: Message {
                channel_id: Snowflake(101),
                mentions: vec![Snowflake(1)],
                ..message(100, 3, "unread elsewhere <@1>")
            },
            author: user(3, "Bob"),
        });
        app
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        app.sidebar.rebuild_if_dirty(&app.store);
        terminal.draw(|frame| render::draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..height {
            for x in 0..width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[tokio::test]
    async fn draws_sidebar_messages_and_status() {
        let mut app = demo_app();
        let screen = render(&mut app, 100, 24);
        println!("{screen}");

        assert!(screen.contains("Rust Hangout"));
        assert!(screen.contains("# general"));
        assert!(screen.contains("@ Bob"), "DM listed by recipient");
        assert!(screen.contains("Alice"));
        assert!(screen.contains("crab.png (46.9 KB)"));
        assert!(screen.contains("# help  1"), "mention badge on the unread channel");
        assert!(screen.contains("@Me"), "mention resolved to a name");
        assert!(screen.contains("(edited)"));
        assert!(screen.contains("online"));
        assert!(!screen.contains("**hello**"), "markdown is rendered, not shown raw");
    }

    #[tokio::test]
    async fn narrow_terminal_shows_one_pane_at_a_time() {
        let mut app = demo_app();
        app.focus = Focus::Input;
        let chat = render(&mut app, 50, 20);
        assert!(!chat.contains("SERVERS"));
        assert!(chat.contains("Alice"));

        app.focus = Focus::Sidebar;
        let side = render(&mut app, 50, 20);
        assert!(side.contains("SERVERS"));
    }

    #[tokio::test]
    async fn switcher_filters_by_every_word() {
        let mut app = demo_app();
        app.toggle_switcher();
        for c in "hang hel".chars() {
            app.switcher.as_mut().unwrap().input.insert(c);
        }
        app.refilter_switcher();
        let labels: Vec<&str> = app
            .switcher
            .as_ref()
            .unwrap()
            .matches
            .iter()
            .map(|(_, l)| l.as_str())
            .collect();
        assert_eq!(labels, ["#help · Rust Hangout"]);
        println!("{}", render(&mut app, 100, 24));
    }

    #[tokio::test]
    async fn tiny_terminal_does_not_panic() {
        let mut app = demo_app();
        for (w, h) in [(1, 1), (5, 3), (20, 5), (59, 10), (61, 4)] {
            render(&mut app, w, h);
            app.focus = Focus::Input;
            render(&mut app, w, h);
            app.focus = Focus::Sidebar;
        }
        app.toggle_switcher();
        render(&mut app, 10, 4);
    }

    fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn selected_content(app: &App) -> &str {
        app.selected_message().map_or("", |m| m.content.as_str())
    }

    #[tokio::test]
    async fn up_from_an_empty_box_picks_messages() {
        let mut app = demo_app();
        app.focus = Focus::Input;
        press(&mut app, KeyCode::Up);
        assert_eq!(app.focus, Focus::Messages);
        assert_eq!(selected_content(&app), "my own words");
        press(&mut app, KeyCode::Up);
        assert!(selected_content(&app).starts_with("hey"));
        let screen = render(&mut app, 100, 24);
        assert!(screen.contains("r reply"), "selection hints are shown");

        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down); // past the newest: back to typing
        assert_eq!(app.focus, Focus::Input);
        assert_eq!(app.selected, None);
    }

    #[tokio::test]
    async fn reply_edit_and_react_set_up_the_input_box() {
        let mut app = demo_app();
        app.focus = Focus::Input;
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up); // Bob's message
        press(&mut app, KeyCode::Char('r'));
        assert!(matches!(app.compose, Compose::Reply(_)));
        assert_eq!(app.focus, Focus::Input);
        assert!(render(&mut app, 100, 24).contains("Replying to Bob"));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.compose, Compose::Message, "Esc cancels the reply first");
        assert_eq!(app.focus, Focus::Input);

        // Someone else's message can't be edited.
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Char('e'));
        assert!(app.error.is_some());
        assert_eq!(app.compose, Compose::Message);

        // Your own can: the box is filled with its text.
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('e'));
        assert!(matches!(app.compose, Compose::Edit(_)));
        assert_eq!(app.input.text(), "my own words");
        assert!(render(&mut app, 100, 24).contains("Editing your message"));
        press(&mut app, KeyCode::Esc);
        assert!(app.input.is_empty(), "cancelling an edit clears the box");

        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Char('+'));
        assert!(matches!(app.compose, Compose::React(_)));
        assert!(render(&mut app, 100, 24).contains("Type an emoji"));
    }

    #[tokio::test]
    async fn delete_asks_first() {
        let mut app = demo_app();
        app.focus = Focus::Input;
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Char('d'));
        assert!(app.confirm_delete.is_some());
        assert!(render(&mut app, 100, 24).contains("Delete Me's message?"));
        press(&mut app, KeyCode::Char('n'));
        assert!(app.confirm_delete.is_none());
        assert_eq!(selected_content(&app), "my own words", "nothing was deleted");
    }

    #[tokio::test]
    async fn reactions_show_under_the_message() {
        let mut app = demo_app();
        let (channel_id, message_id) = (Snowflake(100), app.store.messages(Snowflake(100)).last().unwrap().id);
        let emoji = Emoji {
            name: "🔥".into(),
            id: None,
        };
        for user_id in [2, 3] {
            app.on_gateway(Event::ReactionAdd {
                channel_id,
                message_id,
                user_id: Snowflake(user_id),
                emoji: emoji.clone(),
            });
        }
        let screen = render(&mut app, 100, 24);
        // The test terminal pads wide characters with a space.
        assert!(
            screen.lines().any(|line| line.contains("🔥") && line.contains(" 2 ")),
            "{screen}"
        );
    }

    #[tokio::test]
    async fn hidden_and_muted_channels() {
        use plume_core::models::{GuildSettings, Overwrite, OverwriteKind, Role};
        let mut app = demo_app();
        app.on_gateway(Event::ChannelUpsert(Channel {
            overwrites: vec![Overwrite {
                id: Snowflake(10),
                kind: OverwriteKind::Role,
                allow: 0,
                deny: 1 << 10,
            }],
            ..channel(102, Some(10), Some("secret-staff"), ChannelKind::Text, Some(99))
        }));
        app.on_gateway(Event::RoleUpsert {
            guild_id: Snowflake(10),
            role: Role {
                id: Snowflake(10),
                permissions: 1 << 10,
            },
        });
        app.on_gateway(Event::MemberUpdate {
            guild_id: Snowflake(10),
            user_id: Snowflake(1),
            roles: vec![],
        });
        let screen = render(&mut app, 100, 24);
        assert!(!screen.contains("secret-staff"));
        assert!(screen.contains("# general"));

        // L lists locked channels too; opening one shows a notice and loads nothing.
        app.focus = Focus::Sidebar;
        press(&mut app, KeyCode::Char('L'));
        let screen = render(&mut app, 100, 24);
        assert!(screen.contains("secret-staff") && screen.contains("🔒"));
        app.open_channel(Snowflake(102));
        assert!(!app.loading_history);
        assert!(render(&mut app, 100, 24).contains("permission to read"));
        press(&mut app, KeyCode::Char('x'));
        press(&mut app, KeyCode::Enter);
        assert!(
            app.error
                .as_ref()
                .is_some_and(|(e, _)| e.contains("permission to send"))
        );
        app.focus = Focus::Sidebar;
        press(&mut app, KeyCode::Char('L'));
        app.open_channel(Snowflake(100));
        assert!(!render(&mut app, 100, 24).contains("secret-staff"));

        app.on_gateway(Event::GuildSettingsUpdate(GuildSettings {
            guild_id: Some(Snowflake(10)),
            muted: false,
            muted_channels: vec![Snowflake(101)],
        }));
        app.on_gateway(Event::MessageCreate {
            message: message(200, 3, "noise"),
            author: user(3, "Bob"),
        });
        assert!(app.store.is_muted(Snowflake(101)));
    }

    #[tokio::test]
    async fn threads_and_forums_in_the_sidebar() {
        let mut app = demo_app();
        let thread = |id: u64, parent: u64, name: &str, last: u64| Channel {
            last_message_id: Some(Snowflake(last)),
            ..channel(id, Some(10), Some(name), ChannelKind::Thread, Some(parent))
        };
        app.on_gateway(Event::ChannelUpsert(thread(500, 100, "old-thread", 1)));
        app.on_gateway(Event::ChannelUpsert(thread(501, 100, "busy-thread", 9)));
        app.on_gateway(Event::ChannelUpsert(channel(
            300,
            Some(10),
            Some("ideas"),
            ChannelKind::Forum,
            Some(99),
        )));
        app.on_gateway(Event::ThreadsSync {
            guild_id: Snowflake(10),
            threads: vec![thread(502, 300, "first-post", 5)],
        });

        let entries = sidebar::guild_channels(&app.store, app.store.guild(Snowflake(10)).unwrap(), false);
        let ids: Vec<u64> = entries
            .iter()
            .map(|e| match e {
                Entry::Channel(id) | Entry::Category(id) => id.0,
                _ => 0,
            })
            .collect();
        assert_eq!(
            ids,
            [99, 100, 501, 500, 101, 300, 502],
            "threads under their channel, busiest first"
        );

        let screen = render(&mut app, 100, 24);
        assert!(
            screen.contains("↳ busy-thread") && screen.contains("▤ ideas"),
            "{screen}"
        );

        // Forums aren't chats; their posts are.
        app.activate(Entry::Channel(Snowflake(300)));
        assert!(app.error.as_ref().is_some_and(|(e, _)| e.contains("pick one")));
        app.activate(Entry::Channel(Snowflake(502)));
        assert_eq!(app.store.active_channel(), Some(Snowflake(502)));

        // Archived (deleted) threads drop out.
        app.on_gateway(Event::ChannelDelete { id: Snowflake(501) });
        assert!(!render(&mut app, 100, 24).contains("busy-thread"));
    }

    #[tokio::test]
    async fn upload_command() {
        let mut app = demo_app();
        app.focus = Focus::Input;
        app.input.set("/upload".into());
        press(&mut app, KeyCode::Enter);
        assert!(app.error.as_ref().is_some_and(|(e, _)| e.starts_with("Usage")));
        assert_eq!(app.input.text(), "/upload", "kept so it can be fixed");

        // A failed read puts the command back in the box.
        app.input.take();
        app.on_task(TaskResult::Failed {
            text: "/upload missing.png".into(),
            error: "Can't open missing.png".into(),
        });
        assert_eq!(app.input.text(), "/upload missing.png");
        assert!(render(&mut app, 100, 24).contains("Can't open missing.png"));
    }

    #[tokio::test]
    async fn reconnecting_shows_in_the_status_bar() {
        let mut app = demo_app();
        app.on_gateway(Event::Disconnected);
        assert!(render(&mut app, 100, 24).contains("reconnecting"));
        app.on_gateway(Event::Resumed);
        assert!(render(&mut app, 100, 24).contains("online"));
    }

    #[test]
    fn react_prompt_understands_shortcodes_and_existing_reactions() {
        use plume_core::models::Reaction;
        assert_eq!(
            parse_emoji(":fire:", None),
            Emoji {
                name: "🔥".into(),
                id: None
            }
        );
        assert_eq!(
            parse_emoji("🦀", None),
            Emoji {
                name: "🦀".into(),
                id: None
            }
        );
        let custom = Emoji {
            name: "blob".into(),
            id: Some(Snowflake(5)),
        };
        let message = Message {
            reactions: vec![Reaction {
                emoji: custom.clone(),
                count: 1,
                me: false,
            }],
            ..Message::default()
        };
        assert_eq!(parse_emoji(":blob:", Some(&message)), custom);
    }

    #[test]
    fn finds_the_first_link() {
        assert_eq!(
            first_link("see https://example.com/a, ok").as_deref(),
            Some("https://example.com/a")
        );
        assert_eq!(first_link("`https://in.code`"), None);
    }

    #[tokio::test]
    async fn notifications_only_for_pings_you_are_not_looking_at() {
        let mut app = demo_app();
        app.notifications = true;
        let dm = |id| Message {
            channel_id: Snowflake(200),
            ..message(id, 3, "psst")
        };

        // A DM while you're in another channel.
        app.on_gateway(Event::MessageCreate {
            message: dm(300),
            author: user(3, "Bob"),
        });
        assert!(app.last_notified.contains_key(&Snowflake(200)));

        // Plain chatter in a server channel doesn't notify.
        let chatter = Message {
            channel_id: Snowflake(101),
            ..message(301, 3, "hello all")
        };
        app.on_gateway(Event::MessageCreate {
            message: chatter,
            author: user(3, "Bob"),
        });
        assert!(!app.last_notified.contains_key(&Snowflake(101)));

        // The open channel only notifies while the terminal is in the background.
        let mention = |id| Message {
            mentions: vec![Snowflake(1)],
            ..message(id, 3, "<@1> look")
        };
        app.on_gateway(Event::MessageCreate {
            message: mention(302),
            author: user(3, "Bob"),
        });
        assert!(!app.last_notified.contains_key(&Snowflake(100)));
        app.on_terminal(TermEvent::FocusLost);
        app.on_gateway(Event::MessageCreate {
            message: mention(303),
            author: user(3, "Bob"),
        });
        assert!(app.last_notified.contains_key(&Snowflake(100)));
    }

    #[tokio::test]
    async fn messages_cannot_inject_terminal_escapes() {
        let mut app = demo_app();
        app.on_gateway(Event::MessageCreate {
            message: message(400, 3, "evil \u{1b}]0;pwned\u{7} \u{1b}[2J text"),
            author: user(3, "Bob"),
        });
        let screen = render(&mut app, 100, 24);
        assert!(!screen.contains('\u{1b}') && !screen.contains('\u{7}'));
        assert!(screen.contains("evil"));
    }

    #[tokio::test]
    async fn embeds_show_title_link_and_description() {
        use plume_core::models::Embed;
        let mut app = demo_app();
        let message_id = app.store.messages(Snowflake(100)).last().unwrap().id;
        app.on_gateway(Event::MessageUpdate {
            id: message_id,
            channel_id: Snowflake(100),
            content: None,
            embeds: Some(vec![Embed {
                title: Some("Rust 2.0 released".into()),
                url: Some("https://blog.example/rust".into()),
                description: Some("It's finally here.\n\nSecond paragraph".into()),
            }]),
        });
        let screen = render(&mut app, 100, 24);
        assert!(
            screen.contains("Rust 2.0 released  https://blog.example/rust"),
            "{screen}"
        );
        assert!(screen.contains("It's finally here."));
        assert!(
            !screen.contains("my own words (edited)"),
            "an embed loading isn't an edit"
        );
    }

    #[tokio::test]
    async fn typing_reuses_the_rendered_messages() {
        let mut app = demo_app();
        app.focus = Focus::Input;
        render(&mut app, 100, 24);
        let before = app.view_cache.as_ref().map(|(key, _)| *key);
        app.input.insert('x');
        render(&mut app, 100, 24);
        assert_eq!(app.view_cache.as_ref().map(|(key, _)| *key), before);

        app.on_gateway(Event::MessageCreate {
            message: message(500, 3, "new"),
            author: user(3, "Bob"),
        });
        render(&mut app, 100, 24);
        assert_ne!(
            app.view_cache.as_ref().map(|(key, _)| *key),
            before,
            "new messages rebuild it"
        );
    }

    /// `cargo test --release -p plume-tui -- --ignored --nocapture render_speed`
    #[tokio::test]
    #[ignore = "a benchmark, run by hand"]
    async fn render_speed() {
        let mut app = App::new(
            RestClient::new(&Token::new("t")).unwrap(),
            Gateway::new(Token::new("t"), mpsc::channel(1).0).handle(),
            mpsc::unbounded_channel().0,
            &Options {
                profile: plume_core::Profile::FULL,
                silent_typing: true,
                notifications: false,
                show_locked: false,
            },
        );
        app.on_gateway(demo_app_ready());
        app.store.open_channel(Snowflake(100));
        let page = (0..500)
            .map(|i| {
                (
                    message(
                        i,
                        2 + i % 2,
                        "**bold** and *italic* with `code`, a <@1> ping and https://example.com/link ||spoiler||",
                    ),
                    user(2 + i % 2, "Alice"),
                )
            })
            .collect();
        app.store.insert_history(Snowflake(100), page);
        let start = std::time::Instant::now();
        for _ in 0..100 {
            app.view_cache = None;
            render(&mut app, 160, 50);
        }
        println!("uncached frame: {:.2} ms", start.elapsed().as_secs_f64() * 10.0);
        let start = std::time::Instant::now();
        for _ in 0..100 {
            render(&mut app, 160, 50);
        }
        println!("cached frame:   {:.2} ms", start.elapsed().as_secs_f64() * 10.0);
    }

    /// Random and hostile message text through the parser, the wrapper and the
    /// renderer at many widths. Run with `--ignored` for a much longer loop.
    mod stress {
        use super::*;

        const PIECES: &[&str] = &[
            "*",
            "**",
            "_",
            "__",
            "~~",
            "||",
            "`",
            "```",
            "> ",
            ">>> ",
            "# ",
            "-# ",
            "\\",
            "\n",
            " ",
            "  ",
            "<@1>",
            "<@&5>",
            "<#100>",
            "<:x:123>",
            "<a:y:456>",
            "<t:1700000000:R>",
            "<t:99999999999999:F>",
            "@everyone",
            "https://a.b/c?d=e",
            "<https://x.y>",
            "[t](https://z)",
            ":fire:",
            "🦀",
            "👨‍👩‍👧",
            "e\u{301}",
            "\u{200b}",
            "\u{202e}",
            "中文",
            "\t",
            "\u{1b}[31m",
            "\r",
            "word",
            "longwordwithoutanyspaces_longwordwithoutanyspaces",
        ];

        struct Rng(u64);
        impl Rng {
            fn next(&mut self) -> usize {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                self.0 as usize
            }
        }

        fn random_text(rng: &mut Rng) -> String {
            let len = rng.next() % 60;
            (0..len).map(|_| PIECES[rng.next() % PIECES.len()]).collect()
        }

        fn run(rounds: usize) {
            let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
            let mut app = demo_app();
            for round in 0..rounds {
                let mut message = message(10_000 + round as u64, 2, &random_text(&mut rng));
                message.reply_to = (round % 7 == 0).then_some(Snowflake(90));
                app.on_gateway(Event::MessageCreate {
                    message,
                    author: user(2, "Alice"),
                });
                let width = 1 + (rng.next() % 120) as u16;
                let height = 1 + (rng.next() % 40) as u16;
                render(&mut app, width, height);
            }
        }

        #[tokio::test]
        async fn random_messages_never_break_rendering() {
            run(300);
        }

        #[tokio::test]
        #[ignore]
        async fn random_messages_long() {
            run(20_000);
        }

        #[test]
        fn hostile_messages_render_quickly() {
            let cases = [
                "*".repeat(4000),
                "**a".repeat(1300),
                "||".repeat(2000),
                "`".repeat(4000),
                "> ".repeat(2000),
                "\\".repeat(4000),
                "<@".repeat(2000),
                "_".repeat(4000),
                "[a](".repeat(1000),
                "x".repeat(4000),
                "\n".repeat(2000),
            ];
            for text in cases {
                let start = std::time::Instant::now();
                let message = message(1, 2, &text);
                let mut store = plume_core::Store::new(plume_core::Profile::FEATHER);
                store.insert_history(Snowflake(100), vec![(message, user(2, "Alice"))]);
                let view = text::channel_lines(&store, Snowflake(100), 80);
                assert!(!view.lines.is_empty());
                let took = start.elapsed();
                assert!(took.as_millis() < 500, "{:?}… took {took:?}", &text[..10]);
            }
        }
    }
}
