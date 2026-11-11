//! One LIVE order's timeline, from the send to the moment the engine's own
//! poll saw it finished, and the single log line it ends in (see
//! `order_timing`).
//!
//! Everything is on this server's clock except Kotak's own stamp and the
//! exchange's whole-second confirm time, which come with the push.

use std::collections::HashMap;

use kotak_client::orders::BookFinished;
use serde_json::{json, Value};
use shared_domain::{OrderSent, OrderUpdate};

/// After the engine saw an order finished, how long its push is waited for.
pub const PUSH_GRACE_MS: i64 = 5_000;
/// After Kotak pushed an order finished, how long the engine's poll is waited
/// for before the line is written without it.
pub const POLL_GRACE_MS: i64 = 30_000;
/// Updates for an order this server didn't send (one placed in Kotak's own
/// app) are kept this long, in case its send is only behind them.
pub const UNSENT_KEEP_MS: i64 = 120_000;
/// Kotak's stamp is only compared with ours when the two are this close.
const KOTAK_SKEW_MS: std::ops::RangeInclusive<i64> = -5_000..=30_000;

fn is_finished(status: &str) -> bool {
    matches!(status, "complete" | "rejected" | "cancelled" | "canceled")
}

fn is_accepted(status: &str) -> bool {
    matches!(status, "open" | "trigger pending" | "complete" | "partially filled")
}

/// The push stream, as the line describes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Push {
    Off,
    NotConnected,
    Live,
}

#[derive(Debug, Clone, Default)]
struct Timeline {
    created_ms: i64,
    sent: Option<OrderSent>,
    accepted_ms: Option<i64>,
    first_fill_ms: Option<i64>,
    reject_reason: Option<String>,
    /// The push that finished it.
    finish: Option<OrderUpdate>,
    /// The engine's poll that first saw it finished.
    seen: Option<BookFinished>,
}

/// Every order still being timed, by order number.
#[derive(Debug, Default)]
pub struct Timelines {
    orders: HashMap<String, Timeline>,
    day: Option<chrono::NaiveDate>,
}

impl Timelines {
    fn entry(&mut self, order_no: &str, at_ms: i64) -> &mut Timeline {
        self.orders
            .entry(order_no.trim().to_string())
            .or_insert_with(|| Timeline { created_ms: at_ms, ..Timeline::default() })
    }

    /// A send. One Kotak refused has no number and nothing more to wait
    /// for: its line comes straight back.
    pub fn on_sent(&mut self, sent: &OrderSent) -> Option<Value> {
        match sent.order_no.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            Some(no) => {
                self.entry(no, sent.sent_at_ms).sent = Some(sent.clone());
                None
            }
            None => Some(refused_line(sent)),
        }
    }

    /// An update from Kotak's push stream; it may beat its order's send here.
    pub fn on_update(&mut self, u: &OrderUpdate) {
        let at = u.received_at_ms;
        let t = self.entry(&u.order_no, at);
        if is_accepted(&u.status) {
            t.accepted_ms.get_or_insert(at);
        }
        if u.filled_qty.is_some_and(|q| q > 0) {
            t.first_fill_ms.get_or_insert(at);
        }
        if u.reject_reason.is_some() {
            t.reject_reason.clone_from(&u.reject_reason);
        }
        if is_finished(&u.status) && t.finish.is_none() {
            t.finish = Some(u.clone());
        }
    }

    /// The engine's poll saw an order finished: `true` for an order being
    /// timed here, whose row is worth the write.
    pub fn on_seen(&mut self, seen: &BookFinished) -> bool {
        match self.orders.get_mut(seen.order_no.trim()) {
            Some(t) => {
                t.seen.get_or_insert_with(|| seen.clone());
                true
            }
            None => false,
        }
    }

    /// The line of every order whose story is told at `now`, which then
    /// leaves. A new IST day starts empty.
    pub fn due(&mut self, now: i64, push: Push) -> Vec<Value> {
        let today = chrono::DateTime::from_timestamp_millis(now).map(|t| t.with_timezone(&shared_domain::ist_offset()).date_naive());
        if today.is_some() && self.day != today {
            // Yesterday's unfinished orders (Kotak lets them lapse) go untold.
            if self.day.is_some() {
                self.orders.clear();
            }
            self.day = today;
        }
        let mut lines = Vec::new();
        self.orders.retain(|order_no, t| {
            let Some(sent) = &t.sent else {
                return now - t.created_ms < UNSENT_KEEP_MS;
            };
            let ready = match (&t.seen, &t.finish) {
                (Some(_), Some(_)) => true,
                (Some(seen), None) => push != Push::Live || now - seen.at_ms >= PUSH_GRACE_MS,
                (None, Some(finish)) => now - finish.received_at_ms >= POLL_GRACE_MS,
                (None, None) => false,
            };
            if ready {
                lines.push(line(order_no, sent, t, push));
            }
            !ready
        });
        lines
    }
}

/// "33 ms", "20.1 s", "2m 05s", "1h 20m".
pub fn span(ms: i64) -> String {
    let a = ms.unsigned_abs();
    let sign = if ms < 0 { "-" } else { "" };
    if a < 1_000 {
        format!("{sign}{a} ms")
    } else if a < 60_000 {
        format!("{sign}{:.1} s", a as f64 / 1_000.0)
    } else if a < 3_600_000 {
        format!("{sign}{}m {:02}s", a / 60_000, (a % 60_000) / 1_000)
    } else {
        format!("{sign}{}h {:02}m", a / 3_600_000, (a % 3_600_000) / 60_000)
    }
}

fn side_word(side: &str) -> &'static str {
    if side.trim().eq_ignore_ascii_case("S") {
        "SELL"
    } else {
        "BUY"
    }
}

fn refused_line(sent: &OrderSent) -> Value {
    let answer = sent.replied_at_ms - sent.sent_at_ms;
    let message = format!(
        "{} {} {} refused by Kotak in {}: {}",
        side_word(&sent.side),
        sent.qty,
        sent.trading_symbol,
        span(answer),
        sent.error.as_deref().unwrap_or("no reason given"),
    );
    json!({
        "event": "ORDER_TIMING",
        "level": "INFO",
        "message": message,
        "symbol": sent.trading_symbol,
        "side": side_word(&sent.side),
        "order_type": sent.order_type,
        "qty": sent.qty,
        "status": "refused",
        "kotak_answer_ms": answer,
    })
}

/// One order's line: what happened, then each stage counted from the send.
fn line(order_no: &str, sent: &OrderSent, t: &Timeline, push: Push) -> Value {
    let from_send = |at: i64| at - sent.sent_at_ms;
    let finish = t.finish.as_ref();
    let seen = t.seen.as_ref();
    let status = finish.map(|f| f.status.as_str()).or(seen.map(|s| s.status.as_str())).unwrap_or_default();
    let filled_qty = finish.and_then(|f| f.filled_qty).or(seen.map(|s| s.filled_qty));
    let avg_price = finish.and_then(|f| f.avg_price).or(seen.map(|s| s.avg_price)).filter(|p| *p > 0.0);

    let answer_ms = sent.replied_at_ms - sent.sent_at_ms;
    let at_exchange_ms = t.accepted_ms.map(from_send);
    let first_fill_ms = t.first_fill_ms.map(from_send);
    let exchange_time = finish.and_then(|f| f.exchange_confirm.clone());
    let kotak_to_us_ms = finish
        .and_then(|f| f.kotak_received_ns.map(|ns| f.received_at_ms - ns / 1_000_000))
        .filter(|d| KOTAK_SKEW_MS.contains(d));
    // The two ways this server hears an order finished: Kotak's push, and the
    // engine's own order-book poll (the one the engine acts on).
    let push_ms = finish.map(|f| from_send(f.received_at_ms));
    let poll_ms = seen.map(|s| from_send(s.at_ms));
    // Which told us first, and by how much.
    let (first_by, first_lead_ms) = match (push_ms, poll_ms) {
        (Some(push), Some(poll)) if push <= poll => ("push", Some(poll - push)),
        (Some(push), Some(poll)) => ("poll", Some(push - poll)),
        (Some(_), None) => ("push", None),
        _ => ("poll", None),
    };

    let outcome = match status {
        "complete" => avg_price.map_or("filled".to_string(), |p| format!("filled at {p:.2}")),
        "rejected" => format!("rejected{}", t.reject_reason.as_deref().map(|r| format!(": {r}")).unwrap_or_default()),
        "cancelled" | "canceled" => match filled_qty {
            Some(q) if q > 0 => format!("cancelled after {q} of {} filled", sent.qty),
            _ => "cancelled".to_string(),
        },
        _ => "finished".to_string(),
    };
    let verb = match status {
        "complete" => "filled",
        "rejected" => "rejected",
        "cancelled" | "canceled" => "cancelled",
        _ => "finished",
    };

    let mut parts = vec![
        format!("{} {} {} {outcome}", side_word(&sent.side), sent.qty, sent.trading_symbol),
        format!("Kotak answered in {}", span(answer_ms)),
    ];
    if let Some(ms) = at_exchange_ms {
        parts.push(format!("at the exchange +{}", span(ms)));
    }
    if let (Some(first), Some(done)) = (first_fill_ms, push_ms) {
        if status == "complete" && first != done {
            parts.push(format!("first fill +{}", span(first)));
        }
    }
    if let Some(ms) = push_ms {
        parts.push(format!("push: {verb} +{}", span(ms)));
    }
    if let Some(ms) = poll_ms {
        parts.push(format!("poll: {verb} +{}", span(ms)));
    }
    parts.push(match (first_lead_ms, poll_ms) {
        (Some(0), _) => "first: push and poll together".to_string(),
        (Some(lead), _) => format!("first: {first_by}, {} ahead", span(lead)),
        (None, Some(_)) => format!(
            "first: poll ({})",
            match push {
                Push::Off => "push stream off",
                Push::NotConnected => "push stream not connected",
                Push::Live => "no push for it",
            }
        ),
        (None, None) => format!("first: push (the poll hadn't seen it {} later)", span(POLL_GRACE_MS)),
    });
    if let Some(at) = &exchange_time {
        parts.push(format!("exchange time {at}"));
    }
    if let Some(ms) = kotak_to_us_ms {
        parts.push(format!("Kotak → us {}", span(ms)));
    }
    parts.push(format!("#{order_no}"));

    json!({
        "event": "ORDER_TIMING",
        "level": "INFO",
        "message": parts.join(" · "),
        "order_id": order_no,
        "symbol": sent.trading_symbol,
        "side": side_word(&sent.side),
        "order_type": sent.order_type,
        "qty": sent.qty,
        "status": if status.is_empty() { "finished" } else { status },
        "filled_qty": filled_qty,
        "avg_price": avg_price,
        "kotak_answer_ms": answer_ms,
        "at_exchange_ms": at_exchange_ms,
        "first_fill_ms": first_fill_ms,
        "push_ms": push_ms,
        "poll_ms": poll_ms,
        "first_reported_by": first_by,
        "first_lead_ms": first_lead_ms,
        "exchange_time": exchange_time,
        "kotak_to_us_ms": kotak_to_us_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-11 09:20:00 IST.
    const T0: i64 = 1_794_369_000_000;

    fn sent(no: Option<&str>, at: i64, error: Option<&str>) -> OrderSent {
        OrderSent {
            sent_at_ms: at,
            replied_at_ms: at + 33,
            order_no: no.map(str::to_string),
            error: error.map(str::to_string),
            trading_symbol: "SENSEX26N1282400CE".into(),
            side: "B".into(),
            order_type: "MKT".into(),
            qty: 40,
            price: 0.0,
            trigger: 0.0,
        }
    }

    fn update(no: &str, at: i64, status: &str, filled: i64, avg: Option<f64>) -> OrderUpdate {
        OrderUpdate {
            received_at_ms: at,
            order_no: no.into(),
            status: status.into(),
            filled_qty: Some(filled),
            avg_price: avg,
            kotak_received_ns: Some((at - 40) * 1_000_000),
            exchange_confirm: Some("11-Nov-2026 09:20:20".into()),
            ..OrderUpdate::default()
        }
    }

    fn seen(no: &str, at: i64, status: &str, filled: i64, avg: f64) -> BookFinished {
        BookFinished { order_no: no.into(), status: status.into(), filled_qty: filled, avg_price: avg, at_ms: at }
    }

    #[test]
    fn a_pushed_fill_and_the_engines_sighting_make_one_line_with_every_stage() {
        let mut tl = Timelines::default();
        // Kotak's first push beats the send tap.
        tl.on_update(&update("7", T0 + 20, "open pending", 0, None));
        assert!(tl.on_sent(&sent(Some("7"), T0, None)).is_none());
        tl.on_update(&update("7", T0 + 120, "open", 0, None));
        tl.on_update(&update("7", T0 + 20_100, "complete", 40, Some(466.5)));
        assert!(tl.due(T0 + 20_200, Push::Live).is_empty(), "waits for the engine's poll");
        assert!(tl.on_seen(&seen("7", T0 + 20_800, "complete", 40, 466.5)));

        let lines = tl.due(T0 + 20_900, Push::Live);
        assert_eq!(lines.len(), 1);
        let l = &lines[0];
        assert_eq!(
            l["message"],
            "BUY 40 SENSEX26N1282400CE filled at 466.50 · Kotak answered in 33 ms · at the exchange +120 ms · push: filled +20.1 s · \
             poll: filled +20.8 s · first: push, 700 ms ahead · exchange time 11-Nov-2026 09:20:20 · Kotak → us 40 ms · #7"
        );
        assert_eq!((l["kotak_answer_ms"].as_i64(), l["at_exchange_ms"].as_i64(), l["push_ms"].as_i64(), l["poll_ms"].as_i64()), (Some(33), Some(120), Some(20_100), Some(20_800)));
        assert_eq!((l["first_reported_by"].as_str(), l["first_lead_ms"].as_i64(), l["kotak_to_us_ms"].as_i64()), (Some("push"), Some(700), Some(40)));
        assert!(tl.due(T0 + 60_000, Push::Live).is_empty(), "told once, then gone");
    }

    #[test]
    fn a_poll_that_beats_the_push_is_named_first() {
        let mut tl = Timelines::default();
        tl.on_sent(&sent(Some("13"), T0, None));
        tl.on_seen(&seen("13", T0 + 500, "complete", 40, 500.0));
        assert!(tl.due(T0 + 600, Push::Live).is_empty(), "waits for the push");
        tl.on_update(&update("13", T0 + 800, "complete", 40, Some(500.0)));
        let lines = tl.due(T0 + 900, Push::Live);
        let text = lines[0]["message"].as_str().unwrap();
        assert!(text.contains("push: filled +800 ms · poll: filled +500 ms · first: poll, 300 ms ahead"), "{text}");
        assert_eq!((lines[0]["first_reported_by"].as_str(), lines[0]["first_lead_ms"].as_i64()), (Some("poll"), Some(300)));
    }

    #[test]
    fn with_the_push_stream_off_the_engines_sighting_is_the_whole_story() {
        let mut tl = Timelines::default();
        tl.on_sent(&sent(Some("8"), T0, None));
        tl.on_seen(&seen("8", T0 + 1_100, "complete", 40, 422.6));
        let lines = tl.due(T0 + 1_150, Push::Off);
        assert_eq!(
            lines[0]["message"],
            "BUY 40 SENSEX26N1282400CE filled at 422.60 · Kotak answered in 33 ms · poll: filled +1.1 s · first: poll (push stream off) · #8"
        );
    }

    #[test]
    fn a_live_stream_gets_a_few_seconds_to_bring_the_push_the_poll_beat() {
        let mut tl = Timelines::default();
        tl.on_sent(&sent(Some("9"), T0, None));
        tl.on_seen(&seen("9", T0 + 900, "complete", 40, 500.0));
        assert!(tl.due(T0 + 1_000, Push::Live).is_empty());
        let lines = tl.due(T0 + 900 + PUSH_GRACE_MS, Push::Live);
        assert!(lines[0]["message"].as_str().unwrap().ends_with("poll: filled +900 ms · first: poll (no push for it) · #9"));
    }

    #[test]
    fn a_push_the_engine_never_sees_is_still_told_after_the_grace() {
        let mut tl = Timelines::default();
        tl.on_sent(&sent(Some("10"), T0, None));
        tl.on_update(&update("10", T0 + 80, "rejected", 0, None));
        assert!(tl.due(T0 + 1_000, Push::Live).is_empty());
        let lines = tl.due(T0 + 80 + POLL_GRACE_MS, Push::Live);
        let text = lines[0]["message"].as_str().unwrap();
        assert!(
            text.starts_with("BUY 40 SENSEX26N1282400CE rejected · Kotak answered in 33 ms · push: rejected +80 ms · first: push (the poll hadn't seen it 30.0 s later)"),
            "{text}"
        );
    }

    #[test]
    fn a_refused_send_is_told_at_once_and_updates_for_orders_never_sent_here_are_dropped() {
        let mut tl = Timelines::default();
        let refused = tl.on_sent(&sent(None, T0, Some("RMS: margin exceeds"))).unwrap();
        assert_eq!(refused["message"], "BUY 40 SENSEX26N1282400CE refused by Kotak in 33 ms: RMS: margin exceeds");

        // Placed in Kotak's own app: pushed and polled, never sent from here.
        tl.on_update(&update("11", T0, "complete", 40, Some(100.0)));
        assert!(tl.on_seen(&seen("11", T0 + 500, "complete", 40, 100.0)));
        assert!(tl.due(T0 + 1_000, Push::Live).is_empty());
        assert!(tl.due(T0 + UNSENT_KEEP_MS + 1, Push::Live).is_empty());
        assert!(!tl.on_seen(&seen("11", T0 + UNSENT_KEEP_MS + 2, "complete", 40, 100.0)), "gone");
        // An order the poll lists that nothing here knows (an earlier one).
        assert!(!tl.on_seen(&seen("12", T0, "complete", 40, 100.0)));
    }

    #[test]
    fn spans_read_naturally() {
        assert_eq!([span(33), span(20_100), span(125_000), span(4_980_000), span(-300)], ["33 ms", "20.1 s", "2m 05s", "1h 23m", "-300 ms"]);
    }
}
