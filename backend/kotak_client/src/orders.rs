//! Order timing (analytics only): when each order left this server and when
//! Kotak answered, and Kotak's push stream of order updates ("Order &
//! Position Streaming", `wss://<baseUrl host>/realtime`), which says when an
//! order was accepted and filled without waiting for an order-book poll.
//!
//! Nothing here decides anything: the tap copies a finished `place_live_order`
//! call (only while something listens, and never waits on the listener), and
//! the stream only reads.

use std::sync::LazyLock;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
pub use shared_domain::{OrderSent, OrderUpdate};
use shared_domain::OrderRequest;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message;

fn now_ms() -> i64 {
    shared_domain::now_ist().timestamp_millis()
}

// ---------------------------------------------------------------------------
// The send tap
// ---------------------------------------------------------------------------

/// One `place_live_order` call: the order's own fields, as `OrderRequest`
/// serialises them for Kotak, and how it went.
pub fn order_sent(order: &OrderRequest, sent_at_ms: i64, replied_at_ms: i64, result: Result<&str, String>) -> OrderSent {
    let fields = serde_json::to_value(order).unwrap_or(Value::Null);
    let text = |k: &str| fields.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let number = |k: &str| fields.get(k).and_then(Value::as_str).and_then(|s| s.trim().parse::<f64>().ok()).unwrap_or(0.0);
    let (order_no, error) = match result {
        Ok(no) => (Some(no.to_string()).filter(|n| !n.is_empty()), None),
        Err(e) => (None, Some(e)),
    };
    OrderSent {
        sent_at_ms,
        replied_at_ms,
        order_no,
        error,
        trading_symbol: text("ts"),
        side: text("tt"),
        order_type: text("pt"),
        qty: number("qt") as i64,
        price: number("pr"),
        trigger: number("tp"),
    }
}

static ORDER_TAP: LazyLock<broadcast::Sender<OrderSent>> = LazyLock::new(|| broadcast::channel(256).0);

/// Every order sent from now on, timed. A listener that falls behind misses
/// some; the order itself never waits on it.
pub fn subscribe_orders() -> broadcast::Receiver<OrderSent> {
    ORDER_TAP.subscribe()
}

/// The time an order is sent, for [`publish_order`].
pub(crate) fn send_clock() -> i64 {
    now_ms()
}

/// Copy a finished send to the tap — only while something listens.
pub(crate) fn publish_order(order: &OrderRequest, sent_at_ms: i64, result: Result<&str, String>) {
    if ORDER_TAP.receiver_count() > 0 {
        let _ = ORDER_TAP.send(order_sent(order, sent_at_ms, now_ms(), result));
    }
}

// ---------------------------------------------------------------------------
// The engine's poll tap
// ---------------------------------------------------------------------------

/// The engine's order-book poll showed an order finished (`complete`,
/// `rejected` or `cancelled`) for the first time — the moment the engine
/// learned of it, on this server's clock. It closes the order's timeline:
/// sent, answered, pushed, seen.
#[derive(Debug, Clone, PartialEq)]
pub struct BookFinished {
    pub order_no: String,
    /// Kotak's word, lower case.
    pub status: String,
    pub filled_qty: i64,
    /// `0.0` when nothing filled.
    pub avg_price: f64,
    pub at_ms: i64,
}

/// Every poll of a day lists that day's earlier orders too, finished ones
/// included: a listener that just started sees them all at once.
static BOOK_TAP: LazyLock<broadcast::Sender<BookFinished>> = LazyLock::new(|| broadcast::channel(1024).0);

/// Each order the engine's poll first sees finished, from now on.
pub fn subscribe_book_finished() -> broadcast::Receiver<BookFinished> {
    BOOK_TAP.subscribe()
}

/// Called by the engine's poll for each order it first sees finished; only
/// while something listens, and never waits on the listener.
pub fn publish_book_finished(order_no: &str, status: &str, filled_qty: i64, avg_price: f64) {
    if BOOK_TAP.receiver_count() > 0 {
        let _ = BOOK_TAP.send(BookFinished {
            order_no: order_no.to_string(),
            status: status.trim().to_lowercase(),
            filled_qty,
            avg_price,
            at_ms: now_ms(),
        });
    }
}

// ---------------------------------------------------------------------------
// The push stream
// ---------------------------------------------------------------------------

/// What the stream delivers.
#[derive(Debug, Clone, PartialEq)]
pub enum FeedEvent {
    /// Kotak acknowledged the login.
    Connected,
    Order(OrderUpdate),
    /// Anything else (a position update, a heartbeat reply, a format this
    /// code doesn't know yet), cut short, for the log.
    Other(String),
}

/// The two ways Kotak documents the login message. Its page says a raw
/// `{type:cn,Authorization:…,Sid:…,src:WEB}` string ("NOT JSON"); its demo
/// sends JSON with `source`.
///
/// Raw is the one that works (live session, 2026-11-11): Kotak answers it
/// with `{"ak":"ok","type":"cn","task":"cn","msg":"connected"}` in ~30 ms,
/// and its own `hslib.js` sends exactly that — the demo's JSON with the
/// quotes stripped. The JSON login is refused ("session message format
/// incorrect") and the socket reset. Json is kept as the fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStyle {
    Json,
    Raw,
}

impl AuthStyle {
    /// The login tried first.
    pub const FIRST: AuthStyle = AuthStyle::Raw;

    pub fn other(self) -> AuthStyle {
        match self {
            AuthStyle::Json => AuthStyle::Raw,
            AuthStyle::Raw => AuthStyle::Json,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            AuthStyle::Json => "json",
            AuthStyle::Raw => "raw",
        }
    }

    fn login(self, auth_token: &str, sid: &str) -> String {
        match self {
            AuthStyle::Json => serde_json::json!({ "type": "cn", "Authorization": auth_token, "Sid": sid, "source": "WEB" }).to_string(),
            AuthStyle::Raw => format!("{{type:cn,Authorization:{auth_token},Sid:{sid},src:WEB}}"),
        }
    }
}

/// How long Kotak has to acknowledge the login.
const ACK_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the connect (TCP, TLS and the upgrade) may take. Right after
/// Kotak reset a refused login, the next connect was seen to hang with no
/// answer at all (2026-11-11); unbounded, the stream would wait on it until
/// the next re-login.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The demo's heartbeat period.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(30);
/// What an unknown message is cut to for the log.
const OTHER_MAX_CHARS: usize = 400;

/// `wss://e21.kotaksecurities.com/realtime` for a session whose base URL is
/// `https://e21.kotaksecurities.com` (any path on it is dropped).
pub fn order_feed_url(base_url: &str) -> Option<String> {
    let rest = base_url.trim().split_once("://").map_or(base_url.trim(), |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().filter(|h| !h.is_empty())?;
    Some(format!("wss://{host}/realtime"))
}

fn get<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| v.get(*k)).filter(|x| !x.is_null())
}

fn text(v: &Value, keys: &[&str]) -> Option<String> {
    match get(v, keys)? {
        Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn number(v: &Value, keys: &[&str]) -> Option<f64> {
    match get(v, keys)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn integer(v: &Value, keys: &[&str]) -> Option<i64> {
    match get(v, keys)? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse::<i64>().ok().or_else(|| s.trim().parse::<f64>().ok().map(|f| f as i64)),
        _ => None,
    }
}

/// One message from the stream, read leniently: an order update is anything
/// carrying an order number, at the top or under `data`.
pub fn parse_message(text_frame: &str, received_at_ms: i64) -> FeedEvent {
    let Ok(v) = serde_json::from_str::<Value>(text_frame) else {
        return FeedEvent::Other(text_frame.chars().take(OTHER_MAX_CHARS).collect());
    };
    let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();
    let acked = v.get("ak").and_then(Value::as_str).is_some_and(|a| a.eq_ignore_ascii_case("ok"));
    if kind == "cn" && (acked || v.get("msg").and_then(Value::as_str).is_some_and(|m| m.eq_ignore_ascii_case("connected"))) {
        return FeedEvent::Connected;
    }
    let body = match v.get("data") {
        Some(d @ Value::Object(_)) => d,
        _ => &v,
    };
    let Some(order_no) = text(body, &["nOrdNo", "orderNo"]) else {
        return FeedEvent::Other(text_frame.chars().take(OTHER_MAX_CHARS).collect());
    };
    FeedEvent::Order(OrderUpdate {
        received_at_ms,
        order_no,
        status: text(body, &["ordSt", "orderStatus"]).unwrap_or_default().to_lowercase(),
        trading_symbol: text(body, &["trdSym", "sym"]),
        side: text(body, &["trnsTp"]),
        qty: integer(body, &["qty"]),
        filled_qty: integer(body, &["fldQty"]),
        avg_price: number(body, &["avgPrc"]).filter(|p| *p > 0.0),
        reject_reason: text(body, &["rejRsn"]),
        kotak_received_ns: integer(body, &["updRecvTm"]).filter(|t| *t > 0),
        exchange_epoch_s: integer(body, &["boeSec"]).filter(|t| *t > 0),
        exchange_confirm: text(body, &["exCfmTm"]),
        order_time: text(body, &["ordDtTm"]),
    })
}

/// Connects to the session's order stream, logs in `style`, and sends every
/// event to `out` until the socket closes (`Ok`) or fails. Waits at most
/// [`CONNECT_TIMEOUT`] for the connect and [`ACK_TIMEOUT`] for Kotak to
/// acknowledge the login. Read-only: the only messages sent are the login and
/// the heartbeat.
pub async fn stream_order_updates(
    base_url: &str,
    auth_token: &str,
    sid: &str,
    style: AuthStyle,
    out: mpsc::Sender<FeedEvent>,
) -> Result<(), String> {
    let url = order_feed_url(base_url).ok_or_else(|| format!("no host in base URL {base_url:?}"))?;
    let (ws, _) = tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(url.as_str()))
        .await
        .map_err(|_| format!("connect {url}: no answer in {} s", CONNECT_TIMEOUT.as_secs()))?
        .map_err(|e| format!("connect {url}: {e}"))?;
    let (mut sink, mut stream) = ws.split();
    sink.send(Message::Text(style.login(auth_token, sid))).await.map_err(|e| format!("login send: {e}"))?;

    let ack_deadline = tokio::time::sleep(ACK_TIMEOUT);
    tokio::pin!(ack_deadline);
    let mut acked = false;
    let mut heartbeat = tokio::time::interval(HEARTBEAT_EVERY);
    heartbeat.tick().await;

    loop {
        tokio::select! {
            _ = &mut ack_deadline, if !acked => {
                return Err(format!("no login acknowledgement in {} s ({} login)", ACK_TIMEOUT.as_secs(), style.name()));
            }
            _ = heartbeat.tick() => {
                sink.send(Message::Text(serde_json::json!({ "type": "hb" }).to_string())).await.map_err(|e| format!("heartbeat: {e}"))?;
            }
            frame = stream.next() => {
                let text_frame = match frame {
                    None => return Ok(()),
                    Some(Err(e)) => return Err(format!("read: {e}")),
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Binary(b))) => String::from_utf8_lossy(&b).into_owned(),
                    Some(Ok(Message::Close(c))) => {
                        return if acked { Ok(()) } else { Err(format!("closed before login was acknowledged: {c:?}")) };
                    }
                    Some(Ok(_)) => continue,
                };
                let event = parse_message(&text_frame, now_ms());
                if event == FeedEvent::Connected {
                    acked = true;
                }
                if out.send(event).await.is_err() {
                    let _ = sink.close().await;
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared_domain::{AmoFlag, ExchangeSegment, OrderType, ProductCode, TransactionType, Validity};

    fn order() -> OrderRequest {
        OrderRequest {
            after_market_order: AmoFlag::No,
            disclosed_quantity: "0".into(),
            exchange_segment: ExchangeSegment::NseFo,
            market_protection: "0".into(),
            product_code: ProductCode::Nrml,
            portfolio_flag: "N".into(),
            price: "0".into(),
            order_type: OrderType::Market,
            quantity: "75".into(),
            validity: Validity::Day,
            trigger_price: "0".into(),
            trading_symbol: "NIFTY26N2425000CE".into(),
            transaction_type: TransactionType::Buy,
        }
    }

    #[test]
    fn a_send_is_recorded_with_its_order_fields_and_outcome() {
        let sent = order_sent(&order(), 1_000, 1_180, Ok("261109000123"));
        assert_eq!(
            (sent.trading_symbol.as_str(), sent.side.as_str(), sent.order_type.as_str(), sent.qty),
            ("NIFTY26N2425000CE", "B", "MKT", 75),
        );
        assert_eq!((sent.order_no.as_deref(), sent.error.as_deref(), sent.replied_at_ms - sent.sent_at_ms), (Some("261109000123"), None, 180));
        let failed = order_sent(&order(), 1_000, 1_050, Err("RMS: margin exceeds".into()));
        assert_eq!((failed.order_no, failed.error.as_deref()), (None, Some("RMS: margin exceeds")));
    }

    #[test]
    fn the_stream_url_is_the_session_host() {
        assert_eq!(order_feed_url("https://e21.kotaksecurities.com").as_deref(), Some("wss://e21.kotaksecurities.com/realtime"));
        assert_eq!(order_feed_url("https://e22.kotaksecurities.com/quick/").as_deref(), Some("wss://e22.kotaksecurities.com/realtime"));
        assert_eq!(order_feed_url(""), None);
    }

    #[test]
    fn both_login_styles_carry_the_token_and_sid() {
        let json: Value = serde_json::from_str(&AuthStyle::Json.login("tok", "sid1")).unwrap();
        assert_eq!((json["type"].as_str(), json["Authorization"].as_str(), json["Sid"].as_str()), (Some("cn"), Some("tok"), Some("sid1")));
        assert_eq!(AuthStyle::Raw.login("tok", "sid1"), "{type:cn,Authorization:tok,Sid:sid1,src:WEB}");
        assert_eq!(AuthStyle::Json.other(), AuthStyle::Raw);
        // Raw is what Kotak acknowledges; JSON is refused and the socket reset.
        assert_eq!((AuthStyle::FIRST, AuthStyle::FIRST.other()), (AuthStyle::Raw, AuthStyle::Json));
    }

    #[test]
    fn order_updates_are_read_flat_or_under_data_and_anything_else_is_kept_for_the_log() {
        assert_eq!(parse_message(r#"{"type":"cn","ak":"ok","task":"cn","msg":"connected"}"#, 5), FeedEvent::Connected);

        // Kotak's documented fields, numbers as numbers or strings.
        let flat = r#"{"type":"order","nOrdNo":"261109000123","ordSt":"complete","trdSym":"NIFTY26N2425000CE","trnsTp":"B",
            "qty":75,"fldQty":"75","avgPrc":"123.45","updRecvTm":1794200130123456789,"boeSec":1794200130,
            "exCfmTm":"09-Nov-2026 10:15:30","ordDtTm":"09-Nov-2026 10:15:30"}"#;
        let FeedEvent::Order(u) = parse_message(flat, 1_794_200_130_400) else { panic!("not an order") };
        assert_eq!((u.order_no.as_str(), u.status.as_str(), u.qty, u.filled_qty, u.avg_price), ("261109000123", "complete", Some(75), Some(75), Some(123.45)));
        assert_eq!((u.kotak_received_ns, u.exchange_epoch_s, u.received_at_ms), (Some(1_794_200_130_123_456_789), Some(1_794_200_130), 1_794_200_130_400));
        assert_eq!(u.exchange_confirm.as_deref(), Some("09-Nov-2026 10:15:30"));

        let nested = r#"{"type":"order","data":{"nOrdNo":"7","ordSt":"Open","avgPrc":"0"}}"#;
        let FeedEvent::Order(u) = parse_message(nested, 1) else { panic!("not an order") };
        assert_eq!((u.order_no.as_str(), u.status.as_str(), u.avg_price), ("7", "open", None));

        assert!(matches!(parse_message(r#"{"type":"position","data":{"sym":"NIFTY"}}"#, 1), FeedEvent::Other(_)));
        assert!(matches!(parse_message("not json", 1), FeedEvent::Other(s) if s == "not json"));
    }
}
