-- aameterd metering schema: one table per aaoracled contract operation
-- (see ../aaoracled/openapi.yaml), one row per request aameterd routed —
-- including ones aaoracled rejected or that never reached it.
--
-- Every table starts with the same columns:
--
--   requested_at     RFC 3339 UTC timestamp (ms precision) when aameterd
--                    received the request.
--   source_ip        Best guess at the client: the rightmost
--                    X-Forwarded-For entry, else X-Real-Ip, else the TCP
--                    peer's IP. Those headers are unverified and taken at
--                    face value — compare against peer_addr.
--   peer_addr        TCP peer (ip:port) that actually connected to aameterd.
--   x_forwarded_for  Raw X-Forwarded-For value(s), comma-joined, if sent.
--   x_real_ip        Raw X-Real-Ip value, if sent.
--   user_agent       Raw User-Agent value, if sent.
--   status           HTTP status returned to the client: aaoracled's own, or
--                    aameterd's 502 when aaoracled couldn't be reached.
--   error            `error` code from an error response body, if any.
--   duration_ms      Time spent waiting on aaoracled.

-- Adding a column to an existing table also needs an entry in meter.rs's
-- MIGRATIONS: CREATE TABLE IF NOT EXISTS leaves existing tables alone.

CREATE TABLE IF NOT EXISTS health_checks (            -- GET /health
    id                     INTEGER PRIMARY KEY,
    requested_at           TEXT    NOT NULL,
    source_ip              TEXT    NOT NULL,
    peer_addr              TEXT    NOT NULL,
    x_forwarded_for        TEXT,
    x_real_ip              TEXT,
    user_agent             TEXT,
    status                 INTEGER NOT NULL,
    error                  TEXT,
    duration_ms            INTEGER NOT NULL,
    app_attest_supported   INTEGER,                   -- response's appAttestSupported (0/1)
    supported_environments TEXT,                      -- response's supportedEnvironments, as a JSON array
    environment_forgery    INTEGER                    -- response's environmentForgery (0/1)
);

CREATE TABLE IF NOT EXISTS key_listings (             -- GET /keys
    id              INTEGER PRIMARY KEY,
    requested_at    TEXT    NOT NULL,
    source_ip       TEXT    NOT NULL,
    peer_addr       TEXT    NOT NULL,
    x_forwarded_for TEXT,
    x_real_ip       TEXT,
    user_agent      TEXT,
    status          INTEGER NOT NULL,
    error           TEXT,
    duration_ms     INTEGER NOT NULL,
    key_count       INTEGER                           -- number of keys in the response
);

CREATE TABLE IF NOT EXISTS key_creations (            -- POST /keys
    id               INTEGER PRIMARY KEY,
    requested_at     TEXT    NOT NULL,
    source_ip        TEXT    NOT NULL,
    peer_addr        TEXT    NOT NULL,
    x_forwarded_for  TEXT,
    x_real_ip        TEXT,
    user_agent       TEXT,
    status           INTEGER NOT NULL,
    error            TEXT,
    duration_ms      INTEGER NOT NULL,
    app_id           TEXT,                            -- requested appId, verbatim
    team_id          TEXT,                            -- app_id before its first '.'
    bundle_id        TEXT,                            -- app_id after its first '.'
    environment      TEXT,                            -- requested environment; NULL if omitted (development) or not a string
    client_data_hash TEXT,                            -- requested clientDataHash (base64), verbatim
    key_id           TEXT,                            -- response's keyId (base64); NULL unless minted
    attestation      TEXT                             -- response's attestation (base64 CBOR); NULL unless minted
);

-- `key` below is the opaque GET /keys label from the request path,
-- percent-decoded — not the keyId that POST /keys returns.

CREATE TABLE IF NOT EXISTS key_signings (             -- POST /keys/{key}/sign
    id               INTEGER PRIMARY KEY,
    requested_at     TEXT    NOT NULL,
    source_ip        TEXT    NOT NULL,
    peer_addr        TEXT    NOT NULL,
    x_forwarded_for  TEXT,
    x_real_ip        TEXT,
    user_agent       TEXT,
    status           INTEGER NOT NULL,
    error            TEXT,
    duration_ms      INTEGER NOT NULL,
    key              TEXT    NOT NULL,
    client_data_hash TEXT,                            -- requested clientDataHash (base64), verbatim
    signature        TEXT                             -- response's DER ECDSA signature (base64)
);

CREATE TABLE IF NOT EXISTS key_deletions (            -- DELETE /keys/{key}
    id              INTEGER PRIMARY KEY,
    requested_at    TEXT    NOT NULL,
    source_ip       TEXT    NOT NULL,
    peer_addr       TEXT    NOT NULL,
    x_forwarded_for TEXT,
    x_real_ip       TEXT,
    user_agent      TEXT,
    status          INTEGER NOT NULL,
    error           TEXT,
    duration_ms     INTEGER NOT NULL,
    key             TEXT    NOT NULL,
    deleted         INTEGER                           -- response's deleted (0/1)
);

CREATE INDEX IF NOT EXISTS key_creations_bundle_id ON key_creations (bundle_id);
CREATE INDEX IF NOT EXISTS key_signings_key ON key_signings (key);
CREATE INDEX IF NOT EXISTS key_deletions_key ON key_deletions (key);

-- Every metered request as one timeline, tagged with its operationId.
CREATE VIEW IF NOT EXISTS requests AS
    SELECT 'getHealth' AS operation, id, requested_at, source_ip, peer_addr, x_forwarded_for,
           x_real_ip, user_agent, status, error, duration_ms FROM health_checks
    UNION ALL
    SELECT 'listKeys', id, requested_at, source_ip, peer_addr, x_forwarded_for,
           x_real_ip, user_agent, status, error, duration_ms FROM key_listings
    UNION ALL
    SELECT 'createKey', id, requested_at, source_ip, peer_addr, x_forwarded_for,
           x_real_ip, user_agent, status, error, duration_ms FROM key_creations
    UNION ALL
    SELECT 'signWithKey', id, requested_at, source_ip, peer_addr, x_forwarded_for,
           x_real_ip, user_agent, status, error, duration_ms FROM key_signings
    UNION ALL
    SELECT 'deleteKey', id, requested_at, source_ip, peer_addr, x_forwarded_for,
           x_real_ip, user_agent, status, error, duration_ms FROM key_deletions;
