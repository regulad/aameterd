# AGENTS.md — aameterd

aameterd is a metering proxy in front of `aaoracled` (sibling repo
`../aaoracled`; read its `AGENTS.md` for the research context). It is part
of the same authorized security research, and **aaoracled's scope
constraints apply here unchanged**. See `README.md` for usage and the
schema overview.

## Commit requirements

Every commit created or rewritten by an AI agent in this repository must
carry a `Co-Authored-By` trailer naming the agent and model, for example:

    Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>

Keep Git commit signing enabled; never disable it.

## Rules

- **The contract is `../aaoracled/openapi.yaml`.** Don't fork a copy into
  this repo. If the contract changes there, update `src/proxy.rs`'s `Route`
  (and the metering in `action()`) to match.
- **`Route::parse` mirrors `oracled.m`'s `route()`, quirks included.** It
  matches on the raw, still-percent-encoded path and forwards the key
  segment byte-for-byte, so aaoracled does its own decoding. Don't swap it
  for axum path routing: that decodes `%2F`, answers 405 where aaoracled
  answers 404, and re-encodes keys. If `route()` changes, change both
  `Route::parse` and the mock in `tests/proxy.rs`.
- **Metering never fails a request.** Log the error and return aaoracled's
  answer anyway.
- **Schema changes go in `src/schema.sql`.** Every table keeps the shared
  leading columns, and so does the `requests` view. The schema is applied
  with `CREATE ... IF NOT EXISTS`, which won't alter existing tables, so a
  column change to a table needs a real migration (`PRAGMA user_version`).

## Commands

- `cargo build`, `cargo test`: safe, self-contained (the tests use a mock
  aaoracled on loopback and a temp database).
- `cargo run`: talks to whatever is on `--upstream` (default the device
  tunnel on `:8181`). Against the real device, `POST /keys` mints a real
  key with an Apple round trip and `DELETE /keys/{key}` really deletes one.
