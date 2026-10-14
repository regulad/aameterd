//! SQLite metering store: one table per contract operation (`schema.sql`).

use std::ops::Bound;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;
use rusqlite::types::Value;

const SCHEMA: &str = include_str!("schema.sql");

/// Upgrades for databases created by older aameterd versions, in order;
/// `PRAGMA user_version` counts how many a database has had applied. A new
/// database gets the current `SCHEMA` directly and starts at the latest
/// version, so each entry must leave a table exactly as `SCHEMA` now
/// declares it.
const MIGRATIONS: &[&str] = &[
    // Contract 0.2.0: GET /health's supportedEnvironments and environmentForgery.
    "ALTER TABLE health_checks ADD COLUMN supported_environments TEXT;
     ALTER TABLE health_checks ADD COLUMN environment_forgery INTEGER;",
    // Contract 0.3.0: listing and deleting moved under /labels, and the
    // label-addressed tables follow, so `key` only ever means a keyId.
    // (key_attestations and key_assertions are new tables; SCHEMA adds them.)
    "DROP INDEX IF EXISTS key_deletions_key;
     DROP INDEX IF EXISTS key_signings_key;
     ALTER TABLE key_listings RENAME TO label_listings;
     ALTER TABLE label_listings RENAME COLUMN key_count TO label_count;
     ALTER TABLE key_deletions RENAME TO label_deletions;
     ALTER TABLE label_deletions RENAME COLUMN key TO label;
     ALTER TABLE key_signings RENAME TO label_signings;
     ALTER TABLE label_signings RENAME COLUMN key TO label;",
];

/// Who sent a request and when; recorded identically for every operation.
#[derive(Debug, Clone)]
pub struct RequestMeta {
    pub requested_at: String,
    pub source_ip: String,
    pub peer_addr: String,
    pub x_forwarded_for: Option<String>,
    pub x_real_ip: Option<String>,
    pub user_agent: Option<String>,
}

/// How a request went; recorded identically for every operation.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub status: u16,
    pub error: Option<String>,
    pub duration_ms: i64,
}

/// Operation-specific fields, one variant per table that is still written.
#[derive(Debug, Clone)]
pub enum Action {
    Health {
        app_attest_supported: Option<bool>,
        /// JSON array text, e.g. `["development","production"]`.
        supported_environments: Option<String>,
        environment_forgery: Option<bool>,
    },
    CreateKey {
        app_id: Option<String>,
        team_id: Option<String>,
        bundle_id: Option<String>,
        environment: Option<String>,
        client_data_hash: Option<String>,
        key_id: Option<String>,
        attestation: Option<String>,
    },
    AttestKey {
        key_id: String,
        app_id: Option<String>,
        team_id: Option<String>,
        bundle_id: Option<String>,
        client_data_hash: Option<String>,
        environment: Option<String>,
        attestation: Option<String>,
    },
    AssertKey {
        key_id: String,
        app_id: Option<String>,
        team_id: Option<String>,
        bundle_id: Option<String>,
        client_data_hash: Option<String>,
        environment: Option<String>,
        assertion: Option<String>,
    },
    ListLabels {
        label_count: Option<i64>,
    },
    DeleteLabel {
        label: String,
        deleted: Option<bool>,
    },
}

impl Action {
    /// The table this action is metered into, plus its own columns.
    fn row(&self) -> (&'static str, Vec<(&'static str, Value)>) {
        match self {
            Self::Health { app_attest_supported, supported_environments, environment_forgery } => (
                "health_checks",
                vec![
                    ("app_attest_supported", (*app_attest_supported).into()),
                    ("supported_environments", supported_environments.clone().into()),
                    ("environment_forgery", (*environment_forgery).into()),
                ],
            ),
            Self::CreateKey {
                app_id,
                team_id,
                bundle_id,
                environment,
                client_data_hash,
                key_id,
                attestation,
            } => (
                "key_creations",
                vec![
                    ("app_id", app_id.clone().into()),
                    ("team_id", team_id.clone().into()),
                    ("bundle_id", bundle_id.clone().into()),
                    ("environment", environment.clone().into()),
                    ("client_data_hash", client_data_hash.clone().into()),
                    ("key_id", key_id.clone().into()),
                    ("attestation", attestation.clone().into()),
                ],
            ),
            Self::AttestKey {
                key_id,
                app_id,
                team_id,
                bundle_id,
                client_data_hash,
                environment,
                attestation,
            } => (
                "key_attestations",
                vec![
                    ("key_id", key_id.clone().into()),
                    ("app_id", app_id.clone().into()),
                    ("team_id", team_id.clone().into()),
                    ("bundle_id", bundle_id.clone().into()),
                    ("client_data_hash", client_data_hash.clone().into()),
                    ("environment", environment.clone().into()),
                    ("attestation", attestation.clone().into()),
                ],
            ),
            Self::AssertKey {
                key_id,
                app_id,
                team_id,
                bundle_id,
                client_data_hash,
                environment,
                assertion,
            } => (
                "key_assertions",
                vec![
                    ("key_id", key_id.clone().into()),
                    ("app_id", app_id.clone().into()),
                    ("team_id", team_id.clone().into()),
                    ("bundle_id", bundle_id.clone().into()),
                    ("client_data_hash", client_data_hash.clone().into()),
                    ("environment", environment.clone().into()),
                    ("assertion", assertion.clone().into()),
                ],
            ),
            Self::ListLabels { label_count } => {
                ("label_listings", vec![("label_count", (*label_count).into())])
            }
            Self::DeleteLabel { label, deleted } => (
                "label_deletions",
                vec![("label", label.clone().into()), ("deleted", (*deleted).into())],
            ),
        }
    }
}

/// An operation whose calls aameterd's own API counts.
#[derive(Debug, Clone, Copy)]
pub enum Counted {
    AttestKey,
    AssertKey,
}

#[derive(Clone)]
pub struct Meter {
    conn: Arc<Mutex<Connection>>,
}

impl Meter {
    /// Opens (creating if needed) the database at `path` and brings its
    /// schema up to date.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let mut conn = Connection::open(path)?;
        // WAL so the database can be queried while aameterd keeps writing.
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        migrate(&mut conn)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Inserts one row for a request. Blocking — call off the async runtime.
    pub fn record(&self, meta: &RequestMeta, outcome: &Outcome, action: &Action) -> rusqlite::Result<()> {
        let (table, extra) = action.row();
        let columns: Vec<(&str, Value)> = [
            ("requested_at", meta.requested_at.clone().into()),
            ("source_ip", meta.source_ip.clone().into()),
            ("peer_addr", meta.peer_addr.clone().into()),
            ("x_forwarded_for", meta.x_forwarded_for.clone().into()),
            ("x_real_ip", meta.x_real_ip.clone().into()),
            ("user_agent", meta.user_agent.clone().into()),
            ("status", outcome.status.into()),
            ("error", outcome.error.clone().into()),
            ("duration_ms", outcome.duration_ms.into()),
        ]
        .into_iter()
        .chain(extra)
        .collect();

        let names: Vec<&str> = columns.iter().map(|(name, _)| *name).collect();
        let placeholders = vec!["?"; columns.len()].join(", ");
        let sql = format!("INSERT INTO {table} ({}) VALUES ({placeholders})", names.join(", "));

        let conn = self.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute(&sql, rusqlite::params_from_iter(columns.iter().map(|(_, value)| value)))?;
        Ok(())
    }

    /// Counts calls to `operation`, whatever their outcome, whose
    /// `requested_at` lies within `range`. The bounds are compared with
    /// `requested_at` as text, which orders instants only because
    /// `requested_at` is fixed-width, so give them in its exact layout.
    /// Blocking — call off the async runtime.
    pub fn count(&self, operation: Counted, range: (Bound<&str>, Bound<&str>)) -> rusqlite::Result<i64> {
        let table = match operation {
            Counted::AttestKey => "key_attestations",
            Counted::AssertKey => "key_assertions",
        };
        // A bound as SQL parameters: its value (NULL if unbounded) and
        // whether that value itself is in range.
        let params = |bound| match bound {
            Bound::Included(at) => (Some(at), true),
            Bound::Excluded(at) => (Some(at), false),
            Bound::Unbounded => (None, false),
        };
        let ((start, start_included), (end, end_included)) = (params(range.0), params(range.1));
        let conn = self.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            &format!(
                "SELECT count(*) FROM {table}
                 WHERE (?1 IS NULL OR requested_at > ?1 OR (?2 AND requested_at = ?1))
                   AND (?3 IS NULL OR requested_at < ?3 OR (?4 AND requested_at = ?3))"
            ),
            rusqlite::params![start, start_included, end, end_included],
            |row| row.get(0),
        )
    }
}

/// Applies whichever `MIGRATIONS` an existing database is missing, then
/// `SCHEMA` (creating everything for a new database), in one transaction.
fn migrate(conn: &mut Connection) -> anyhow::Result<()> {
    let tx = conn.transaction()?;
    // The view holds no data and CREATE VIEW IF NOT EXISTS would keep a stale
    // definition, so it's always rebuilt from SCHEMA. Dropping it first also
    // keeps it out of the way of table renames below.
    tx.execute_batch("DROP VIEW IF EXISTS requests;")?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let latest = MIGRATIONS.len();
    let Some(pending) = usize::try_from(version).ok().and_then(|v| MIGRATIONS.get(v..)) else {
        anyhow::bail!("database is at schema version {version}, newer than this aameterd's {latest}");
    };
    // Before user_version existed (0.1.0), every database was at version 0,
    // so a version-0 database that already has tables needs every migration.
    let existing: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'health_checks')",
        [],
        |row| row.get(0),
    )?;
    if existing {
        for migration in pending {
            tx.execute_batch(migration)?;
        }
    }
    tx.execute_batch(SCHEMA)?;
    tx.pragma_update(None, "user_version", latest as i64)?;
    tx.commit()?;
    Ok(())
}
