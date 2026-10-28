//! Every SSE stream goes out unbuffered. `X-Accel-Buffering: no` tells Nginx
//! to pass each event on as it is written. The VM's Nginx also has
//! `proxy_buffering off`, but a replaced config would otherwise bring back
//! the 16 KB bursts that held the live frames back by seconds.

use axum::{
    http::{header, HeaderValue},
    response::Response,
};

/// A response mapper for the router: marks every `text/event-stream`
/// answer, whichever route sent it, so a new stream can't miss it.
pub async fn no_buffering(mut resp: Response) -> Response {
    let sse = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/event-stream"));
    if sse {
        resp.headers_mut().insert("x-accel-buffering", HeaderValue::from_static("no"));
    }
    resp
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use axum::{
        body::Body,
        http::Request,
        response::sse::{Event, Sse},
        routing::get,
        Router,
    };
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn streams_go_out_unbuffered_and_nothing_else_is_touched() {
        let app = Router::new()
            .route("/stream", get(|| async { Sse::new(futures::stream::iter([Ok::<_, Infallible>(Event::default().data("x"))])) }))
            .route("/plain", get(|| async { "plain" }))
            .layer(axum::middleware::map_response(no_buffering));
        let get_ = |path: &str| Request::get(path).body(Body::empty()).unwrap();
        let stream = app.clone().oneshot(get_("/stream")).await.unwrap();
        assert_eq!(stream.headers().get("x-accel-buffering").map(|v| v.to_str().unwrap()), Some("no"));
        let plain = app.oneshot(get_("/plain")).await.unwrap();
        assert!(plain.headers().get("x-accel-buffering").is_none());
    }
}
