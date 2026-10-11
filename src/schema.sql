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
--
-- Credentials are addressed in two namespaces that must not be mixed up
-- (contract 0.3.0 splits them into /keys and /labels): `key_id` is the keyId
-- POST /keys returns, `label` is an opaque keychain label from GET /labels.
-- Both are stored percent-decoded.
--
-- Changing a table that already exists also takes an entry in meter.rs's
-- MIGRATIONS: CREATE TABLE IF NOT EXISTS leaves existing tables alone. New
-- tables and indexes need none, and the `requests` view is rebuilt from here
-- on every start.

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
    app_attest_supported   INTEGER,                   -- response's appAttestSupported (0/1); gone in contract 0.3.0
    supported_environments TEXT,                      -- response's supportedEnvironments, as a JSON array
    environment_forgery    INTEGER                    -- response's environmentForgery (0/1)
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
    client_data_hash TEXT,                            -- requested clientDataHash, if sent; contract 0.3.0 rejects it
    key_id           TEXT,                            -- response's keyId (base64); NULL unless minted
    attestation      TEXT                             -- response's attestation; contract 0.1.0-0.2.0 only
);

CREATE TABLE IF NOT EXISTS key_attestations (         -- POST /keys/{keyId}/attest
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
    key_id           TEXT    NOT NULL,                -- keyId from the request path
    app_id           TEXT,                            -- requested appId, verbatim
    team_id          TEXT,                            -- app_id before its first '.'
    bundle_id        TEXT,                            -- app_id after its first '.'
    client_data_hash TEXT,                            -- requested clientDataHash (base64), verbatim
    environment      TEXT,                            -- response's environment, read off the key's label by aaoracled
    attestation      TEXT                             -- response's attestation (base64 CBOR)
);

CREATE TABLE IF NOT EXISTS key_assertions (           -- POST /keys/{keyId}/assert
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
    key_id           TEXT    NOT NULL,                -- keyId from the request path
    app_id           TEXT,                            -- requested appId, verbatim
    team_id          TEXT,                            -- app_id before its first '.'
    bundle_id        TEXT,                            -- app_id after its first '.'
    client_data_hash TEXT,                            -- requested clientDataHash (base64), verbatim
    environment      TEXT,                            -- response's environment, read off the key's label by aaoracled
    assertion        TEXT                             -- response's assertion (base64 CBOR: signature, authenticatorData)
);

CREATE TABLE IF NOT EXISTS label_listings (           -- GET /labels (GET /keys before contract 0.3.0)
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
    label_count     INTEGER                           -- number of labels in the response
);

CREATE TABLE IF NOT EXISTS label_deletions (          -- DELETE /labels/{label} (DELETE /keys/{key} before 0.3.0)
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
    label           TEXT    NOT NULL,                 -- label from the request path
    deleted         INTEGER                           -- response's deleted (0/1)
);

-- Retired: contract 0.3.0 removed POST /keys/{key}/sign (assertKey replaces
-- it). Kept for the rows older databases already hold; nothing writes it.
CREATE TABLE IF NOT EXISTS label_signings (           -- POST /keys/{key}/sign, contract 0.1.0-0.2.0
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
    label            TEXT    NOT NULL,                -- label from the request path
    client_data_hash TEXT,                            -- requested clientDataHash (base64), verbatim
    signature        TEXT                             -- response's DER ECDSA signature (base64)
);

CREATE INDEX IF NOT EXISTS key_creations_bundle_id ON key_creations (bundle_id);
CREATE INDEX IF NOT EXISTS key_creations_key_id ON key_creations (key_id);
CREATE INDEX IF NOT EXISTS key_attestations_key_id ON key_attestations (key_id);
CREATE INDEX IF NOT EXISTS key_assertions_key_id ON key_assertions (key_id);
CREATE INDEX IF NOT EXISTS label_deletions_label ON label_deletions (label);
CREATE INDEX IF NOT EXISTS label_signings_label ON label_signings (label);

-- Every metered request as one timeline, tagged with its operationId.
-- label_listings and label_deletions rows from before contract 0.3.0 were
-- listKeys/deleteKey; they're tagged with their 0.3.0 successors here.
CREATE VIEW IF NOT EXISTS requests AS
    SELECT 'getHealth' AS operation, id, requested_at, source_ip, peer_addr, x_forwarded_for,
           x_real_ip, user_agent, status, error, duration_ms FROM health_checks
    UNION ALL
    SELECT 'createKey', id, requested_at, source_ip, peer_addr, x_forwarded_for,
           x_real_ip, user_agent, status, error, duration_ms FROM key_creations
    UNION ALL
    SELECT 'attestKey', id, requested_at, source_ip, peer_addr, x_forwarded_for,
           x_real_ip, user_agent, status, error, duration_ms FROM key_attestations
    UNION ALL
    SELECT 'assertKey', id, requested_at, source_ip, peer_addr, x_forwarded_for,
           x_real_ip, user_agent, status, error, duration_ms FROM key_assertions
    UNION ALL
    SELECT 'listLabels', id, requested_at, source_ip, peer_addr, x_forwarded_for,
           x_real_ip, user_agent, status, error, duration_ms FROM label_listings
    UNION ALL
    SELECT 'deleteLabel', id, requested_at, source_ip, peer_addr, x_forwarded_for,
           x_real_ip, user_agent, status, error, duration_ms FROM label_deletions
    UNION ALL
    SELECT 'signWithKey', id, requested_at, source_ip, peer_addr, x_forwarded_for,
           x_real_ip, user_agent, status, error, duration_ms FROM label_signings;
