# 09 — Roadmap

## M0 — Skeleton ✅
Cargo workspace, CI (fmt, clippy, test), logging with token redaction, a GTK window that opens.

## M1 — Read-only client ✅ (except the SQLite startup snapshot)
QR and token login → keyring. Gateway connect/heartbeat/resume. Guild rail + channel tree. Message list with plain text and avatars. SQLite snapshot for instant startup.

## M1.5 — Feather spike ✅ (TUI done: 7.9 MB release binary, ~5 MB before login; `plume-lite` not started)
Build `plume-tui` against the M1 core. Add the READY stream filter. Measure RSS with the heavy-account fixture and prove that 5–30 MB holds. Prototype `plume-lite` on the Slint software renderer and compare it with FLTK.

## M2 — Daily driver (text)
Send/edit/delete, replies, the full markdown renderer, embeds, attachments (view + upload), reactions, emoji picker, typing indicators, unread/mention state + ack, notifications, DMs and group DMs, threads, quick switcher, settings page.

## M3 — Voice (works in the TUI too)
Join/leave a voice channel and DM calls, audio in/out, DAVE, mute/deafen, push-to-talk, per-user volume.
- **Pieces:** voice gateway (WebSocket) → UDP with IP discovery → RTP with transport encryption (`aead_aes256_gcm_rtpsize`) → **DAVE** (Discord's MLS-based end-to-end encryption, required for calls) → Opus encode/decode → `cpal` for microphone and speakers.
- **DAVE** is the risky part: reuse an existing open-source Rust implementation instead of writing MLS ourselves, and test it against recorded handshakes.
- **Behind the `voice` cargo feature**, so Feather builds without it stay small. Budget: +5–15 MB RSS while in a call, ~0% CPU when not.

## M3.5 — Video and screen share (GUI only)
A terminal can't show live video, so this needs `plume-gtk` (or `plume-lite`) first.
1. **Watch** others' cameras and streams: video RTP depacketizing, VP8/H.264/AV1 decode (hardware where available), render to a texture.
2. **Send** camera and screen: capture (PipeWire portal on Linux, platform APIs elsewhere), encode, simulcast/bitrate control.
Large effort; not promised for the first release.

## M4 — Polish and distribution
Themes and the built-in feature toggles, a profile switcher with a live RSS readout, `plume-lite` reaching parity with the TUI, Flatpak (Flathub), RPM/COPR (Fedora), AUR, AppImage. Windows/macOS builds if GTK behaves well there.

## M5 — Stretch
Plugin API, screen share/video, multiple accounts, search UI.

## Progress notes
- **Done in the TUI from M2:** sending, the markdown renderer, attachments (as links), mentions, typing indicators (both ways), unread and mention counts, DMs and group DMs, the quick switcher.
- **Still open from M2:** edit/delete/reply from the client, reactions, emoji picker, embeds, threads, notifications, read state synced with Discord (unread counts currently start from zero at launch), loading older history while scrolling, and hiding channels you can't view (needs `permissions.rs`).
- READY is parsed straight from borrowed JSON into typed structs, so fields Plume doesn't use are skipped without being allocated. That covers most of what `ready_filter.rs` was planned for; measure with a big account before doing more.
