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

/// The openapi.yaml example key: base64 `+`, `/`, `=` plus `:` separators.
const KEY: &str = "aa:9Ur1MdQmPZZbOLudX+0T2lAbouGfiPZH1GQOIRyp88Y=d:4VI0nln7iw2mu3QIYzOuKkK9973ltBVMr6Rra6VrfSU=:eoYNTZIaNEumvCYOW4JCGkUhl7iv/6HpXUuvbGt+Tk0=:rk";
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

/// Stand-in for aaoracled with the same dispatch as oracled.m's `route()`:
/// raw path, prefix/suffix matching, then percent-decoding of the key.
async fn mock_aaoracled(seen: Seen, method: Method, uri: Uri, body: Bytes) -> (StatusCode, axum::Json<Value>) {
    let path = uri.path().to_owned();
    seen.lock().unwrap().push(format!("{method} {path}"));
    let body: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let decode = |raw: &str| percent_decode_str(raw).decode_utf8().unwrap().into_owned();

    let (status, json) = if method == Method::GET && path == "/health" {
        (200, json!({ "ok": true, "appAttestSupported": true, "defaultEnvironment": "development" }))
    } else if method == Method::GET && path == "/keys" {
        (200, json!({ "keys": [KEY, "aa:other"] }))
    } else if method == Method::POST && path == "/keys" {
        let app_id = body["appId"].as_str().unwrap_or_default();
        if app_id.starts_with("ZZZZZZZZZZ.") {
            (502, json!({ "error": "attestKey_failed", "detail": "DCErrorInvalidKey" }))
        } else {
            let environment = body["environment"].as_str().unwrap_or("development");
            (201, json!({
                "keyId": "a2V5aWQ=", "appId": app_id, "environment": environment,
                "attestation": "YXR0ZXN0YXRpb24=", "counter": 0,
            }))
        }
    } else if method == Method::POST && path.starts_with("/keys/") && path.ends_with("/sign") {
        (200, json!({ "key": decode(&path[6..path.len() - 5]), "signature": "c2lnbmF0dXJl" }))
    } else if method == Method::DELETE && path.starts_with("/keys/") {
        (200, json!({ "key": decode(&path[6..]), "deleted": true }))
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
    let body = json!({ "appId": "ABCDE12345.com.example.target", "environment": "production", "clientDataHash": CDH });
    let (status, response) = call(request, Some(body)).await;

    assert_eq!(status, 201);
    assert_eq!(response["keyId"], "a2V5aWQ=");
    assert_eq!(response["attestation"], "YXR0ZXN0YXRpb24=");
    assert_eq!(h.seen(), ["POST /keys"]);

    let row = h.db().query_row(
        "SELECT requested_at, source_ip, peer_addr, x_forwarded_for, x_real_ip, user_agent, status, error,
                app_id, team_id, bundle_id, environment, client_data_hash, key_id, attestation
         FROM key_creations",
        [],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, Option<String>>(7)?,
                [8, 9, 10, 11, 12, 13, 14].map(|i| r.get::<_, String>(i).unwrap()),
            ))
        },
    );
    let (requested_at, source_ip, peer_addr, xff, real_ip, user_agent, status, error, fields) = row.unwrap();
    humantime::parse_rfc3339(&requested_at).unwrap();
    assert_eq!(source_ip, "10.0.0.1");
    assert!(peer_addr.starts_with("127.0.0.1:"), "{peer_addr}");
    assert_eq!(xff, "203.0.113.7, 10.0.0.1");
    assert_eq!(real_ip, "198.51.100.1");
    assert_eq!(user_agent, "fake-devicecheckd/1");
    assert_eq!(status, 201);
    assert_eq!(error, None);
    assert_eq!(
        fields,
        [
            "ABCDE12345.com.example.target",
            "ABCDE12345",
            "com.example.target",
            "production",
            CDH,
            "a2V5aWQ=",
            "YXR0ZXN0YXRpb24=",
        ]
    );
}

#[tokio::test]
async fn rejected_create_key_is_relayed_and_metered() {
    let h = Harness::new().await;
    let request = h.request(Method::POST, "/keys").header("X-Real-Ip", "198.51.100.9");
    let body = json!({ "appId": "ZZZZZZZZZZ.com.example.refused", "clientDataHash": CDH });
    let (status, response) = call(request, Some(body)).await;

    assert_eq!(status, 502);
    assert_eq!(response, json!({ "error": "attestKey_failed", "detail": "DCErrorInvalidKey" }));

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
        ("198.51.100.9".into(), None, 502, "attestKey_failed".into(), "com.example.refused".into(), None, None)
    );
}

#[tokio::test]
async fn sign_and_delete_pass_the_raw_key_through() {
    let h = Harness::new().await;
    let encoded = utf8_percent_encode(KEY, NON_ALPHANUMERIC).to_string();

    let sign_path = format!("/keys/{encoded}/sign");
    let request = h
        .request(Method::POST, &sign_path)
        .header("X-Forwarded-For", "192.0.2.1")
        .header("X-Forwarded-For", "192.0.2.2, 192.0.2.3,");
    let (status, response) = call(request, Some(json!({ "clientDataHash": CDH }))).await;
    assert_eq!(status, 200);
    assert_eq!(response["key"], KEY);

    // Like aaoracled, an unencoded key (literal `/`, `+`, `=`) also works.
    let (status, response) = call(h.request(Method::DELETE, &format!("/keys/{KEY}")), None).await;
    assert_eq!(status, 200);
    assert_eq!(response, json!({ "key": KEY, "deleted": true }));

    assert_eq!(h.seen(), [format!("POST {sign_path}"), format!("DELETE /keys/{KEY}")]);

    let db = h.db();
    let signing = db.query_row("SELECT source_ip, key, client_data_hash, signature FROM key_signings", [], |r| {
        Ok([0, 1, 2, 3].map(|i| r.get::<_, String>(i).unwrap()))
    });
    // Last entry of the last X-Forwarded-For header.
    assert_eq!(signing.unwrap(), ["192.0.2.3", KEY, CDH, "c2lnbmF0dXJl"]);
    let deletion = db.query_row("SELECT source_ip, key, deleted FROM key_deletions", [], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, bool>(2)?))
    });
    // No forwarding headers: the TCP peer.
    assert_eq!(deletion.unwrap(), ("127.0.0.1".into(), KEY.into(), true));
}

#[tokio::test]
async fn health_and_listing_are_metered_into_the_timeline() {
    let h = Harness::new().await;
    assert_eq!(call(h.request(Method::GET, "/health"), None).await.1["appAttestSupported"], true);
    assert_eq!(call(h.request(Method::GET, "/keys"), None).await.1["keys"][0], KEY);

    let db = h.db();
    let supported: bool = db.query_row("SELECT app_attest_supported FROM health_checks", [], |r| r.get(0)).unwrap();
    assert!(supported);
    let key_count: i64 = db.query_row("SELECT key_count FROM key_listings", [], |r| r.get(0)).unwrap();
    assert_eq!(key_count, 2);

    let mut timeline = db.prepare("SELECT operation, status FROM requests ORDER BY operation").unwrap();
    let timeline: Vec<(String, i64)> =
        timeline.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect();
    assert_eq!(timeline, [("getHealth".into(), 200), ("listKeys".into(), 200)]);
}

#[tokio::test]
async fn unreachable_upstream_is_a_metered_502() {
    // Grab a free port, then close it so nothing is listening there.
    let (listener, dead) = listen().await;
    drop(listener);
    let h = Harness::with_upstream(&format!("http://{dead}"), Seen::default()).await;

    let (status, response) = call(h.request(Method::GET, "/keys"), None).await;
    assert_eq!(status, 502);
    assert_eq!(response["error"], "upstream_unreachable");

    let row = h.db().query_row("SELECT status, error, key_count FROM key_listings", [], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?))
    });
    assert_eq!(row.unwrap(), (502, "upstream_unreachable".into(), None));
}

#[tokio::test]
async fn off_contract_and_keyless_requests_never_reach_upstream() {
    let h = Harness::new().await;
    for (method, path) in [(Method::GET, "/nope"), (Method::GET, "/keys/x"), (Method::PUT, "/keys"), (Method::POST, "/keys/x")] {
        assert_eq!(call(h.request(method, path), None).await, (404, json!({ "error": "not_found" })), "{path}");
    }
    let metered: i64 = h.db().query_row("SELECT count(*) FROM requests", [], |r| r.get(0)).unwrap();
    assert_eq!(metered, 0);

    let (status, response) = call(h.request(Method::POST, "/keys//sign"), Some(json!({ "clientDataHash": CDH }))).await;
    assert_eq!(status, 400);
    assert_eq!(response["error"], "bad_request");
    let row = h.db().query_row("SELECT key, status FROM key_signings", [], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    });
    assert_eq!(row.unwrap(), (String::new(), 400));

    assert!(h.seen().is_empty(), "{:?}", h.seen());
}
