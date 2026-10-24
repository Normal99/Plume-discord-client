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
