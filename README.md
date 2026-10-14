# aameterd

Metering proxy for [`aaoracled`](https://github.com/regulad/aaoracled). It
serves the same HTTP contract
([`openapi.yaml`](https://github.com/regulad/aaoracled/blob/master/openapi.yaml)),
forwards every request to a real `aaoracled`, and records each one into
SQLite. Put it between a fake `devicecheckd` and the device to see every App
Attest operation an app under analysis makes:

```
app in Simulator ──▶ fake devicecheckd ──HTTP──▶ aameterd ──HTTP──▶ aaoracled (device)
                                                    │
                                                    ▼
                                             aameterd.sqlite3
```

Created by [Parker Wahle](https://github.com/regulad). Anthropic's Claude 5 series models
(Fable, Opus, Sonnet & Haiku) were used to rapidly iterate.

## Run

```bash
echo AAORACLED_HOST=<aaoracled hostname or IP> > .env
docker compose up -d --build
```

or, without Docker, `cargo run --release -- --upstream http://<aaoracled host>:8181`.
Clients then talk to `:8182` instead of `:8181`. The metering database is
kept in the stack's `aameterd` volume.

`aaoracled` itself only listens on the device's loopback, so the host must
be something that forwards to it, e.g. an SSH tunnel
(`ssh -g -L 8181:127.0.0.1:8181 <device-alias>`).

## Metering

Every request is a row in the table for its operation: `health_checks`,
`key_creations`, `key_attestations`, `key_assertions`, `label_listings`, or
`label_deletions` (plus `label_signings`, which only holds requests to the
sign route that aaoracled 0.3.0 removed). The `requests` view merges them into
one timeline. Every row records:

- when the request arrived
- the source IP (the last `X-Forwarded-For` entry, else `X-Real-Ip`, else
  the TCP peer)
- the status and any error code
- the operation's own fields, such as the app/team/bundle ID, keyId or
  label, and client data hash

Databases from older aameterd versions are upgraded in place on startup.

The columns are documented in [`src/schema.sql`](src/schema.sql).

```bash
sqlite3 aameterd.sqlite3 "SELECT requested_at, operation, source_ip, status FROM requests ORDER BY requested_at"
```

## Live counts

aameterd also serves its own read-only API over the metering database,
[`openapi.yaml`](openapi.yaml), on `:8183` (`--stats-listen` /
`AAMETERD_STATS_LISTEN`), so an app can be watched while it runs:

```bash
curl 'http://127.0.0.1:8183/attestations?from=2026-10-11T19:00:00Z&until=2026-10-11T20:00:00Z'
```

returns how many attestKey calls arrived in that hour as a bare JSON
integer, such as `3`. `/assertions` takes the same parameters and counts
assertKey calls instead. An app usually attests each key once and then
asserts on every request, so `/assertions` is the count that follows its
activity.

Every call counts, including ones that failed. A call is recorded only once
it finishes, which can take as long as the upstream timeout (120 s by
default). It is then counted at the time it arrived, so a window's count can
still go up after the window has closed.

## License

All original work in this repository is licensed under
[AGPLv3](./LICENSE.md).
