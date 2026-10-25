# Plume progress log

A running log of what's been fixed and added, newest first, so you can see
what changed between test builds.

## How to try the latest build

Every push builds Plume for Linux, Windows and macOS.

1. Open the repo's **Actions** tab and click the newest green **CI** run
   (the branch name is shown next to it).
2. Scroll to **Artifacts** and download the one for your system:
   `plume-tui-windows-x86_64`, `plume-tui-macos-arm64`,
   `plume-tui-linux-x86_64-voice` (Linux with voice calls) or
   `plume-tui-linux-x86_64` (Linux, text only, runs anywhere).
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

### Milestone 7: voice calls (in review)

- **Voice channels in the sidebar:** voice and stage channels (`🔊`) show
  who's in them underneath, with `●` lit while someone talks and
  `muted`/`deaf` next to their name. DMs show who's in a call too.
- **Join and leave:** `Enter` on a voice channel joins it and shows its
  text chat. In a DM, type `/call` to start a call; the other person's
  Discord rings. `/leave` (or `Alt+L` from anywhere) hangs up.
- **Mute and deafen:** `Alt+M` and `Alt+D` from anywhere while in a call.
  Deafening mutes you too; unmuting undeafens, like Discord. The status bar
  shows the call, `[muted]`/`[deafened]`, and 🔒 when the call is end-to-end
  encrypted.
- **Per-person volume:** `/volume alice 50` makes Alice quieter (0–200%,
  part of a name is enough). It's shown next to their name and kept for
  later calls while Plume is open.
- **End-to-end encryption (DAVE):** calls use Discord's new end-to-end
  encryption when everyone supports it. `/privacy` shows the call's privacy
  code, which matches what the official app shows for the same call.
- **Light on resources:** nothing voice-related runs until you join a call.
  Audio is Opus at 64 kbps with a voice gate, so silence isn't sent.
  Reconnects on its own if the voice server drops.
- **Linux:** voice needs the system sound library (ALSA, already on every
  desktop Linux), so there are now two Linux downloads: `linux-x86_64-voice`
  and the static text-only `linux-x86_64`.
- Video and screen sharing are not in the terminal client.

### Milestone 6: ready to share (in review)

- **Releases:** pushing a version tag (like `v0.2.0`) publishes a GitHub
  Release that anyone can download, no GitHub account needed. Builds:
  Windows, macOS Apple Silicon, macOS Intel, and Linux, with checksums.
- **Linux builds are static** (musl), so they run on any distribution. They
  also use less memory: ~3.1 MB after a heavy login instead of ~5.3 MB.
- **First run logs you in:** starting `plume-tui` without a saved login now
  opens the QR login and then the client, so friends only need one command.
- **Windows:** if it's started by double-clicking and hits an error, the
  window stays open so the message can be read.

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
- **Threads and forums:** active threads appear under their channel (`↳`),
  busiest first, and open like any channel. Forum channels (`▤`) list their
  posts underneath. Archived threads disappear. Threads follow their
  channel's permissions.
- **Stress tests:** 20,000 random messages full of markdown, emoji, control
  characters and odd widths render without a crash; worst-case inputs
  (thousands of `*`, `||`, backticks) still render in milliseconds.
  114 tests now.

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
