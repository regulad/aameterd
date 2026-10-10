//! aaoracled's HTTP contract (`../aaoracled/openapi.yaml`), forwarded
//! verbatim to a real aaoracled and metered on the way back.

use std::borrow::Cow;
use std::error::Error;
use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use percent_encoding::percent_decode_str;
use serde_json::{Value, json};

use crate::meter::{Action, Meter, Outcome, RequestMeta};

#[derive(Clone)]
struct AppState {
    upstream: String,
    client: reqwest::Client,
    meter: Meter,
}

/// Builds the proxy in front of the aaoracled at `upstream` (a base URL such
/// as `http://127.0.0.1:8181`), giving up on it after `timeout`.
pub fn router(upstream: &str, timeout: Duration, meter: Meter) -> reqwest::Result<Router> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        // aaoracled is a direct target (normally an SSH tunnel on loopback);
        // never send it through an HTTP(S)_PROXY picked up from the environment.
        .no_proxy()
        .build()?;
    let state = AppState { upstream: upstream.trim_end_matches('/').to_owned(), client, meter };
    // No axum routes on purpose: dispatch goes through `Route::parse`, which
    // mirrors oracled.m's own router (e.g. wrong methods are 404s, not 405s).
    Ok(Router::new().fallback(handle).with_state(state))
}

/// One contract operation, named after its openapi.yaml operationId.
enum Route {
    GetHealth,
    ListKeys,
    CreateKey,
    /// The key exactly as it appeared in the path, still percent-encoded.
    SignWithKey(String),
    DeleteKey(String),
}

impl Route {
    /// Mirrors oracled.m's `route()`, including matching on the raw,
    /// still-encoded path: a `/` inside a key reaches aaoracled the same way
    /// whether or not the client encoded it as `%2F`.
    fn parse(method: &Method, path: &str) -> Option<Self> {
        match (method, path) {
            (&Method::GET, "/health") => return Some(Self::GetHealth),
            (&Method::GET, "/keys") => return Some(Self::ListKeys),
            (&Method::POST, "/keys") => return Some(Self::CreateKey),
            _ => {}
        }
        let rest = path.strip_prefix("/keys/")?;
        if *method == Method::POST && path.ends_with("/sign") {
            Some(Self::SignWithKey(rest.strip_suffix("/sign").unwrap_or_default().to_owned()))
        } else if *method == Method::DELETE {
            Some(Self::DeleteKey(rest.to_owned()))
        } else {
            None
        }
    }

    fn operation_id(&self) -> &'static str {
        match self {
            Self::GetHealth => "getHealth",
            Self::ListKeys => "listKeys",
            Self::CreateKey => "createKey",
            Self::SignWithKey(_) => "signWithKey",
            Self::DeleteKey(_) => "deleteKey",
        }
    }

    fn raw_key(&self) -> Option<&str> {
        match self {
            Self::SignWithKey(key) | Self::DeleteKey(key) => Some(key),
            _ => None,
        }
    }
}

async fn handle(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let meta = request_meta(&headers, peer);
    let Some(route) = Route::parse(&method, uri.path()) else {
        tracing::warn!(%method, %uri, source_ip = %meta.source_ip, "unrouted request, not forwarded");
        return json_response(StatusCode::NOT_FOUND, error_body("not_found", None));
    };

    let started = Instant::now();
    let (status, response_body) = if route.raw_key() == Some("") {
        // aaoracled answers this with the same 400 (or, for `/keys/sign`, an
        // NSRangeException), so don't bother it.
        let detail = "key required in path (from GET /keys)";
        (StatusCode::BAD_REQUEST, error_body("bad_request", Some(detail)))
    } else {
        match forward(&state, &method, uri.path(), &body).await {
            Ok(response) => response,
            Err(e) => {
                let code = if e.is_timeout() { "upstream_timeout" } else { "upstream_unreachable" };
                (StatusCode::BAD_GATEWAY, error_body(code, Some(&error_chain(&e))))
            }
        }
    };
    let duration_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);

    let request_json = serde_json::from_slice::<Value>(&body).ok();
    let response_json = serde_json::from_slice::<Value>(&response_body).ok();
    let outcome = Outcome {
        status: status.as_u16(),
        error: str_field(response_json.as_ref(), "error"),
        duration_ms,
    };
    let action = action(&route, request_json.as_ref(), response_json.as_ref());

    let operation = route.operation_id();
    let source_ip = meta.source_ip.clone();
    let meter = state.meter.clone();
    // Metering never fails the request; the client still gets aaoracled's answer.
    match tokio::task::spawn_blocking(move || meter.record(&meta, &outcome, &action)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::error!(error = %e, operation, "failed to meter request"),
        Err(e) => tracing::error!(error = %e, operation, "metering task panicked"),
    }
    tracing::info!(operation, status = status.as_u16(), %source_ip, duration_ms, "proxied");

    json_response(status, response_body)
}

/// Sends the request on to aaoracled with its path untouched and returns
/// aaoracled's status and body verbatim.
async fn forward(
    state: &AppState,
    method: &Method,
    path: &str,
    body: &Bytes,
) -> reqwest::Result<(StatusCode, Bytes)> {
    let mut request = state.client.request(method.clone(), format!("{}{path}", state.upstream));
    if *method == Method::POST {
        request = request.header(header::CONTENT_TYPE, "application/json").body(body.clone());
    }
    let response = request.send().await?;
    let status = response.status();
    Ok((status, response.bytes().await?))
}

/// Pulls the operation-specific metering fields out of the request and
/// response bodies. Every field is best-effort: a malformed request or an
/// error response just leaves the corresponding columns NULL.
fn action(route: &Route, request: Option<&Value>, response: Option<&Value>) -> Action {
    let req = |key: &str| str_field(request, key);
    let resp = |key: &str| str_field(response, key);
    match route {
        Route::GetHealth => Action::Health {
            app_attest_supported: response.and_then(|v| v.get("appAttestSupported")?.as_bool()),
        },
        Route::ListKeys => Action::ListKeys {
            key_count: response.and_then(|v| Some(v.get("keys")?.as_array()?.len() as i64)),
        },
        Route::CreateKey => {
            let app_id = req("appId");
            let (team_id, bundle_id) = match app_id.as_deref().and_then(|id| id.split_once('.')) {
                Some((team, bundle)) => (Some(team.to_owned()), Some(bundle.to_owned())),
                None => (None, None),
            };
            Action::CreateKey {
                app_id,
                team_id,
                bundle_id,
                environment: req("environment"),
                client_data_hash: req("clientDataHash"),
                key_id: resp("keyId"),
                attestation: resp("attestation"),
            }
        }
        Route::SignWithKey(raw) => Action::SignKey {
            key: decode_key(raw),
            client_data_hash: req("clientDataHash"),
            signature: resp("signature"),
        },
        Route::DeleteKey(raw) => Action::DeleteKey {
            key: decode_key(raw),
            deleted: response.and_then(|v| v.get("deleted")?.as_bool()),
        },
    }
}

fn request_meta(headers: &HeaderMap, peer: SocketAddr) -> RequestMeta {
    let peer = SocketAddr::new(peer.ip().to_canonical(), peer.port());
    let x_forwarded_for = header_values(headers, "x-forwarded-for");
    let x_real_ip = header_values(headers, "x-real-ip");
    // The rightmost X-Forwarded-For hop is the peer the nearest proxy saw (the
    // only entry a client can't forge through a proxy that appends);
    // X-Real-Ip is the fallback. Neither is verified, so the raw values and
    // the real TCP peer are recorded alongside.
    let source_ip = x_forwarded_for
        .as_deref()
        .and_then(|v| v.split(',').map(str::trim).rfind(|hop| !hop.is_empty()))
        .or_else(|| x_real_ip.as_deref().map(str::trim).filter(|ip| !ip.is_empty()))
        .map_or_else(|| peer.ip().to_string(), str::to_owned);
    RequestMeta {
        requested_at: humantime::format_rfc3339_millis(SystemTime::now()).to_string(),
        source_ip,
        peer_addr: peer.to_string(),
        x_forwarded_for,
        x_real_ip,
        user_agent: header_values(headers, "user-agent"),
    }
}

/// Every value sent for `name`, comma-joined in order (the same folding
/// proxies apply to repeated X-Forwarded-For headers).
fn header_values(headers: &HeaderMap, name: &str) -> Option<String> {
    let values: Vec<Cow<str>> =
        headers.get_all(name).iter().map(|v| String::from_utf8_lossy(v.as_bytes())).collect();
    (!values.is_empty()).then(|| values.join(", "))
}

/// Decodes a path key the way aaoracled does (`stringByRemovingPercentEncoding`:
/// `%XX` only, `+` stays `+`).
fn decode_key(raw: &str) -> String {
    percent_decode_str(raw).decode_utf8_lossy().into_owned()
}

fn str_field(value: Option<&Value>, key: &str) -> Option<String> {
    value?.get(key)?.as_str().map(str::to_owned)
}

/// The contract's `Error` schema.
fn error_body(code: &str, detail: Option<&str>) -> Bytes {
    let body = match detail {
        Some(detail) => json!({ "error": code, "detail": detail }),
        None => json!({ "error": code }),
    };
    body.to_string().into()
}

fn json_response(status: StatusCode, body: Bytes) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// `e` and all of its sources, since reqwest's top-level message alone
/// ("error sending request") hides why.
fn error_chain(e: &dyn Error) -> String {
    let mut message = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}
