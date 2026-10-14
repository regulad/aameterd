//! aameterd's own contract (`openapi.yaml`), checked over HTTP against a
//! database with known rows in it.

use std::net::SocketAddr;
use std::time::{Duration, SystemTime};

use aameterd::Meter;
use aameterd::meter::{Action, Outcome, RequestMeta};
use serde_json::{Value, json};
use tokio::net::TcpListener;

struct Harness {
    meter: Meter,
    stats: String,
    http: reqwest::Client,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let meter = Meter::open(&dir.path().join("meter.sqlite3")).unwrap();
        let app = aameterd::stats_router(meter.clone());
        let (listener, stats) = listen().await;
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            meter,
            stats: format!("http://{stats}"),
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            _dir: dir,
        }
    }

    /// Records one request of `action` received at `requested_at`.
    fn record(&self, requested_at: &str, status: u16, action: Action) {
        let meta = RequestMeta {
            requested_at: requested_at.into(),
            source_ip: "192.0.2.1".into(),
            peer_addr: "192.0.2.1:1".into(),
            x_forwarded_for: None,
            x_real_ip: None,
            user_agent: None,
        };
        let outcome = Outcome { status, error: None, duration_ms: 1 };
        self.meter.record(&meta, &outcome, &action).unwrap();
    }

    /// `GET {path_and_query}`: status, Content-Type, and raw body.
    async fn get(&self, path_and_query: &str) -> (u16, String, String) {
        let response = self.http.get(format!("{}{path_and_query}", self.stats)).send().await.unwrap();
        let content_type = response.headers()["content-type"].to_str().unwrap().to_owned();
        (response.status().as_u16(), content_type, response.text().await.unwrap())
    }

    /// The count for `from`/`until`, given raw (already query-encoded).
    async fn count(&self, from: &str, until: &str) -> i64 {
        let (status, _, body) = self.get(&format!("/attestations?from={from}&until={until}")).await;
        assert_eq!(status, 200, "{from} {until}: {body}");
        serde_json::from_str(&body).unwrap()
    }

    /// Status and JSON body of a request expected to fail.
    async fn error(&self, path_and_query: &str) -> (u16, Value) {
        let (status, _, body) = self.get(path_and_query).await;
        (status, serde_json::from_str(&body).unwrap())
    }
}

async fn listen() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (listener, addr)
}

fn attest() -> Action {
    Action::AttestKey {
        key_id: "a2V5".into(),
        app_id: None,
        team_id: None,
        bundle_id: None,
        client_data_hash: None,
        environment: None,
        attestation: None,
    }
}

#[tokio::test]
async fn counts_attestations_received_in_the_window() {
    let h = Harness::new().await;
    for at in ["2026-10-11T19:07:53.845Z", "2026-10-11T19:07:53.846Z", "2026-10-11T19:07:53.847Z"] {
        h.record(at, 200, attest());
    }
    // A failed call still counts; other operations never do.
    h.record("2026-10-11T23:59:59.999Z", 502, attest());
    h.record("2026-10-11T19:07:53.846Z", 201, Action::CreateKey {
        app_id: None,
        team_id: None,
        bundle_id: None,
        environment: None,
        client_data_hash: None,
        key_id: None,
        attestation: Some("YXR0ZXN0YXRpb24=".into()),
    });
    h.record("2026-10-11T19:07:53.846Z", 200, Action::ListLabels { label_count: Some(1) });

    let (status, content_type, body) =
        h.get("/attestations?from=2026-10-11T00:00:00Z&until=2026-10-12T00:00:00Z").await;
    assert_eq!((status, content_type.as_str(), body.as_str()), (200, "application/json", "4"), "a bare integer");

    let cases = [
        // `from` is inclusive, `until` exclusive.
        ("2026-10-11T19:07:53.846Z", "2026-10-11T19:07:53.847Z", 1),
        ("2026-10-11T19:07:53.846000000Z", "2026-10-11T19:07:53.8470Z", 1),
        ("2026-10-11T19:07:53Z", "2026-10-11T19:07:54Z", 3),
        // Bounds finer than requested_at's milliseconds are compared as
        // instants: .846 is before .846258, though as text "...846Z" sorts
        // after "...846258Z".
        ("2026-10-11T19:07:53.846258Z", "2026-10-11T19:07:54Z", 1),
        ("2026-10-11T19:07:53.845999Z", "2026-10-11T19:07:53.846001Z", 1),
        ("2026-10-11T19:07:53.845001Z", "2026-10-11T19:07:53.846999999Z", 1),
        ("2026-10-11T23:59:59.9995Z", "2026-10-12T00:00:00Z", 0),
        ("2026-10-11T23:59:59.999Z", "2026-10-11T23:59:59.9991Z", 1),
        // An empty window, and windows around no calls at all.
        ("2026-10-11T19:07:53.846Z", "2026-10-11T19:07:53.846Z", 0),
        ("0000-01-01T00:00:00Z", "2026-10-11T19:07:53.845Z", 0),
        ("2026-10-12T00:00:00Z", "9999-12-31T23:59:59.999999999Z", 0),
        ("0000-01-01T00:00:00Z", "9999-12-31T23:59:59Z", 4),
    ];
    for (from, until, expected) in cases {
        assert_eq!(h.count(from, until).await, expected, "{from} {until}");
    }

    // Percent-encoding is decoded first.
    assert_eq!(h.count("2026-10-11T19%3A07%3A53Z", "2026-10-11T19%3A07%3A54Z").await, 3);
}

#[tokio::test]
async fn a_proxied_attest_is_counted() {
    // An unreachable aaoracled still meters the call (as a 502), and the
    // proxy and this server share one database.
    let h = Harness::new().await;
    let (listener, dead) = listen().await;
    drop(listener);
    let proxy = aameterd::router(&format!("http://{dead}"), Duration::from_secs(5), h.meter.clone()).unwrap();
    let (listener, proxy_addr) = listen().await;
    tokio::spawn(async move {
        axum::serve(listener, proxy.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap()
    });

    // Truncated like requested_at, or a call in the same millisecond as
    // `from` would rightly fall before it.
    let from = humantime::format_rfc3339_millis(SystemTime::now()).to_string();
    let response = h.http.post(format!("http://{proxy_addr}/keys/a2V5/attest")).body("{}").send().await.unwrap();
    assert_eq!(response.status(), 502);
    let until = humantime::format_rfc3339_micros(SystemTime::now() + Duration::from_millis(1)).to_string();

    assert_eq!(h.count(&from, &until).await, 1);
}

#[tokio::test]
async fn malformed_queries_are_rejected() {
    let h = Harness::new().await;
    let bad_request = |detail: &str| (400, json!({ "error": "bad_request", "detail": detail }));
    let window = "from=2026-10-11T19:07:53Z&until=2026-10-11T19:07:54Z";

    assert_eq!(h.error("/attestations").await, bad_request("from is required"));
    assert_eq!(h.error("/attestations?from=2026-10-11T19:07:53Z").await, bad_request("until is required"));
    assert_eq!(
        h.error(&format!("/attestations?{window}&from=2026-10-11T19:07:52Z")).await,
        bad_request("from must be sent only once")
    );
    assert_eq!(
        h.error("/attestations?from=2026-10-11T19:07:54Z&until=2026-10-11T19:07:53.999999Z").await,
        bad_request("from is after until")
    );
    assert_eq!(
        h.error(&format!("/attestations?{window}&appId=x&limit=1&appId=y")).await,
        (400, json!({ "error": "unknown_parameter", "detail": "GET /attestations does not accept: appId, limit" }))
    );

    let not_timestamps = [
        ("2026-10-11T19:07:53%2B00:00", "2026-10-11T19:07:53+00:00"),
        // A raw `+` is a space once decoded.
        ("2026-10-11T19:07:53+00:00", "2026-10-11T19:07:53 00:00"),
        ("2026-10-11T19:07:53", "2026-10-11T19:07:53"),
        ("2026-10-11T19:07:53z", "2026-10-11T19:07:53z"),
        ("2026-10-11t19:07:53Z", "2026-10-11t19:07:53Z"),
        ("2026-10-11 19:07:53Z", "2026-10-11 19:07:53Z"),
        ("2026-10-11T19:07:53.Z", "2026-10-11T19:07:53.Z"),
        ("2026-10-11T19:07:53.1234567890Z", "2026-10-11T19:07:53.1234567890Z"),
        ("2026-10-11T19:07:53.12a4Z", "2026-10-11T19:07:53.12a4Z"),
        ("2026-10-11T19:07:5Z", "2026-10-11T19:07:5Z"),
        ("2026-10-11T19:07:-5Z", "2026-10-11T19:07:-5Z"),
        ("2026-02-29T00:00:00Z", "2026-02-29T00:00:00Z"),
        ("2026-04-31T00:00:00Z", "2026-04-31T00:00:00Z"),
        ("2026-13-01T00:00:00Z", "2026-13-01T00:00:00Z"),
        ("2026-10-00T00:00:00Z", "2026-10-00T00:00:00Z"),
        ("2026-10-11T24:00:00Z", "2026-10-11T24:00:00Z"),
        ("2026-10-11T19:60:00Z", "2026-10-11T19:60:00Z"),
        ("2026-10-11T19:07:60Z", "2026-10-11T19:07:60Z"),
        ("1776416400", "1776416400"),
        ("", ""),
    ];
    for (sent, decoded) in not_timestamps {
        let detail = format!("from must be a UTC timestamp like 2026-10-11T19:07:53.846258Z, not {decoded:?}");
        assert_eq!(h.error(&format!("/attestations?from={sent}&until=x")).await, bad_request(&detail), "{sent}");
    }

    // Real leap days are fine.
    assert_eq!(h.count("2000-02-29T00:00:00Z", "2024-02-29T00:00:00Z").await, 0);
}

#[tokio::test]
async fn other_paths_are_not_found() {
    let h = Harness::new().await;
    for path in ["/", "/attestations/", "/health", "/keys"] {
        assert_eq!(h.error(path).await, (404, json!({ "error": "not_found" })), "{path}");
    }
}
