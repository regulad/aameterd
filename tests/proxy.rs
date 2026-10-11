//! End-to-end: a real aameterd in front of a mock aaoracled whose routing
//! copies oracled.m's `route()`, checked over HTTP and in the database.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aameterd::Meter;
use axum::Router;
use axum::body::Bytes;
use axum::http::{Method, StatusCode, Uri};
use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use rusqlite::Connection;
use serde_json::{Value, json};
use tokio::net::TcpListener;

/// The openapi.yaml example label: base64 `+`, `/`, `=` plus `:` separators.
const LABEL: &str = "aa:9Ur1MdQmPZZbOLudX+0T2lAbouGfiPZH1GQOIRyp88Y=d:4VI0nln7iw2mu3QIYzOuKkK9973ltBVMr6Rra6VrfSU=:eoYNTZIaNEumvCYOW4JCGkUhl7iv/6HpXUuvbGt+Tk0=:rk";
/// A keyId (standard base64) with every character that needs escaping.
const KEY_ID: &str = "AyiKfDNaC+7s9b9MvDiGHxeWvMfbNPuE/VvuBp5Hd9E=";
const APP_ID: &str = "ABCDE12345.com.example.target";
const CDH: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

/// `METHOD raw-path` for every request the mock aaoracled received.
type Seen = Arc<Mutex<Vec<String>>>;

struct Harness {
    proxy: String,
    db: PathBuf,
    seen: Seen,
    http: reqwest::Client,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn new() -> Self {
        let seen = Seen::default();
        let mock = Router::new().fallback({
            let seen = seen.clone();
            move |method: Method, uri: Uri, body: Bytes| mock_aaoracled(seen.clone(), method, uri, body)
        });
        let (listener, upstream) = listen().await;
        tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
        Self::with_upstream(&format!("http://{upstream}"), seen).await
    }

    async fn with_upstream(upstream: &str, seen: Seen) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("meter.sqlite3");
        let app = aameterd::router(upstream, Duration::from_secs(5), Meter::open(&db).unwrap()).unwrap();
        let (listener, proxy) = listen().await;
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap()
        });
        Self {
            proxy: format!("http://{proxy}"),
            db,
            seen,
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            _dir: dir,
        }
    }

    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.http.request(method, format!("{}{path}", self.proxy))
    }

    fn db(&self) -> Connection {
        Connection::open(&self.db).unwrap()
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

async fn listen() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (listener, addr)
}

/// Sends `request` (with a JSON body, if given) and returns status + JSON body.
async fn call(request: reqwest::RequestBuilder, body: Option<Value>) -> (u16, Value) {
    let request = match body {
        Some(body) => request.header("Content-Type", "application/json").body(body.to_string()),
        None => request,
    };
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    (status, serde_json::from_slice(&response.bytes().await.unwrap()).unwrap())
}

fn encode(id: &str) -> String {
    utf8_percent_encode(id, NON_ALPHANUMERIC).to_string()
}

/// Stand-in for aaoracled (contract 0.3.0) with the same dispatch as
/// oracled.m's `route()`: raw path, prefix/suffix matching, then
/// percent-decoding of the keyId or label.
async fn mock_aaoracled(seen: Seen, method: Method, uri: Uri, body: Bytes) -> (StatusCode, axum::Json<Value>) {
    let path = uri.path().to_owned();
    seen.lock().unwrap().push(format!("{method} {path}"));
    let body: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let decode = |raw: &str| percent_decode_str(raw).decode_utf8().unwrap().into_owned();
    // Request bodies are closed, as with oracled.m's unknownFields().
    let unknown = |allowed: &[&str]| {
        body.as_object().is_some_and(|fields| fields.keys().any(|k| !allowed.contains(&k.as_str())))
    };
    let app_id = body["appId"].as_str().unwrap_or_default();

    let (status, json) = if method == Method::GET && path == "/health" {
        (200, json!({
            "ok": true, "supportedEnvironments": ["development", "production"], "environmentForgery": true,
        }))
    } else if method == Method::POST && path == "/keys" {
        let environment = match body.get("environment") {
            None => Some("development"),
            Some(Value::String(e)) if e == "development" || e == "production" => Some(e.as_str()),
            Some(_) => None,
        };
        match environment {
            _ if unknown(&["appId", "environment"]) => (400, json!({ "error": "unknown_field" })),
            None => (400, json!({ "error": "bad_request", "detail": "environment must be \"development\" or \"production\"" })),
            Some(_) if app_id.starts_with("ZZZZZZZZZZ.") => {
                (502, json!({ "error": "generateKey_failed", "detail": "DCErrorInvalidKey" }))
            }
            Some(environment) => (201, json!({ "keyId": KEY_ID, "appId": app_id, "environment": environment })),
        }
    } else if method == Method::POST
        && path.starts_with("/keys/")
        && (path.ends_with("/attest") || path.ends_with("/assert"))
    {
        let key_id = decode(&path[6..path.len() - 7]);
        if unknown(&["appId", "clientDataHash"]) {
            (400, json!({ "error": "unknown_field" }))
        } else if key_id != KEY_ID {
            (404, json!({ "error": "unknown_key", "detail": "no credential on this device matches that keyId" }))
        } else if path.ends_with("/attest") {
            (200, json!({
                "keyId": key_id, "appId": app_id, "environment": "development",
                "attestation": "YXR0ZXN0YXRpb24=", "counter": 0,
            }))
        } else {
            (200, json!({ "keyId": key_id, "appId": app_id, "environment": "development", "assertion": "YXNzZXJ0aW9u" }))
        }
    } else if method == Method::GET && path == "/labels" {
        (200, json!({ "labels": [
            { "label": LABEL, "environment": "development" },
            { "label": "aa:other", "environment": null },
        ] }))
    } else if method == Method::DELETE && path.starts_with("/labels/") {
        let label = decode(&path[8..]);
        if label.starts_with("aa:") {
            (200, json!({ "label": label, "deleted": true }))
        } else {
            (400, json!({ "error": "bad_request", "detail": "not a credential label (must start \"aa:\")" }))
        }
    } else {
        (404, json!({ "error": "not_found" }))
    };
    (StatusCode::from_u16(status).unwrap(), axum::Json(json))
}

#[tokio::test]
async fn create_key_is_forwarded_and_metered() {
    let h = Harness::new().await;
    let request = h
        .request(Method::POST, "/keys")
        .header("X-Forwarded-For", "203.0.113.7, 10.0.0.1")
        .header("X-Real-Ip", "198.51.100.1")
        .header("User-Agent", "fake-devicecheckd/1");
    let (status, response) = call(request, Some(json!({ "appId": APP_ID, "environment": "production" }))).await;

    assert_eq!(status, 201);
    assert_eq!(response, json!({ "keyId": KEY_ID, "appId": APP_ID, "environment": "production" }));
    assert_eq!(h.seen(), ["POST /keys"]);

    let row = h.db().query_row(
        "SELECT requested_at, source_ip, peer_addr, x_forwarded_for, x_real_ip, user_agent, status, error,
                app_id, team_id, bundle_id, environment, key_id, client_data_hash, attestation
         FROM key_creations",
        [],
        |r| {
            Ok((
                [0, 1, 2, 3, 4, 5].map(|i| r.get::<_, String>(i).unwrap()),
                r.get::<_, i64>(6)?,
                r.get::<_, Option<String>>(7)?,
                [8, 9, 10, 11, 12].map(|i| r.get::<_, String>(i).unwrap()),
                [13, 14].map(|i| r.get::<_, Option<String>>(i).unwrap()),
            ))
        },
    );
    let ([requested_at, source_ip, peer_addr, xff, real_ip, user_agent], status, error, fields, absent) =
        row.unwrap();
    humantime::parse_rfc3339(&requested_at).unwrap();
    assert_eq!(source_ip, "10.0.0.1");
    assert!(peer_addr.starts_with("127.0.0.1:"), "{peer_addr}");
    assert_eq!(xff, "203.0.113.7, 10.0.0.1");
    assert_eq!(real_ip, "198.51.100.1");
    assert_eq!(user_agent, "fake-devicecheckd/1");
    assert_eq!((status, error), (201, None));
    assert_eq!(fields, [APP_ID, "ABCDE12345", "com.example.target", "production", KEY_ID]);
    assert_eq!(absent, [None, None], "0.3.0 createKey neither takes a clientDataHash nor attests");
}

#[tokio::test]
async fn rejected_create_key_is_relayed_and_metered() {
    let h = Harness::new().await;
    let request = h.request(Method::POST, "/keys").header("X-Real-Ip", "198.51.100.9");
    let (status, response) = call(request, Some(json!({ "appId": "ZZZZZZZZZZ.com.example.refused" }))).await;

    assert_eq!(status, 502);
    assert_eq!(response, json!({ "error": "generateKey_failed", "detail": "DCErrorInvalidKey" }));

    let row = h.db().query_row(
        "SELECT source_ip, x_forwarded_for, status, error, bundle_id, environment, key_id FROM key_creations",
        [],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        },
    );
    assert_eq!(
        row.unwrap(),
        ("198.51.100.9".into(), None, 502, "generateKey_failed".into(), "com.example.refused".into(), None, None)
    );
}

#[tokio::test]
async fn rejected_environment_is_relayed_and_metered() {
    let h = Harness::new().await;
    let body = json!({ "appId": APP_ID, "environment": "staging" });
    let (status, response) = call(h.request(Method::POST, "/keys"), Some(body)).await;
    assert_eq!(status, 400);
    assert_eq!(response["error"], "bad_request");

    let row = h.db().query_row("SELECT status, error, environment, key_id FROM key_creations", [], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?))
    });
    assert_eq!(row.unwrap(), (400, "bad_request".into(), "staging".into(), None));
}

#[tokio::test]
async fn stale_0_2_create_key_is_rejected_and_metered() {
    // A 0.2.0 client still sends a clientDataHash to POST /keys, expecting it
    // to attest too; 0.3.0 refuses unknown fields rather than ignoring them.
    let h = Harness::new().await;
    let body = json!({ "appId": APP_ID, "clientDataHash": CDH });
    let (status, response) = call(h.request(Method::POST, "/keys"), Some(body)).await;
    assert_eq!(status, 400);
    assert_eq!(response["error"], "unknown_field");

    let row = h.db().query_row("SELECT status, error, client_data_hash, key_id FROM key_creations", [], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?))
    });
    assert_eq!(row.unwrap(), (400, "unknown_field".into(), CDH.into(), None));
}

#[tokio::test]
async fn attest_and_assert_pass_the_raw_key_id_through() {
    let h = Harness::new().await;
    let encoded = encode(KEY_ID);
    let body = json!({ "appId": APP_ID, "clientDataHash": CDH });

    let attest_path = format!("/keys/{encoded}/attest");
    let request = h
        .request(Method::POST, &attest_path)
        .header("X-Forwarded-For", "192.0.2.1")
        .header("X-Forwarded-For", "192.0.2.2, 192.0.2.3,");
    let (status, response) = call(request, Some(body.clone())).await;
    assert_eq!(status, 200);
    assert_eq!(response["keyId"], KEY_ID);
    assert_eq!(response["attestation"], "YXR0ZXN0YXRpb24=");

    let assert_path = format!("/keys/{encoded}/assert");
    let (status, response) = call(h.request(Method::POST, &assert_path), Some(body.clone())).await;
    assert_eq!(status, 200);
    assert_eq!(response["assertion"], "YXNzZXJ0aW9u");

    let unknown_path = format!("/keys/{}/attest", encode("bm9wZQ=="));
    let (status, response) = call(h.request(Method::POST, &unknown_path), Some(body)).await;
    assert_eq!(status, 404);
    assert_eq!(response["error"], "unknown_key");

    assert_eq!(
        h.seen(),
        [format!("POST {attest_path}"), format!("POST {assert_path}"), format!("POST {unknown_path}")]
    );

    let db = h.db();
    let mut attestations = db
        .prepare(
            "SELECT source_ip, key_id, app_id, team_id, bundle_id, client_data_hash, environment, attestation, status, error
             FROM key_attestations ORDER BY id",
        )
        .unwrap();
    let attestations: Vec<_> = attestations
        .query_map([], |r| {
            Ok((
                [0, 1, 2, 3, 4, 5].map(|i| r.get::<_, String>(i).unwrap()),
                [6, 7].map(|i| r.get::<_, Option<String>>(i).unwrap()),
                r.get::<_, i64>(8)?,
                r.get::<_, Option<String>>(9)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let common = |source_ip: &str, key_id: &str| {
        [source_ip, key_id, APP_ID, "ABCDE12345", "com.example.target", CDH].map(str::to_owned)
    };
    assert_eq!(
        attestations,
        [
            // Last entry of the last X-Forwarded-For header.
            (common("192.0.2.3", KEY_ID), [Some("development".into()), Some("YXR0ZXN0YXRpb24=".into())], 200, None),
            // No forwarding headers: the TCP peer.
            (common("127.0.0.1", "bm9wZQ=="), [None, None], 404, Some("unknown_key".into())),
        ]
    );

    let assertion = db.query_row(
        "SELECT key_id, app_id, bundle_id, client_data_hash, environment, assertion FROM key_assertions",
        [],
        |r| Ok([0, 1, 2, 3, 4, 5].map(|i| r.get::<_, String>(i).unwrap())),
    );
    assert_eq!(assertion.unwrap(), [KEY_ID, APP_ID, "com.example.target", CDH, "development", "YXNzZXJ0aW9u"]);
}

#[tokio::test]
async fn list_and_delete_labels_pass_the_raw_label_through() {
    let h = Harness::new().await;
    let labels = call(h.request(Method::GET, "/labels"), None).await.1;
    assert_eq!(labels["labels"][0], json!({ "label": LABEL, "environment": "development" }));

    let encoded_path = format!("/labels/{}", encode(LABEL));
    let (status, response) = call(h.request(Method::DELETE, &encoded_path), None).await;
    assert_eq!(status, 200);
    assert_eq!(response, json!({ "label": LABEL, "deleted": true }));

    // Like aaoracled, an unencoded label (literal `/`, `+`, `=`) also works.
    let (status, _) = call(h.request(Method::DELETE, &format!("/labels/{LABEL}")), None).await;
    assert_eq!(status, 200);

    // A keyId is not a label; aaoracled says so instead of "deleting" it.
    let key_id_path = format!("/labels/{}", encode(KEY_ID));
    let (status, response) = call(h.request(Method::DELETE, &key_id_path), None).await;
    assert_eq!(status, 400);
    assert_eq!(response["error"], "bad_request");

    assert_eq!(
        h.seen(),
        [
            "GET /labels".to_owned(),
            format!("DELETE {encoded_path}"),
            format!("DELETE /labels/{LABEL}"),
            format!("DELETE {key_id_path}"),
        ]
    );

    let db = h.db();
    let label_count: i64 = db.query_row("SELECT label_count FROM label_listings", [], |r| r.get(0)).unwrap();
    assert_eq!(label_count, 2);
    let mut deletions = db.prepare("SELECT label, deleted, status FROM label_deletions ORDER BY id").unwrap();
    let deletions: Vec<(String, Option<bool>, i64)> = deletions
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        deletions,
        [(LABEL.into(), Some(true), 200), (LABEL.into(), Some(true), 200), (KEY_ID.into(), None, 400)]
    );
}

#[tokio::test]
async fn health_is_metered_and_every_operation_lands_in_the_timeline() {
    let h = Harness::new().await;
    assert_eq!(call(h.request(Method::GET, "/health"), None).await.1["environmentForgery"], true);

    let health = h.db().query_row(
        "SELECT app_attest_supported, supported_environments, environment_forgery FROM health_checks",
        [],
        |r| Ok((r.get::<_, Option<bool>>(0)?, r.get::<_, String>(1)?, r.get::<_, bool>(2)?)),
    );
    assert_eq!(health.unwrap(), (None, r#"["development","production"]"#.into(), true));

    let body = json!({ "appId": APP_ID, "clientDataHash": CDH });
    call(h.request(Method::POST, "/keys"), Some(json!({ "appId": APP_ID }))).await;
    call(h.request(Method::POST, &format!("/keys/{}/attest", encode(KEY_ID))), Some(body.clone())).await;
    call(h.request(Method::POST, &format!("/keys/{}/assert", encode(KEY_ID))), Some(body)).await;
    call(h.request(Method::GET, "/labels"), None).await;
    call(h.request(Method::DELETE, &format!("/labels/{}", encode(LABEL))), None).await;

    let db = h.db();
    let mut timeline = db.prepare("SELECT operation, status FROM requests ORDER BY operation").unwrap();
    let timeline: Vec<(String, i64)> =
        timeline.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect();
    let expected = [
        ("assertKey", 200),
        ("attestKey", 200),
        ("createKey", 201),
        ("deleteLabel", 200),
        ("getHealth", 200),
        ("listLabels", 200),
    ];
    assert_eq!(timeline, expected.map(|(operation, status)| (operation.to_owned(), status)));
}

#[tokio::test]
async fn unreachable_upstream_is_a_metered_502() {
    // Grab a free port, then close it so nothing is listening there.
    let (listener, dead) = listen().await;
    drop(listener);
    let h = Harness::with_upstream(&format!("http://{dead}"), Seen::default()).await;

    let (status, response) = call(h.request(Method::GET, "/labels"), None).await;
    assert_eq!(status, 502);
    assert_eq!(response["error"], "upstream_unreachable");

    let row = h.db().query_row("SELECT status, error, label_count FROM label_listings", [], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?))
    });
    assert_eq!(row.unwrap(), (502, "upstream_unreachable".into(), None));
}

#[tokio::test]
async fn off_contract_and_identifierless_requests_never_reach_upstream() {
    let h = Harness::new().await;
    let off_contract = [
        (Method::GET, "/nope"),
        // Routes contract 0.3.0 removed.
        (Method::GET, "/keys"),
        (Method::DELETE, "/keys/x"),
        (Method::POST, "/keys/x/sign"),
        // Real routes, wrong shape or method.
        (Method::POST, "/keys/x"),
        (Method::GET, "/keys/x/attest"),
        (Method::PUT, "/labels"),
        (Method::GET, "/labels/x"),
        (Method::POST, "/labels/x"),
    ];
    for (method, path) in off_contract {
        assert_eq!(call(h.request(method, path), None).await, (404, json!({ "error": "not_found" })), "{path}");
    }
    let metered: i64 = h.db().query_row("SELECT count(*) FROM requests", [], |r| r.get(0)).unwrap();
    assert_eq!(metered, 0);

    let body = json!({ "appId": APP_ID, "clientDataHash": CDH });
    for (method, path, table, detail) in [
        (Method::POST, "/keys/attest", "key_attestations", "keyId required in path (from POST /keys)"),
        (Method::POST, "/keys//assert", "key_assertions", "keyId required in path (from POST /keys)"),
        (Method::DELETE, "/labels/", "label_deletions", "label required in path"),
    ] {
        let (status, response) = call(h.request(method, path), Some(body.clone())).await;
        assert_eq!((status, response), (400, json!({ "error": "bad_request", "detail": detail })), "{path}");
        let column = if table == "label_deletions" { "label" } else { "key_id" };
        let row = h.db().query_row(&format!("SELECT {column}, status FROM {table}"), [], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        });
        assert_eq!(row.unwrap(), (String::new(), 400), "{path}");
    }

    assert!(h.seen().is_empty(), "{:?}", h.seen());
}
