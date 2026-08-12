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
- The extension pushes results to a loopback listener owned by gnomon.
- gnomon itself never reads a cookie.
- Target interval: 30s.

## Schema rules

- `limits[]` is the primary contract.
- `five_hour` and `seven_day` are a fallback, used ONLY when `limits` is absent or empty.
- `deny_unknown_fields` is forbidden everywhere.
- `kind`, `group`, and `severity` are `String`. They are never enums.
- `percent` parses as `f64`.
- Unknown `severity` values render with error styling, not warning styling.
- The API emits unreleased codenamed keys. They MUST be ignored silently and MUST NEVER
  cause a parse failure.
- Fixture `live_oauth.json` has unreleased codename keys renamed; see
  `tests/fixtures/README.md`.

## UI rule

Render one bar per entry in `limits[]`. Never hardcode a bar count.

## Security rules

- No credential is ever logged, cached to disk, or written to a crash dump.
- The loopback listener binds `127.0.0.1` only.
- The loopback listener requires a shared token.

## Unverified, do not build on

- The meaning of `is_active`.
- The server-side recompute granularity of `percent`.
