//! aameterd's own contract (`openapi.yaml`): live counts read from the
//! metering database, served on a listener separate from the proxy's.

use std::collections::BTreeSet;
use std::ops::Bound;

use axum::Router;
use axum::body::Bytes;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;

use crate::meter::Meter;
use crate::proxy::{error_body, json_response};

/// Builds the server for aameterd's own contract over `meter`'s database.
/// It only reads; nothing here reaches aaoracled.
pub fn router(meter: Meter) -> Router {
    Router::new()
        .route("/attestations", get(count_attestations))
        .fallback(|| async { json_response(StatusCode::NOT_FOUND, error_body("not_found", None)) })
        .with_state(meter)
}

/// `GET /attestations` (`countAttestations`).
async fn count_attestations(
    State(meter): State<Meter>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Response {
    let window = match query {
        Ok(Query(params)) => window(params),
        Err(rejection) => Err(bad_request(&rejection.body_text())),
    };
    let (from, until) = match window {
        Ok(window) => window,
        Err(body) => return json_response(StatusCode::BAD_REQUEST, body),
    };

    let count = tokio::task::spawn_blocking(move || meter.count_attestations((from.start(), until.end()))).await;
    let detail = match count {
        Ok(Ok(count)) => return json_response(StatusCode::OK, count.to_string().into()),
        Ok(Err(e)) => e.to_string(),
        Err(e) => e.to_string(),
    };
    tracing::error!(error = %detail, operation = "countAttestations", "failed to read the metering database");
    json_response(StatusCode::INTERNAL_SERVER_ERROR, error_body("database_error", Some(&detail)))
}

/// `from` and `until`, each sent exactly once, and no other parameter.
fn window(params: Vec<(String, String)>) -> Result<(Timestamp, Timestamp), Bytes> {
    // Sorted and deduplicated, like aaoracled's unknown_field detail.
    let unknown: BTreeSet<&str> =
        params.iter().map(|(name, _)| name.as_str()).filter(|name| !["from", "until"].contains(name)).collect();
    if !unknown.is_empty() {
        let names = unknown.into_iter().collect::<Vec<_>>().join(", ");
        return Err(error_body("unknown_parameter", Some(&format!("GET /attestations does not accept: {names}"))));
    }

    let (mut from, mut until) = (None, None);
    for (name, value) in params {
        let slot = if name == "from" { &mut from } else { &mut until };
        if slot.replace(value).is_some() {
            return Err(bad_request(&format!("{name} must be sent only once")));
        }
    }
    let parse = |name: &str, value: Option<String>| {
        let value = value.ok_or_else(|| bad_request(&format!("{name} is required")))?;
        Timestamp::parse(&value).ok_or_else(|| {
            bad_request(&format!("{name} must be a UTC timestamp like 2026-10-11T19:07:53.846258Z, not {value:?}"))
        })
    };
    let (from, until) = (parse("from", from)?, parse("until", until)?);
    if from.nanos > until.nanos {
        return Err(bad_request("from is after until"));
    }
    Ok((from, until))
}

fn bad_request(detail: &str) -> Bytes {
    error_body("bad_request", Some(detail))
}

/// The contract's `Timestamp`, kept as text in the fixed-width layouts that
/// make comparing text the same as comparing instants.
struct Timestamp {
    /// `YYYY-MM-DDTHH:MM:SS.nnnnnnnnnZ`: the full instant.
    nanos: String,
    /// `YYYY-MM-DDTHH:MM:SS.mmmZ`, `requested_at`'s own layout: the instant
    /// truncated to the millisecond.
    millis: String,
    /// Whether that truncation dropped any nonzero digits.
    truncated: bool,
}

impl Timestamp {
    /// Parses `YYYY-MM-DDTHH:MM:SS`, then 0 to 9 fractional-second digits,
    /// then `Z`, naming a real date and time.
    fn parse(s: &str) -> Option<Self> {
        let rest = s.strip_suffix('Z')?;
        let (date_time, fraction) = match rest.split_once('.') {
            Some((_, "")) => return None,
            Some(split) => split,
            None => (rest, ""),
        };
        let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        if fraction.len() > 9 || !digits(fraction) || date_time.len() != 19 {
            return None;
        }
        let separators = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')];
        if separators.iter().any(|&(i, separator)| date_time.as_bytes()[i] != separator) {
            return None;
        }
        let field = |i: usize, len: usize| date_time.get(i..i + len).filter(|s| digits(s))?.parse::<u32>().ok();
        let [Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)] =
            [(0, 4), (5, 2), (8, 2), (11, 2), (14, 2), (17, 2)].map(|(i, len)| field(i, len))
        else {
            return None;
        };
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let month_days = match month {
            2 if leap => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            1..=12 => 31,
            _ => return None,
        };
        if !(1..=month_days).contains(&day) || hour > 23 || minute > 59 || second > 59 {
            return None;
        }

        let nanos = format!("{date_time}.{fraction:0<9}Z");
        Some(Self { millis: format!("{}Z", &nanos[..23]), truncated: &nanos[23..29] != "000000", nanos })
    }

    // `requested_at` is truncated to the millisecond, and the contract
    // compares that truncated instant with the bounds. A bound with finer
    // digits falls strictly inside a millisecond m, so m itself is before it
    // and m + 1ms after it: as a bound, it excludes or includes its own
    // truncation instead of matching it exactly.

    /// This instant as the inclusive start of a `requested_at` range.
    fn start(&self) -> Bound<&str> {
        if self.truncated { Bound::Excluded(&self.millis) } else { Bound::Included(&self.millis) }
    }

    /// This instant as the exclusive end of a `requested_at` range.
    fn end(&self) -> Bound<&str> {
        if self.truncated { Bound::Included(&self.millis) } else { Bound::Excluded(&self.millis) }
    }
}
