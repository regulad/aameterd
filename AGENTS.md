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

- **The contract is `../aaoracled/openapi.yaml`; aameterd tracks 0.3.0.**
  Don't fork a copy into this repo. If the contract changes there, update
  `src/proxy.rs`'s `Route` (and the metering in `action()`), plus the mock
  in `tests/proxy.rs`, to match.
- **`Route::parse` mirrors `oracled.m`'s `route()`, quirks included.** It
  matches on the raw, still-percent-encoded path and forwards the keyId or
  label segment byte-for-byte, so aaoracled does its own decoding. Don't
  swap it for axum path routing: that decodes `%2F`, answers 405 where
  aaoracled answers 404, and re-encodes identifiers. If `route()` changes,
  change both `Route::parse` and the mock in `tests/proxy.rs`.
- **Keep keyIds and labels apart.** They are different identifiers (the
  contract splits them into `/keys` and `/labels`); in the schema a keyId
  is always `key_id` and a label is always `label`, in `key_*` and
  `label_*` tables respectively.
- **Metering never fails a request.** Log the error and return aaoracled's
  answer anyway.
- **Schema changes go in `src/schema.sql`.** Every table keeps the shared
  leading columns, and so does the `requests` view. `CREATE ... IF NOT
  EXISTS` won't alter an existing table, so changing one also takes a new
  entry at the end of `MIGRATIONS` in `src/meter.rs`, leaving the table
  exactly as `schema.sql` declares it (new columns go last, where `ALTER
  TABLE ADD COLUMN` puts them). `PRAGMA user_version` counts applied
  migrations. New tables and indexes need no migration, and the `requests`
  view is dropped and rebuilt from `schema.sql` on every start.
  `tests/meter.rs` migrates the 0.1.0 schema
  (`tests/fixtures/schema-v0.sql`, never edit it) and checks the result
  matches a new database.

## Commands

- `cargo build`, `cargo test`: safe, self-contained (the tests use a mock
  aaoracled on loopback and a temp database).
- `cargo run`: talks to whatever is on `--upstream` (default the device
  tunnel on `:8181`). Against the real device, `POST /keys` mints a real
  key, `POST /keys/{keyId}/attest` makes an Apple CA round trip, and
  `DELETE /labels/{label}` really deletes a credential.
