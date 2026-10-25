# Plume (working name)

A lightweight, **native** Discord client with selectable weight: 🪶 Feather (5–30 MB), 💬 Standard (50–100 MB), 🧩 Full with voice and plugins (100–250 MB). Same idea as Vesktop — a nicer desktop client — but without Electron or any embedded browser.

**Status:** the 🪶 Feather terminal client (`plume-tui`) works for daily chat: replies, edits, deletes, reactions, files, threads, unread state synced with your other devices, scrollback, and voice calls (end-to-end encrypted). GTK, video and the rest are still planned. See the [roadmap](docs/09-roadmap.md).

> ⚠️ Third-party clients break Discord's Terms of Service. Use at your own risk. See [10-risks.md](docs/10-risks.md).

## Install

Download the file for your system from the [latest release](../../releases/latest),
unpack it, and run `plume-tui` in a terminal. The first run walks you through
logging in: scan the QR code with the Discord app on your phone
(Settings → Scan QR Code).

- **Windows:** `plume-tui-…-windows-x86_64.zip`. Windows Terminal works best. If SmartScreen warns, choose "More info" → "Run anyway".
- **macOS:** `…-macos-arm64` for M1 and later, `…-macos-x86_64` for Intel. The build isn't signed, so run `xattr -d com.apple.quarantine plume-tui` once.
- **Linux:** `…-linux-x86_64-voice` for voice calls (needs ALSA, which every desktop distribution has), or `…-linux-x86_64`, a static text-only build for any distribution.

Want the newest work in progress? Each CI run in the Actions tab has builds too
(you need to be signed in to GitHub). See [PROGRESS.md](PROGRESS.md) for what's new.

## Build it yourself

On Linux, voice needs the ALSA headers (`sudo apt install libasound2-dev`, `sudo dnf install alsa-lib-devel`), or build without voice using `--no-default-features`.

```bash
cargo build --release -p plume-tui
./target/release/plume-tui login     # scan the QR code with the Discord app
./target/release/plume-tui
```

If QR login says a captcha is required, log in with a token instead (it's checked with Discord, then kept in your system keyring, or in a private file if there isn't one):

```bash
read -rs DISCORD_TOKEN && export DISCORD_TOKEN
./target/release/plume-tui login --token
```

### Keys

| Where | Keys |
|---|---|
| Anywhere | `Tab` switch sidebar/input · `Ctrl+K` jump to a channel · `PgUp`/`PgDn` scroll · `Ctrl+C` quit |
| In a call | `Alt+M` mute · `Alt+D` deafen · `Alt+L` leave |
| Sidebar | `↑`/`↓` or `j`/`k` move · `Enter`/`→` open, expand or join a voice channel · `←` collapse · `L` show/hide locked channels · `i` start typing |
| Input | `Enter` send · `/upload <file> [message]` send a file · `/call` call this DM · `/leave` hang up · `/volume <name> <0-200>` how loud someone is · `/privacy` show the call's privacy code · `↑` (empty box) pick a message · `Esc` back to sidebar (or cancel a reply/edit) · `End` (empty box) jump to newest · `Ctrl+W` delete word · `Ctrl+U` clear |
| Picking a message | `↑`/`↓` move (going past the top loads older messages) · `r` reply · `e` edit yours · `d` delete · `+` react · `1`–`9` toggle that reaction · `o` open its link or attachment · `Esc` back |

Type `:fire:`-style shortcodes and they're sent as emoji.

Settings: run `plume-tui config` to create a settings file, or use options: `--profile feather|standard|full` (how much to keep in memory), `--silent-typing`, `--no-notifications`, `--show-locked` (also list channels you can't read; toggle with `L` in the sidebar). Logs go to `~/.local/state/plume/plume-tui.log` (`%LOCALAPPDATA%\plume` on Windows).

## Crates

| Crate | What it does |
|---|---|
| `plume-core` | Data types, the in-memory store, profiles, the redacting `Token` type |
| `plume-gateway` | The live WebSocket connection: login, heartbeats, resume, reconnect, compression |
| `plume-rest` | HTTP API with rate limiting: history, sending, typing |
| `plume-auth` | QR login, token login, keyring storage |
| `plume-markdown` | Discord's markdown → syntax tree → styled spans |
| `plume-notify` | Desktop notifications |
| `plume-voice` | Voice calls: voice gateway, encrypted RTP over UDP, DAVE end-to-end encryption, microphone/speakers |
| `plume-tui` | 🪶 Feather frontend (ratatui) |

## Docs

| Doc | What it covers |
|---|---|
| [01-vision.md](docs/01-vision.md) | Goals, non-goals, and what "lightweight" means here |
| [02-architecture.md](docs/02-architecture.md) | Crates, data flow, threading |
| [03-tech-stack.md](docs/03-tech-stack.md) | Language and toolkit choices, and why |
| [04-discord-protocol.md](docs/04-discord-protocol.md) | Gateway, REST, rate limits, login |
| [05-ui-design.md](docs/05-ui-design.md) | Layout, widgets, keyboard UX, theming |
| [06-performance-budget.md](docs/06-performance-budget.md) | Hard numbers we hold ourselves to |
| [07-voice.md](docs/07-voice.md) | Voice/video plan (the hardest part) |
| [08-extensibility.md](docs/08-extensibility.md) | Themes and plugins without a JS runtime |
| [09-roadmap.md](docs/09-roadmap.md) | Milestones in order |
| [10-risks.md](docs/10-risks.md) | ToS, account safety, API drift |
| [11-usage-profiles.md](docs/11-usage-profiles.md) | Feather (5–30 MB) / Standard / Full tiers |
| [adr/](docs/adr/) | Architecture decision records |
