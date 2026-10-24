# Plume progress log

A running log of what's been fixed and added, newest first, so you can see
what changed between test builds.

## How to try the latest build

Every push builds Plume for Linux, Windows and macOS.

1. Open the repo's **Actions** tab and click the newest green **CI** run
   (the branch name is shown next to it).
2. Scroll to **Artifacts** and download the one for your system:
   `plume-tui-linux-x86_64`, `plume-tui-windows-x86_64` or `plume-tui-macos-arm64`.
3. Unzip it and log in, then start the client:

   ```bash
   ./plume-tui login        # scan the QR code with the Discord phone app
   ./plume-tui
   ```

   On Linux and macOS you may need `chmod +x plume-tui` first. On Windows,
   run `plume-tui.exe` from a terminal (Windows Terminal works best).

Logs go to `~/.local/state/plume/plume-tui.log` (`%LOCALAPPDATA%\plume` on
Windows). If something breaks, that file is the most useful thing to send.

## Changelog

### Milestone 5: files and settings (in review)

- **Send files:** type `/upload <file>` in the message box, optionally followed
  by a message (`/upload ~/cat.png look at him`). Quoted paths work, so you
  can drag a file into the terminal. Uses reply mode if you're replying.
  Files over 10 MB are refused before uploading. If it fails, the command
  goes back in the box.
- **Settings file:** `plume-tui config` creates and shows `config.toml`
  (`~/.config/plume/` on Linux/macOS, `%APPDATA%\plume\` on Windows) with
  `profile`, `silent_typing`, `notifications` and `show_locked`. Typos are
  reported with the line number. Command-line options still win.

### Milestone 4: speed and size (in review)

- **Smaller download:** the release build is 5.3 MB, down from 8.3 MB
  (size-optimised, no unwinding). The terminal is still restored on a crash.
- **Typing no longer redraws the chat:** the message view is cached and only
  rebuilt when something changes. A 500-message frame drops from 3.9 ms to
  0.6 ms.
- **Memory after login:** a synthetic heavy account (100 servers × 60
  channels, 300 DMs, a 4.3 MB READY) loads in ~27 ms and settles at ~5.3 MB
  RSS; spare heap is handed back to the OS after login (Linux).
  Try it: `cargo run --release -p plume-gateway --example ready_bench`.
- **Locked channels (optional):** press `L` in the sidebar (or start with
  `--show-locked`) to also list channels you can't read, with a 🔒. Opening one
  shows a notice instead of an error. Press `L` again to hide them.

### Milestone 3: reliability (in review)

- **Login without a system keyring:** on servers, WSL or minimal Linux
  installs, the token is saved to `~/.config/plume/token`, readable only by
  you. `plume-tui logout` removes it.
- **Desktop notifications** for DMs and mentions you aren't looking at
  (Linux notification daemon, macOS Notification Center; other terminals get
  a terminal notification and bell). Muted channels stay quiet, and each
  channel notifies at most every 5 seconds. Turn off with `--no-notifications`.
- **Embeds** (link previews, bot messages) show their title, link and a short
  description.
- **Safety:** messages can't inject terminal escape codes (now tested).

### Milestone 2: everyday chat features (in review)

- **Reply, edit, delete:** with the box empty, press `↑` to pick a message,
  then `r` to reply, `e` to edit your own, `d` to delete (it asks first).
- **Reactions:** shown under each message with counts (yours highlighted),
  live as people react. `+` reacts with any emoji or `:shortcode:`, and `1`–`9`
  toggles an existing one.
- **Older messages:** scrolling past the top loads the page before. Memory
  stays bounded: scrollback is dropped when you leave the channel.
- **Unread state synced with Discord:** what you've read on your phone shows
  as read here at launch, and reading here marks it read everywhere. Channels
  aren't marked read while the terminal window is in the background.
- **Muted servers and channels** are dimmed and don't count as unread (pings
  still do).
- **Channels you can't see are hidden**, using your roles and the channel's
  permissions, same as the official client.
- **`:shortcode:` emoji** in messages are sent as real emoji (`:fire:` → 🔥).
- **`o` opens a link or attachment** in your browser (web links only).
- **Connection status** shows "reconnecting…" when the network drops.
- 97 tests now.

### Milestone 1: it builds again (in review)

The original upload was missing the two crates everything depends on, so
nothing compiled. They're rebuilt to match how the rest of the code uses them.

- **plume-core rebuilt:** Discord IDs, the token type (never printed in logs),
  message/channel/server/user models, JSON parsing that tolerates fields
  Discord adds or leaves out, and the Feather/Standard/Full profiles.
- **Message store:** keeps only a few channels' messages in memory (2 × 50 on
  Feather), merges live messages that arrive while history is loading, never
  shows your own message twice, tracks unread counts and pings, and refetches
  the open channel after a reconnect so nothing is missed.
- **plume-gateway rebuilt:** the live connection with zlib compression,
  heartbeats, dead-connection detection, resume after network drops,
  reconnect with backoff, and a clear error when the token is rejected.
- **Announcement channels** now show in the channel list.
- **Tests:** a fake Discord server on localhost checks login, reconnecting,
  resuming, heartbeats and bad tokens on every build, plus unit tests for the
  store, parsing and compression. 67 tests in total.
- **CI:** formatting, lint and tests on every push, plus downloadable builds.
