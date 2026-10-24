# 04 — Discord protocol plan

## Gateway
- Fetch the URL from `GET /gateway`, then connect with `?v=10&encoding=json&compress=zlib-stream`.
- Lifecycle: HELLO → start heartbeating (with jitter) → IDENTIFY (user-client properties and capabilities) → READY / READY_SUPPLEMENTAL.
- **Resume:** store `session_id`, `resume_gateway_url`, and the last sequence number. When the connection drops, RESUME first. On INVALID_SESSION(false), do a fresh IDENTIFY.
- Reconnect backoff: 1s → 2s → 4s … capped at 60s, with jitter.
- Dispatches needed for the MVP: READY, GUILD_CREATE/UPDATE/DELETE, CHANNEL_*, THREAD_*, MESSAGE_CREATE/UPDATE/DELETE/DELETE_BULK, MESSAGE_REACTION_*, TYPING_START, PRESENCE_UPDATE, GUILD_MEMBER_LIST_UPDATE, MESSAGE_ACK, USER_SETTINGS_PROTO_UPDATE.
- Fixtures: record real (scrubbed) frames into `tests/fixtures/gateway/` so the parser can be tested offline.

## REST
- Base: `https://discord.com/api/v10`.
- Rate limits: read the `X-RateLimit-Bucket`, `-Remaining`, and `-Reset-After` headers. Keep one queue per bucket plus a global limiter. On 429, honour `retry_after`. **Never** blast requests in parallel.
- MVP routes: messages (get/send/edit/delete), reactions, typing, ack, attachment upload (pre-signed upload URL flow), user/guild profile, search (later).

## Login
1. **QR remote auth (preferred).** Connect to the remote-auth gateway, show a QR code, the user scans it with the mobile app, and we receive the token. No password ever touches our app, and no captcha.
2. **Paste token.** Advanced fallback.
3. Email/password login is **out of scope**. It needs captcha and MFA flows and is the riskiest path.

The token lives in the OS keyring. On systems without one (servers, minimal installs, WSL) it falls back to `~/.config/plume/token` with mode 0600, and `logout` removes both. It is never written to config files or logs (the `Token` type redacts itself in `Debug`).

## Client identity
Send super-properties and headers consistent with a desktop client and keep them in one place (`plume-rest/src/client.rs`) so they're easy to update when Discord changes them.
