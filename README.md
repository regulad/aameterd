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
`key_listings`, `key_creations`, `key_signings`, or `key_deletions`. The
`requests` view merges them into one timeline. Every row records:

- when the request arrived
- the source IP (the last `X-Forwarded-For` entry, else `X-Real-Ip`, else
  the TCP peer)
- the status and any error code
- the operation's own fields, such as the app/team/bundle ID, key, and
  client data hash

The columns are documented in [`src/schema.sql`](src/schema.sql).

```bash
sqlite3 aameterd.sqlite3 "SELECT requested_at, operation, source_ip, status FROM requests ORDER BY requested_at"
```

## License

All original work in this repository is licensed under
[AGPLv3](./LICENSE.md).
