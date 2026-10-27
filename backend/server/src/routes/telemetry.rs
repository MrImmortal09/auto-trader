//! Stats endpoints: Prometheus exposition plus the System screen's pulse.
//!
//! All three are read-only GETs, public under the same rule as every other
//! dashboard read (see `auth_middleware`).

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{MatchedPath, Request, State},
    http::header,
    middleware::Next,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use futures::stream::StreamExt as _;
use serde::Serialize;
use shared_domain::telemetry;
use tokio_stream::wrappers::WatchStream;

use crate::pulse::{Point, Sample};
use crate::AppState;

/// `GET /metrics` — Prometheus text exposition, for any external scraper.
pub async fn metrics_handler() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        telemetry::render_text(),
    )
}

#[derive(Serialize)]
pub struct TelemetrySnapshot {
    latest: Option<Arc<Sample>>,
    history: Vec<Point>,
    sample_every_secs: u64,
}

/// `GET /api/telemetry/snapshot` — the latest sample plus the 15-minute
/// history ring, so the System screen draws full charts on load.
pub async fn telemetry_snapshot_handler(State(state): State<AppState>) -> Json<TelemetrySnapshot> {
    Json(TelemetrySnapshot {
        latest: state.pulse.latest(),
        history: state.pulse.history(),
        sample_every_secs: crate::pulse::SAMPLE_EVERY.as_secs(),
    })
}

/// `GET /api/telemetry/stream` — SSE, one [`Sample`] per sampler tick. The
/// owner's and the admin's; an admin disabled meanwhile loses it at the
/// next sample.
pub async fn telemetry_stream_handler(
    State(state): State<AppState>,
    axum::Extension(identity): axum::Extension<crate::auth::Identity>,
) -> Sse<impl futures::stream::Stream<Item = Result<Event, Infallible>>> {
    let auth = state.auth.clone();
    let stream = WatchStream::from_changes(state.pulse.subscribe())
        .take_while(move |_| {
            let still = crate::auth::still_signed_in(&auth, &identity);
            async move { still }
        })
        .filter_map(|sample| async move {
            let sample = sample?;
            serde_json::to_string(&*sample).ok().map(|json| Ok(Event::default().data(json)))
        });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Decrements the in-flight gauge however the request ends, including a
/// client hanging up mid-request.
struct InFlight;

impl InFlight {
    fn enter() -> Self {
        telemetry::HTTP_IN_FLIGHT.inc();
        InFlight
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        telemetry::HTTP_IN_FLIGHT.dec();
    }
}

/// HTTP RED metrics. Labels by the matched route pattern (bounded); the static
/// frontend fallback and unknown `/api` paths collapse to one label each.
pub async fn http_metrics(req: Request, next: Next) -> Response {
    let route = match req.extensions().get::<MatchedPath>() {
        Some(p) => p.as_str().to_owned(),
        None if req.uri().path().starts_with("/api/") => "unmatched".to_owned(),
        None => "static".to_owned(),
    };
    let method = req.method().as_str().to_owned();
    let _in_flight = InFlight::enter();
    let started = Instant::now();
    let resp = next.run(req).await;
    telemetry::http_request(&route, &method, resp.status().as_u16(), started.elapsed().as_secs_f64());
    resp
}
