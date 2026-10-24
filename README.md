# Plume (working name)

A lightweight, **native** Discord client with selectable weight: 🪶 Feather (5–30 MB), 💬 Standard (50–100 MB), 🧩 Full with voice and plugins (100–250 MB). Same idea as Vesktop — a nicer desktop client — but without Electron or any embedded browser.

**Status:** the 🪶 Feather terminal client (`plume-tui`) works for daily text chat. GTK, voice and the rest are still planned. See the [roadmap](docs/09-roadmap.md).

> ⚠️ Third-party clients break Discord's Terms of Service. Use at your own risk. See [10-risks.md](docs/10-risks.md).

## Try it

Download a build for Linux, Windows or macOS from the latest **CI** run in the
Actions tab (see [PROGRESS.md](PROGRESS.md) for steps and what's new), or
build it yourself:

## Quick start

```bash
cargo build --release -p plume-tui
./target/release/plume-tui login     # scan the QR code with the Discord app
./target/release/plume-tui
```

If QR login says a captcha is required, log in with a token instead (it's checked with Discord, then kept in your system keyring):

```bash
read -rs DISCORD_TOKEN && export DISCORD_TOKEN
./target/release/plume-tui login --token
```

### Keys

| Where | Keys |
|---|---|
| Anywhere | `Tab` switch sidebar/input · `Ctrl+K` jump to a channel · `PgUp`/`PgDn` scroll · `Ctrl+C` quit |
| Sidebar | `↑`/`↓` or `j`/`k` move · `Enter`/`→` open or expand · `←` collapse · `i` start typing |
| Input | `Enter` send · `Esc` back to sidebar · `End` (empty box) jump to newest · `Ctrl+W` delete word · `Ctrl+U` clear |

Options: `--profile feather|standard|full` (how much to keep in memory), `--silent-typing`. Logs go to `~/.local/state/plume/plume-tui.log` (`%LOCALAPPDATA%\plume` on Windows).

## Crates

| Crate | What it does |
|---|---|
| `plume-core` | Data types, the in-memory store, profiles, the redacting `Token` type |
| `plume-gateway` | The live WebSocket connection: login, heartbeats, resume, reconnect, compression |
| `plume-rest` | HTTP API with rate limiting: history, sending, typing |
| `plume-auth` | QR login, token login, keyring storage |
| `plume-markdown` | Discord's markdown → syntax tree → styled spans |
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
