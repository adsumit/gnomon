# gnomon — SPEC

Binding rules. Implementations MUST conform.

## Purpose

gnomon is a Wayland-native, always-on-top meter for Claude subscription limits.

## Data sources

Two transports. ONE schema. Both transports MUST deserialize into the same types.

### A) Direct OAuth poll

- `GET https://api.anthropic.com/api/oauth/usage`
- Headers:
  - `Authorization: Bearer <token>`
  - `anthropic-beta: oauth-2025-04-20`
  - `User-Agent: claude-code/<version>`
- Token source: `~/.claude/.credentials.json` at `.claudeAiOauth.accessToken`, or the
  environment variable `CLAUDE_CODE_OAUTH_TOKEN`.
- Minimum poll interval: 180s. gnomon MUST NOT issue requests more frequently.

### B) Chrome extension bridge

- The extension calls `claude.ai/api/organizations/{orgId}/usage` from inside the page.
- The extension delivers results over Chrome native messaging (stdio) to the
  `com.gnomon.bridge` host. It MUST NOT use HTTP to reach gnomon.
- The host forwards the payload to a Unix domain socket at
  `$XDG_RUNTIME_DIR/gnomon/bridge.sock`.
- The payload is semantically preserved but NOT byte-preserved on the
  extension-to-host hop. `sendNativeMessage` takes an object, so the extension parses the
  response text and Chrome re-serializes it; whitespace and number formatting are
  normalized by Chrome.
- gnomon opens no listening network socket, on loopback or otherwise. There is no port,
  so the extension holds no gnomon secret and there is nothing to authenticate.
- gnomon itself never reads a cookie.
- Refresh is event-driven: on request completion, and on tab becoming visible. Heartbeat
  60s while the tab is visible, 300s while hidden. Hard floor of 2s between fetches.
- Transport A, at its 180s minimum, remains the ONLY source that observes Claude Code CLI
  usage. Transport B observes browser usage only.

## Schema rules

- `limits[]` is the primary contract.
- `five_hour` and `seven_day` are a fallback, used ONLY when `limits` is absent or empty.
- `deny_unknown_fields` is forbidden everywhere.
- `kind`, `group`, and `severity` are `String`. They are never enums.
- `percent` parses as `f64`.
- Unknown `severity` values render with error styling, not warning styling.
- When `limits[]` is absent, synthesized windows carry `severity: None` and the render
  class is derived from percent: <75 normal, <90 warning, else error.
- `WindowSource` records whether a window came from `limits[]` or the legacy fallback.
- The API emits unreleased codenamed keys. They MUST be ignored silently and MUST NEVER
  cause a parse failure.
- Fixture `live_oauth.json` has unreleased codename keys renamed; see
  `tests/fixtures/README.md`.

## UI rule

Render one bar per entry in `limits[]`. Never hardcode a bar count.

## Security rules

- No credential is ever logged, cached to disk, or written to a crash dump.
- gnomon MUST NOT open a listening network socket, on loopback or otherwise.
- IPC is a Unix domain socket at `$XDG_RUNTIME_DIR/gnomon/bridge.sock`, mode 0600, inside
  a directory of mode 0700. Access control is filesystem permissions.
- The extension holds no gnomon secret. There is no port to authenticate against.

## Unverified, do not build on

- The meaning of `is_active`.
- ~~The server-side recompute granularity of `percent`.~~ FINDING: three consecutive
  readings seconds apart returned identical values with no usage in between, so `percent`
  changes only when usage is consumed. The GUI therefore skips redraws on unchanged
  payloads.
- Whether Chrome's Local Network Access prompt affects extension service workers. Native
  messaging was chosen over loopback so the answer does not matter.
