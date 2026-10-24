use std::collections::HashMap;
use std::error::Error;
use std::io::stdout;
use std::time::{Duration, Instant};

use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use futures_util::StreamExt;
use plume_core::models::{Message, User};
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

/// Discord's limit for one message (without Nitro).
const MAX_MESSAGE_LENGTH: usize = 2000;
/// Discord shows "is typing" for about 10 seconds per trigger.
const TYPING_TIMEOUT: Duration = Duration::from_secs(10);
/// So we don't re-send "typing" on every key press.
const TYPING_RESEND: Duration = Duration::from_secs(8);
const ERROR_TIMEOUT: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Input,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connection {
    Connecting,
    Online,
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
    Sent {
        text: String,
        result: Result<(Message, User), RestError>,
    },
}

/// Everything the UI shows and remembers. `render.rs` only reads it.
pub struct App {
    pub store: Store,
    pub sidebar: Sidebar,
    pub sidebar_state: ListState,
    pub focus: Focus,
    pub input: Input,
    /// Lines scrolled up from the newest message.
    pub scroll: usize,
    /// Set by the renderer so PgUp/PgDn can move by half a screen.
    pub messages_height: u16,
    pub loading_history: bool,
    pub connection: Connection,
    pub error: Option<(String, Instant)>,
    pub memory_kb: Option<u64>,
    pub options_tier: Tier,
    pub switcher: Option<Switcher>,

    rest: RestClient,
    gateway: GatewayHandle,
    tasks: mpsc::UnboundedSender<TaskResult>,
    /// Who's typing where, and when we last heard about it.
    typing: HashMap<Snowflake, Vec<(Snowflake, Instant)>>,
    last_typing_sent: Option<(Snowflake, Instant)>,
    silent_typing: bool,
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
    let _ = crossterm::execute!(stdout(), EnableBracketedPaste);
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

    let _ = crossterm::execute!(stdout(), DisableBracketedPaste);
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
            sidebar: Sidebar::default(),
            sidebar_state: ListState::default(),
            focus: Focus::Sidebar,
            input: Input::default(),
            scroll: 0,
            messages_height: 0,
            loading_history: false,
            connection: Connection::Connecting,
            error: None,
            memory_kb: resident_memory_kb(),
            options_tier: options.profile.tier,
            switcher: None,
            rest,
            gateway,
            tasks,
            typing: HashMap::new(),
            last_typing_sent: None,
            silent_typing: options.silent_typing,
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
            Event::GuildCreate { .. }
            | Event::GuildDelete { .. }
            | Event::ChannelUpsert(_)
            | Event::ChannelDelete { .. } => self.sidebar.mark_dirty(),
            Event::MessageCreate { message, .. } => {
                // They sent it, so they're not typing any more.
                if let Some(typers) = self.typing.get_mut(&message.channel_id) {
                    typers.retain(|(user, _)| *user != message.author_id);
                }
                // DMs re-sort by activity.
                if self.store.channel(message.channel_id).is_some_and(sidebar::is_dm) {
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
            Event::MessageUpdate { channel_id, .. } | Event::MessageDelete { channel_id, .. } => {
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
            TaskResult::Sent { text, result } => match result {
                // Store it now; the gateway's copy of the same message is ignored.
                Ok((message, author)) => self.store.apply(Event::MessageCreate { message, author }),
                Err(error) => {
                    self.show_error(format!("Couldn't send: {error}"));
                    if self.input.is_empty() {
                        self.input.set(text); // don't lose what they typed
                    }
                }
            },
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

        let half_page = usize::from(self.messages_height / 2).max(1);
        match key.code {
            KeyCode::PageUp => self.scroll += half_page,
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(half_page),
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::Input,
                    Focus::Input => Focus::Sidebar,
                };
            }
            _ => match self.focus {
                Focus::Sidebar => self.sidebar_key(key),
                Focus::Input => self.input_key(key),
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
            _ => {}
        }
    }

    fn activate(&mut self, entry: Entry) {
        match entry {
            Entry::DmHeader => self.sidebar.toggle_dms(),
            Entry::Guild(id) => self.sidebar.toggle_guild(id),
            Entry::Dm(id) | Entry::Channel(id) => self.open_channel(id),
            Entry::ServerHeader | Entry::Category(_) => {}
        }
    }

    fn input_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => self.focus = Focus::Sidebar,
            KeyCode::Enter => self.send(),
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
                self.notify_typing();
            }
            _ => {}
        }
    }

    // ---- Actions ------------------------------------------------------------

    fn open_channel(&mut self, channel_id: Snowflake) {
        self.scroll = 0;
        self.focus = Focus::Input;
        let needs_history = self.store.open_channel(channel_id);
        self.sidebar.reveal_channel(&self.store, channel_id);

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

    fn send(&mut self) {
        let Some(channel_id) = self.store.active_channel() else {
            self.show_error("Open a channel first");
            return;
        };
        let text = self.input.text().trim().to_owned();
        if text.is_empty() {
            return;
        }
        let length = text.chars().count();
        if length > MAX_MESSAGE_LENGTH {
            self.show_error(format!("Message is too long ({length}/{MAX_MESSAGE_LENGTH})"));
            return;
        }

        self.input.take();
        self.scroll = 0;
        self.last_typing_sent = None;
        let rest = self.rest.clone();
        let tasks = self.tasks.clone();
        tokio::spawn(async move {
            let result = rest.send_message(channel_id, &text, None).await;
            let _ = tasks.send(TaskResult::Sent { text, result });
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
            for entry in sidebar::guild_channels(&self.store, guild) {
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
        }
    }

    /// An app with one server, a DM, and a few messages, without any network.
    fn demo_app() -> App {
        let options = Options {
            profile: plume_core::Profile::FEATHER,
            silent_typing: true,
        };
        let token = Token::new("test");
        let (events, _) = mpsc::channel(1);
        let handle = Gateway::new(token.clone(), events).handle();
        let (tasks, _) = mpsc::unbounded_channel();
        let mut app = App::new(RestClient::new(&token).unwrap(), handle, tasks, &options);

        app.on_gateway(Event::Ready {
            me: user(1, "Me"),
            guilds: vec![Guild {
                id: Snowflake(10),
                name: "Rust Hangout".into(),
                channels: vec![Snowflake(99), Snowflake(100), Snowflake(101)],
            }],
            channels: vec![
                channel(99, Some(10), Some("Text Channels"), ChannelKind::Category, None),
                channel(100, Some(10), Some("general"), ChannelKind::Text, Some(99)),
                channel(101, Some(10), Some("help"), ChannelKind::Text, Some(99)),
                channel(200, None, None, ChannelKind::Dm, None),
            ],
            users: vec![user(2, "Alice"), user(3, "Bob")],
        });

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
}
