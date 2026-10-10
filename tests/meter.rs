//! Schema migrations: a database left by an older aameterd must end up
//! exactly as shaped as a newly created one, with its rows intact.

use std::path::Path;

use aameterd::Meter;
use aameterd::meter::{Action, Outcome, RequestMeta};
use rusqlite::Connection;

/// `src/schema.sql` exactly as aameterd 0.1.0 shipped it (user_version 0).
const SCHEMA_V0: &str = include_str!("fixtures/schema-v0.sql");

/// Column (name, type, notnull, pk) per table/view.
type Columns = Vec<(String, String, bool, i64)>;

/// `user_version` plus every schema object and its columns.
fn shape(db: &Path) -> (i64, Vec<(String, String, Columns)>) {
    let conn = Connection::open(db).unwrap();
    let version = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
    let mut objects = conn.prepare("SELECT type, name FROM sqlite_master ORDER BY name").unwrap();
    let objects = objects
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .map(|(kind, name)| {
            let mut columns =
                conn.prepare("SELECT name, type, \"notnull\", pk FROM pragma_table_info(?1)").unwrap();
            let columns: Columns = columns
                .query_map([&name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            (kind, name, columns)
        })
        .collect();
    (version, objects)
}

#[test]
fn v0_database_is_migrated_to_match_a_new_one() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("old.sqlite3");
    let conn = Connection::open(&old).unwrap();
    conn.execute_batch(SCHEMA_V0).unwrap();
    conn.execute(
        "INSERT INTO health_checks (requested_at, source_ip, peer_addr, status, duration_ms, app_attest_supported)
         VALUES ('2026-10-09T00:00:00.000Z', '192.0.2.1', '192.0.2.1:1', 200, 1, 1)",
        [],
    )
    .unwrap();
    drop(conn);

    let meter = Meter::open(&old).unwrap();
    let meta = RequestMeta {
        requested_at: "2026-10-10T00:00:00.000Z".into(),
        source_ip: "192.0.2.2".into(),
        peer_addr: "192.0.2.2:2".into(),
        x_forwarded_for: None,
        x_real_ip: None,
        user_agent: None,
    };
    let outcome = Outcome { status: 200, error: None, duration_ms: 1 };
    let action = Action::Health {
        app_attest_supported: Some(true),
        supported_environments: Some(r#"["development","production"]"#.into()),
        environment_forgery: Some(true),
    };
    meter.record(&meta, &outcome, &action).unwrap();
    drop(meter);

    let new = dir.path().join("new.sqlite3");
    drop(Meter::open(&new).unwrap());
    let migrated = shape(&old);
    assert_eq!(migrated, shape(&new));
    assert_eq!(migrated.0, 1);

    let conn = Connection::open(&old).unwrap();
    let mut rows = conn
        .prepare("SELECT source_ip, app_attest_supported, supported_environments, environment_forgery FROM health_checks ORDER BY id")
        .unwrap();
    let rows: Vec<(String, bool, Option<String>, Option<bool>)> = rows
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        rows,
        [
            ("192.0.2.1".into(), true, None, None),
            ("192.0.2.2".into(), true, Some(r#"["development","production"]"#.into()), Some(true)),
        ]
    );

    // Reopening an up-to-date database changes nothing.
    drop(Meter::open(&old).unwrap());
    assert_eq!(shape(&old), migrated);
}

#[test]
fn newer_database_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("future.sqlite3");
    Connection::open(&db).unwrap().pragma_update(None, "user_version", 99).unwrap();

    let err = Meter::open(&db).err().expect("a database from a newer aameterd must not be opened");
    assert!(err.to_string().contains("newer"), "{err}");
}
