# Fixtures

`live_oauth.json` is a real capture from the OAuth usage endpoint. Eight unreleased
Anthropic codename keys were renamed to `unreleased_a` through `unreleased_h`. Values
and structure are otherwise unmodified.

The unredacted capture is `live_oauth.raw.json`, which is gitignored and never
committed.

The other three fixtures are hand-written edge cases:

- `legacy_no_limits.json` — `five_hour` and `seven_day` present, no `limits` key.
- `unknown_shape.json` — one unrecognised `limits` entry plus unknown top-level keys.
- `empty.json` — empty `limits` array, all other known keys null.

`live_oauth.json` is FROZEN. It was captured 2026-08-12 and its exact percent and
severity values are asserted in `tests/parse_fixtures.rs`. Do not re-capture it without
updating those assertions — the session window resets on a rolling schedule, so a fresh
capture will almost never match. `explicit_warning.json` is hand-written to cover
explicit severity passthrough, which the frozen capture happens not to exercise.
