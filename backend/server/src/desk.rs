//! The v2 Desk's views: today's positions and figures (`/api/v2/desk`), the
//! Telegram feed (`/api/v2/feed`) and the status bar (`/api/v2/status`).
//! Read-only — nothing here sends an order or changes a position.
//!
//! Money follows `history`: a sell's P&L is its net proceeds less the same
//! quantity at its call's average buy cost, charges included on both sides.
//! An open position's P&L is `(LTP − average) × held`, before exit charges,
//! as v1's dashboard shows it.

use chrono::{DateTime, FixedOffset, Timelike};
use serde::Serialize;
use shared_domain::{ChartLevels, MonitoredPosition, TradeState};

use crate::history::{Call, LegRow, UNKNOWN_CHANNEL};

/// The colour a chip takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tone {
    Profit,
    Loss,
    Neutral,
}

/// How a closed call ended, as its chip reads.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Outcome {
    pub label: String,
    pub tone: Tone,
}

fn is_buy(leg: &LegRow) -> bool {
    leg.action.eq_ignore_ascii_case("BUY")
}

/// "SOME_REASON" → "Some reason", for a reason with no label of its own.
fn sentence_case(reason: &str) -> String {
    let words = reason.replace('_', " ").to_lowercase();
    let mut chars = words.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// What a closed call's chip says: how its last sell went, and whether
/// target 1 came first ("T1 hit · stop trailed"). `None` before any sell.
pub fn outcome(call: &Call) -> Option<Outcome> {
    let reasons: Vec<&str> = call.legs.iter().filter(|l| !is_buy(l)).filter_map(|l| l.reason.as_deref()).collect();
    let last = *reasons.last()?;
    let t1_first = reasons.iter().any(|r| r.starts_with("TGT1"));
    // A trailed or chart stop can lock in a profit, so it takes the call's sign.
    let by_pnl = if call.pnl.unwrap_or(0.0) >= 0.0 { Tone::Profit } else { Tone::Loss };
    let (label, tone) = match last {
        "TGT2_HIT" => ("T2 hit".to_string(), Tone::Profit),
        r if r.starts_with("TGT") => ("T1 hit".to_string(), Tone::Profit),
        "SL_HIT" | "TRAIL_SL_HIT" | "TRAILED_SL_HIT" if t1_first => ("T1 hit · stop trailed".to_string(), by_pnl),
        "SL_HIT" => ("SL hit".to_string(), Tone::Loss),
        "TRAIL_SL_HIT" | "TRAILED_SL_HIT" => ("Trailed stop hit".to_string(), by_pnl),
        "CHART_STOP" => ("Chart stop".to_string(), by_pnl),
        "CHART_TARGET" => ("Chart target".to_string(), Tone::Profit),
        "OPPOSITE_SIGNAL_EXIT" => ("Opposite call".to_string(), Tone::Neutral),
        "CLOSED_VIA_FRONTEND" | "MANUAL_SELL" | "MANUAL_CLOSE" | "MANUAL_EXIT_ALL" => ("Manual exit".to_string(), Tone::Neutral),
        "KILL_SWITCH" => ("Kill switch".to_string(), Tone::Neutral),
        "EXPIRY_SQUAREOFF" => ("Expiry square-off".to_string(), Tone::Neutral),
        "ENTRY_CHANGED_ERROR" => ("Call edited".to_string(), Tone::Neutral),
        r if r.starts_with("EXIT_AT") => ("Telegram exit".to_string(), Tone::Neutral),
        other => (sentence_case(other), Tone::Neutral),
    };
    Some(Outcome { label, tone })
}

/// A call's average buy cost per unit, charges included.
fn avg_cost(call: &Call) -> Option<f64> {
    let (qty, net) = call.legs.iter().filter(|l| is_buy(l)).fold((0_i64, 0.0), |(q, n), l| (q + l.qty, n + l.net));
    (qty > 0).then(|| net / qty as f64)
}

/// Realized P&L of the sells on `day` (`YYYY-MM-DD`), across every call — a
/// partial exit of a call still open counts on the day it sold.
pub fn booked_on(calls: &[Call], day: &str) -> f64 {
    calls
        .iter()
        .filter_map(|c| {
            let cost = avg_cost(c)?;
            Some(c.legs.iter().filter(|l| !is_buy(l) && l.t.starts_with(day)).map(|l| l.net - l.qty as f64 * cost).sum::<f64>())
        })
        .sum()
}

/// "NIFTY 25000 CE", or the bare index without a strike.
pub fn contract_label(index: &str, strike: Option<f64>, side: Option<&str>) -> String {
    let mut label = index.to_string();
    if let Some(k) = strike {
        label.push(' ');
        label.push_str(&if k.fract() == 0.0 { format!("{k:.0}") } else { format!("{k}") });
    }
    if let Some(side) = side {
        label.push(' ');
        label.push_str(side);
    }
    label
}

/// One trade row, for a toast.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fill {
    pub id: i64,
    /// IST `YYYY-MM-DD HH:MM:SS`.
    pub t: String,
    pub action: String,
    pub qty: i64,
    pub price: f64,
    pub contract: String,
    pub channel: String,
    pub reason: Option<String>,
    /// A sell's realized P&L.
    pub pnl: Option<f64>,
}

/// `day`'s trade rows with an id above `after`, oldest first — the newest
/// `limit` of them.
pub fn fills_after(calls: &[Call], day: &str, after: i64, limit: usize) -> Vec<Fill> {
    let mut fills: Vec<Fill> = calls
        .iter()
        .flat_map(|c| {
            let cost = avg_cost(c);
            let contract = contract_label(&c.index, c.strike, c.side.as_deref());
            c.legs.iter().filter(move |l| l.id > after && l.t.starts_with(day)).map(move |l| Fill {
                id: l.id,
                t: l.t.clone(),
                action: l.action.to_ascii_uppercase(),
                qty: l.qty,
                price: l.price,
                contract: contract.clone(),
                channel: c.channel.clone(),
                reason: l.reason.clone(),
                pnl: if is_buy(l) { None } else { cost.map(|cost| l.net - l.qty as f64 * cost) },
            })
        })
        .collect();
    fills.sort_by_key(|f| f.id);
    if fills.len() > limit {
        fills.drain(..fills.len() - limit);
    }
    fills
}

/// The newest trade row's id, or 0 without any.
pub fn last_leg_id(calls: &[Call]) -> i64 {
    calls.iter().flat_map(|c| c.legs.iter().map(|l| l.id)).max().unwrap_or(0)
}

/// Where a position came from, as its chip reads.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Source {
    /// `telegram`, `scalp` or `other`.
    pub kind: &'static str,
    pub title: String,
}

pub fn source_of(p: &MonitoredPosition) -> Source {
    if p.manual {
        return Source { kind: "scalp", title: "Scalp".to_string() };
    }
    match &p.signal.chat {
        Some(chat) => Source {
            kind: "telegram",
            title: chat
                .title
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("Chat {}", chat.id)),
        },
        None if p.signal.source.starts_with("telegram") => Source { kind: "telegram", title: UNKNOWN_CHANNEL.to_string() },
        None => Source { kind: "other", title: UNKNOWN_CHANNEL.to_string() },
    }
}

/// What the route knows about a position beyond the position itself.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub ltp: Option<f64>,
    /// The underlying index's value, for chart levels set on the index.
    pub index_ltp: Option<f64>,
    /// Lots a waiting call buys when no lots are set on it.
    pub default_lots: i32,
    /// IST `YYYY-MM-DD HH:MM:SS` of the call's first buy.
    pub first_buy_at: Option<String>,
    pub zone: Option<(f64, f64)>,
    pub valid_till: Option<String>,
}

/// One open or waiting position, as a Desk row shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeskPosition {
    pub id: String,
    pub contract: String,
    pub index: String,
    pub strike: Option<f64>,
    pub side: Option<String>,
    pub source: Source,
    /// `waiting`, `active` or `target1`.
    pub state: &'static str,
    pub qty: i32,
    pub lot_size: i32,
    /// Lots a waiting call will buy.
    pub lots: i32,
    /// Whether those lots were set by hand.
    pub lots_set: bool,
    pub avg: f64,
    /// IST `HH:MM`: the first buy, or when a waiting call arrived.
    pub since: Option<String>,
    pub ltp: Option<f64>,
    pub pnl: Option<f64>,
    pub pnl_pct: Option<f64>,
    pub entry: f64,
    pub entry_max: Option<f64>,
    pub entry_condition: String,
    pub zone: Option<(f64, f64)>,
    pub valid_till: Option<String>,
    /// The live stop.
    pub stop: f64,
    /// The stop the call gave.
    pub signal_stop: f64,
    pub targets: Vec<f64>,
    /// A dynamic-targeting runner's next rung.
    pub next_target: Option<f64>,
    pub chart: ChartLevels,
    pub index_ltp: Option<f64>,
    /// Why −1 lot and Exit wait: `halted`, `exiting`, `selling`, `buying`.
    pub busy: Option<&'static str>,
}

/// IST `HH:MM` from a `YYYY-MM-DD HH:MM:SS` timestamp.
fn hh_mm(ts: &str) -> Option<String> {
    ts.get(11..16).map(str::to_string)
}

impl DeskPosition {
    pub fn of(p: &MonitoredPosition, cx: &Context) -> DeskPosition {
        let state = match p.state {
            TradeState::WaitingForEntry => "waiting",
            TradeState::Target1Hit => "target1",
            _ => "active",
        };
        let held = !matches!(p.state, TradeState::WaitingForEntry) && p.executed_qty > 0;
        let ltp = cx.ltp.filter(|v| *v > 0.0);
        let pnl = ltp.filter(|_| held).map(|ltp| (ltp - p.avg_buy_price) * f64::from(p.executed_qty));
        let pnl_pct = ltp.filter(|_| held && p.avg_buy_price > 0.0).map(|ltp| (ltp - p.avg_buy_price) / p.avg_buy_price * 100.0);
        let busy = if p.live_halt.is_some() {
            Some("halted")
        } else if p.force_exit.is_some() {
            Some("exiting")
        } else if p.pending_exit_order_id.is_some() || p.manual_sell_qty.is_some() {
            Some("selling")
        } else if p.buy_in_flight() {
            Some("buying")
        } else {
            None
        };
        let since = if held { cx.first_buy_at.as_deref().and_then(hh_mm) } else { hh_mm(&p.created_at) };
        DeskPosition {
            id: p.id.clone(),
            contract: contract_label(&p.signal.instrument_name, p.signal.strike, p.signal.option_type.as_deref()),
            index: p.signal.instrument_name.clone(),
            strike: p.signal.strike,
            side: p.signal.option_type.clone(),
            source: source_of(p),
            state,
            qty: p.executed_qty,
            lot_size: p.resolved_order.as_ref().and_then(|o| o.quantity.parse().ok()).filter(|v: &i32| *v > 0).unwrap_or(1),
            lots: p.override_qty.unwrap_or(cx.default_lots).max(0),
            lots_set: p.override_qty.is_some(),
            avg: p.avg_buy_price,
            since,
            ltp,
            pnl,
            pnl_pct,
            entry: p.signal.entry_price,
            entry_max: p.signal.entry_price_max,
            entry_condition: p.signal.entry_condition.clone(),
            zone: cx.zone,
            valid_till: cx.valid_till.clone(),
            stop: p.current_sl,
            signal_stop: p.signal.stop_loss,
            targets: p.signal.targets.clone(),
            next_target: p.next_dynamic_target,
            chart: p.chart.clone(),
            index_ltp: cx.index_ltp.filter(|v| *v > 0.0),
            busy,
        }
    }
}

/// The Desk's figures for today.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeskKpis {
    /// Booked plus open.
    pub net: f64,
    pub booked: f64,
    pub open: f64,
    /// Calls taken today: open, waiting and closed.
    pub calls: usize,
}

pub fn kpis(booked: f64, open: &[DeskPosition], waiting: usize, closed: usize) -> DeskKpis {
    // `+ 0.0` turns the −0.0 an empty sum gives into 0.0, so nothing reads "−₹0".
    let open_pnl = open.iter().filter_map(|p| p.pnl).sum::<f64>() + 0.0;
    let booked = booked + 0.0;
    DeskKpis { net: booked + open_pnl, booked, open: open_pnl, calls: open.len() + waiting + closed }
}

/// One stored Telegram message — see `DbWriteMessage::TelegramMessage`.
#[derive(Debug, Clone, Default, sqlx::FromRow)]
pub struct FeedRow {
    pub id: i64,
    pub chat_id: i64,
    pub chat_title: Option<String>,
    pub msg_id: i64,
    pub sent_at: String,
    pub text: String,
    pub kind: String,
    pub reply_to: Option<i64>,
    pub value: Option<f64>,
    pub edited_at: Option<String>,
}

/// What became of a message.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FeedStatus {
    /// Its call is waiting for its entry.
    Waiting { valid_till: Option<String> },
    /// Its call holds `qty`, bought at `price` on average.
    Bought { qty: i64, price: f64 },
    /// Its call is fully sold.
    Closed { outcome: Option<Outcome>, pnl: Option<f64> },
    /// A call the engine didn't take — the reason is in the engine log.
    NotTaken,
    /// A call that isn't an index option (`index_options_only`).
    Skipped,
    /// Not a call; `no_instrument` when it read like one.
    NotACall { no_instrument: bool },
    /// A reply moving its call's stop.
    StopUpdate { price: Option<f64> },
    /// A reply exiting its call.
    Exit { price: Option<f64> },
}

/// What became of `row`, from the open positions and today's calls.
/// `valid_till` is when a waiting call stops being valid.
pub fn feed_status(
    row: &FeedRow,
    positions: &[MonitoredPosition],
    calls: &[Call],
    valid_till: impl Fn(&MonitoredPosition) -> Option<String>,
) -> FeedStatus {
    match row.kind.as_str() {
        "signal" => {}
        "not_index_option" => return FeedStatus::Skipped,
        "ignored_call" => return FeedStatus::NotACall { no_instrument: true },
        "sl_update" => return FeedStatus::StopUpdate { price: row.value },
        "exit" => return FeedStatus::Exit { price: row.value },
        _ => return FeedStatus::NotACall { no_instrument: false },
    }
    let id = row.msg_id.to_string();
    let open = positions.iter().find(|p| {
        !matches!(p.state, TradeState::Closed)
            && !p.manual
            && p.signal.chat.as_ref().map(|c| c.id) == Some(row.chat_id)
            && p.signal.signal_id.as_deref() == Some(id.as_str())
    });
    if let Some(p) = open {
        return match p.state {
            TradeState::WaitingForEntry => FeedStatus::Waiting { valid_till: valid_till(p) },
            _ => FeedStatus::Bought { qty: i64::from(p.executed_qty), price: p.avg_buy_price },
        };
    }
    let call = calls.iter().find(|c| c.chat_id == Some(row.chat_id) && c.signal_id.as_deref() == Some(id.as_str()));
    match call {
        Some(c) if c.closed_at.is_some() => FeedStatus::Closed { outcome: outcome(c), pnl: c.pnl },
        Some(c) if c.bought_qty > c.sold_qty => {
            let (qty, value) = c.legs.iter().filter(|l| is_buy(l)).fold((0_i64, 0.0), |(q, v), l| (q + l.qty, v + l.price * l.qty as f64));
            FeedStatus::Bought { qty: c.bought_qty - c.sold_qty, price: if qty > 0 { value / qty as f64 } else { 0.0 } }
        }
        _ => FeedStatus::NotTaken,
    }
}

/// One message in the feed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FeedItem {
    pub id: i64,
    pub chat_id: i64,
    pub channel: String,
    pub msg_id: i64,
    /// IST `YYYY-MM-DD HH:MM:SS`.
    pub sent_at: String,
    pub text: String,
    pub edited: bool,
    /// The message a reply command answers.
    pub reply_to: Option<i64>,
    pub status: FeedStatus,
}

impl FeedItem {
    pub fn of(row: FeedRow, status: FeedStatus) -> FeedItem {
        FeedItem {
            channel: row
                .chat_title
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("Chat {}", row.chat_id)),
            id: row.id,
            chat_id: row.chat_id,
            msg_id: row.msg_id,
            sent_at: row.sent_at,
            text: row.text,
            edited: row.edited_at.is_some(),
            reply_to: row.reply_to,
            status,
        }
    }
}

/// A status-bar dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Down,
    /// Not applicable right now: not configured, or the market is closed.
    Off,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Health {
    pub level: Level,
    pub label: String,
}

fn health(level: Level, label: impl Into<String>) -> Health {
    Health { level, label: label.into() }
}

/// The five dots.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HealthSet {
    pub engine: Health,
    pub feed: Health,
    pub kotak: Health,
    pub telegram: Health,
    pub upstox: Health,
}

/// What the dots are read from — the System screen's latest sample.
#[derive(Debug, Clone, Default)]
pub struct HealthInputs {
    pub engine_heartbeat_age_secs: Option<f64>,
    pub feed_connected: bool,
    pub feed_subscriptions: i64,
    pub feed_last_tick_age_secs: Option<f64>,
    pub kotak_connected: bool,
    pub kotak_session_healthy: bool,
    pub telegram_connected: bool,
    pub upstox_configured: bool,
    pub upstox_last_success_age_secs: Option<f64>,
}

/// The dots, by the System screen's rules: a quiet feed or Upstox only
/// counts while the market is open.
pub fn health_set(s: &HealthInputs, market_hours: bool) -> HealthSet {
    let engine = match s.engine_heartbeat_age_secs {
        Some(age) if age <= 2.0 => health(Level::Ok, "Running"),
        Some(_) => health(Level::Down, "Stuck"),
        None => health(Level::Down, "No ticks"),
    };
    let feed = if !market_hours {
        health(Level::Off, "Market closed")
    } else if !s.feed_connected {
        health(Level::Down, "Disconnected")
    } else if s.feed_subscriptions == 0 {
        health(Level::Off, "Nothing subscribed")
    } else {
        match s.feed_last_tick_age_secs {
            Some(age) if age <= 10.0 => health(Level::Ok, "Live"),
            Some(age) => health(Level::Down, format!("No ticks for {age:.0} s")),
            None => health(Level::Down, "No ticks yet"),
        }
    };
    let kotak = if s.kotak_session_healthy {
        health(Level::Ok, "Logged in")
    } else if s.kotak_connected {
        health(Level::Down, "Session failing")
    } else if market_hours {
        health(Level::Down, "Not logged in")
    } else {
        health(Level::Off, "Not logged in")
    };
    let telegram = if s.telegram_connected { health(Level::Ok, "Listening") } else { health(Level::Down, "Not running") };
    let upstox = if !s.upstox_configured {
        health(Level::Off, "Not configured")
    } else if !market_hours {
        health(Level::Off, "Market closed")
    } else {
        match s.upstox_last_success_age_secs {
            Some(age) if age <= 180.0 => health(Level::Ok, "Updating"),
            Some(age) => health(Level::Down, format!("Last data {:.0} min ago", age / 60.0)),
            None => health(Level::Down, "No data yet"),
        }
    };
    HealthSet { engine, feed, kotak, telegram, upstox }
}

/// Whether the market is open, and the status bar's line about it: open
/// 09:15–15:30 IST on a trading day.
pub fn market_phase(now: DateTime<FixedOffset>, trading_day: bool) -> (bool, &'static str) {
    let minutes = now.hour() * 60 + now.minute();
    if !trading_day {
        (false, "Market closed")
    } else if minutes < 9 * 60 + 15 {
        (false, "Market opens 09:15")
    } else if minutes < 15 * 60 + 30 {
        (true, "Market open · closes 15:30")
    } else {
        (false, "Market closed")
    }
}

/// When the kill switch stopped trading, and what it did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stopped {
    /// IST `HH:MM`.
    pub at: Option<String>,
    pub positions_exited: i64,
    pub entries_cancelled: i64,
}

/// `Stopped` from the last `KILL_SWITCH_ENGAGED` log line and its timestamp.
pub fn stopped_from_log(message: &str, timestamp: &str) -> Stopped {
    let v: serde_json::Value = serde_json::from_str(message).unwrap_or_default();
    Stopped {
        at: hh_mm(timestamp),
        positions_exited: v["liquidated_positions"].as_i64().unwrap_or(0),
        entries_cancelled: v["abandoned_entries"].as_i64().unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{self, Leg};
    use shared_domain::{SignalChat, TradeSignal};

    fn leg(id: i64, t: &str, action: &str, qty: i64, net: f64, signal: &str, reason: Option<&str>) -> Leg {
        Leg {
            id,
            ticker: "NIFTY".into(),
            action: action.into(),
            qty,
            executed_price: net / qty as f64,
            timestamp: t.into(),
            net_value: net,
            charges: 20.0,
            signal_id: Some(signal.into()),
            raw_message: None,
            exit_reason: reason.map(Into::into),
            chat_id: Some(-100),
            chat_title: Some("Options Pro".into()),
            option_type: Some("CE".into()),
            strike: Some(25000.0),
        }
    }

    /// Call 1 bought yesterday, half sold yesterday at T1 and the rest today
    /// on its trailed stop; call 2 bought and half sold today, still open.
    fn two_calls() -> Vec<Call> {
        history::calls(vec![
            leg(1, "2026-10-20 10:00:00", "BUY", 130, 13_000.0, "11", None),
            leg(2, "2026-10-20 10:30:00", "SELL", 65, 7_500.0, "11", Some("TGT1_PARTIAL")),
            leg(3, "2026-10-21 09:30:00", "SELL", 65, 6_600.0, "11", Some("TRAIL_SL_HIT")),
            leg(4, "2026-10-21 10:00:00", "BUY", 130, 15_600.0, "12", None),
            leg(5, "2026-10-21 10:20:00", "SELL", 65, 9_100.0, "12", Some("TGT1_PARTIAL")),
        ])
    }

    #[test]
    fn booked_counts_each_sell_on_its_own_day() {
        let calls = two_calls();
        // Today: 6,600 − 65 × 100 on call 1, and 9,100 − 65 × 120 on call 2.
        assert!((booked_on(&calls, "2026-10-21") - (100.0 + 1_300.0)).abs() < 1e-9);
        assert!((booked_on(&calls, "2026-10-20") - 1_000.0).abs() < 1e-9);
        assert_eq!(booked_on(&calls, "2026-10-19"), 0.0);
    }

    #[test]
    fn outcomes_read_the_way_the_call_ended() {
        let calls = two_calls();
        assert_eq!(outcome(&calls[0]), Some(Outcome { label: "T1 hit · stop trailed".into(), tone: Tone::Profit }));
        let ended = |reasons: &[&str], pnl: f64| {
            let mut legs = vec![leg(1, "2026-10-21 10:00:00", "BUY", 65, 6_500.0, "1", None)];
            for (i, r) in reasons.iter().enumerate() {
                legs.push(leg(2 + i as i64, "2026-10-21 10:30:00", "SELL", 65, 6_500.0 + pnl, "1", Some(r)));
            }
            outcome(&history::calls(legs)[0]).unwrap()
        };
        assert_eq!(ended(&["TGT2_HIT"], 500.0).label, "T2 hit");
        assert_eq!(ended(&["SL_HIT"], -500.0), Outcome { label: "SL hit".into(), tone: Tone::Loss });
        assert_eq!(ended(&["OPPOSITE_SIGNAL_EXIT"], -50.0).label, "Opposite call");
        assert_eq!(ended(&["CLOSED_VIA_FRONTEND"], 50.0).label, "Manual exit");
        assert_eq!(ended(&["EXIT_AT_130.00"], 50.0).label, "Telegram exit");
        assert_eq!(ended(&["CHART_STOP"], 80.0).tone, Tone::Profit, "a stop that locked in a profit");
        assert_eq!(ended(&["SOME_NEW_REASON"], 0.0).label, "Some new reason");
    }

    #[test]
    fn fills_after_a_cursor_are_todays_newest_with_their_pnl() {
        let calls = two_calls();
        assert_eq!(last_leg_id(&calls), 5);
        let fills = fills_after(&calls, "2026-10-21", 2, 10);
        assert_eq!(fills.iter().map(|f| f.id).collect::<Vec<_>>(), [3, 4, 5]);
        assert_eq!((fills[1].action.as_str(), fills[1].contract.as_str(), fills[1].pnl), ("BUY", "NIFTY 25000 CE", None));
        assert!((fills[2].pnl.unwrap() - 1_300.0).abs() < 1e-9);
        assert_eq!(fills_after(&calls, "2026-10-21", 2, 1).iter().map(|f| f.id).collect::<Vec<_>>(), [5]);
        assert!(fills_after(&calls, "2026-10-21", 5, 10).is_empty());
    }

    fn position(state: TradeState, signal_id: &str, chat: i64) -> MonitoredPosition {
        MonitoredPosition {
            id: format!("p{signal_id}"),
            signal: TradeSignal {
                instrument_name: "NIFTY".into(),
                strike: Some(25000.0),
                option_type: Some("CE".into()),
                expiry: None,
                action: "BUY".into(),
                entry_condition: "ABOVE".into(),
                entry_price: 150.0,
                entry_price_max: None,
                targets: vec![170.0, 190.0],
                stop_loss: 128.0,
                source: "telegram".into(),
                signal_id: Some(signal_id.into()),
                raw_message: None,
                chat: Some(SignalChat { id: chat, title: Some(" Options Pro ".into()) }),
            },
            state,
            created_at: "2026-10-21 11:01:30".into(),
            current_sl: 128.0,
            executed_qty: 130,
            avg_buy_price: 150.0,
            ..Default::default()
        }
    }

    #[test]
    fn a_desk_row_carries_its_pnl_source_and_times() {
        let p = position(TradeState::Active, "12", -100);
        let row = DeskPosition::of(&p, &Context {
            ltp: Some(161.0),
            default_lots: 2,
            first_buy_at: Some("2026-10-21 11:02:05".into()),
            ..Default::default()
        });
        assert_eq!((row.contract.as_str(), row.state, row.since.as_deref()), ("NIFTY 25000 CE", "active", Some("11:02")));
        assert_eq!(row.source, Source { kind: "telegram", title: "Options Pro".into() });
        assert!((row.pnl.unwrap() - 1_430.0).abs() < 1e-9);
        assert!((row.pnl_pct.unwrap() - 11.0 / 1.5).abs() < 1e-9);
        assert_eq!(row.busy, None);

        let mut waiting = position(TradeState::WaitingForEntry, "13", -100);
        waiting.executed_qty = 0;
        waiting.avg_buy_price = 0.0;
        let row = DeskPosition::of(&waiting, &Context { ltp: Some(140.0), default_lots: 2, ..Default::default() });
        assert_eq!((row.pnl, row.lots, row.lots_set, row.since.as_deref()), (None, 2, false, Some("11:01")));
        waiting.override_qty = Some(3);
        waiting.force_exit = Some("KILL_SWITCH".into());
        let row = DeskPosition::of(&waiting, &Context::default());
        assert_eq!((row.lots, row.lots_set, row.busy), (3, true, Some("exiting")));

        let k = kpis(500.0, &[DeskPosition::of(&p, &Context { ltp: Some(161.0), ..Default::default() })], 1, 6);
        assert_eq!(k, DeskKpis { net: 1_930.0, booked: 500.0, open: 1_430.0, calls: 8 });
        let none = kpis(-0.0, &[], 0, 0);
        assert!(none.open.is_sign_positive() && none.booked.is_sign_positive());
    }

    #[test]
    fn a_message_shows_what_became_of_its_call() {
        let calls = two_calls();
        let row = |msg_id: i64, kind: &str| FeedRow { chat_id: -100, msg_id, kind: kind.into(), value: Some(110.0), ..Default::default() };
        let till = |_: &MonitoredPosition| Some("12:10".to_string());
        let positions = vec![position(TradeState::Active, "12", -100), position(TradeState::WaitingForEntry, "13", -100)];

        assert_eq!(feed_status(&row(12, "signal"), &positions, &calls, till), FeedStatus::Bought { qty: 130, price: 150.0 });
        assert_eq!(feed_status(&row(13, "signal"), &positions, &calls, till), FeedStatus::Waiting { valid_till: Some("12:10".into()) });
        match feed_status(&row(11, "signal"), &positions, &calls, till) {
            FeedStatus::Closed { outcome, pnl } => {
                assert_eq!(outcome.unwrap().label, "T1 hit · stop trailed");
                assert!((pnl.unwrap() - 1_100.0).abs() < 1e-9);
            }
            other => panic!("expected closed, got {other:?}"),
        }
        assert_eq!(feed_status(&row(14, "signal"), &positions, &calls, till), FeedStatus::NotTaken);
        // Message 12 of another chat is another call.
        let other_chat = FeedRow { chat_id: -200, ..row(12, "signal") };
        assert_eq!(feed_status(&other_chat, &positions, &calls, till), FeedStatus::NotTaken);
        assert_eq!(feed_status(&row(15, "not_index_option"), &positions, &calls, till), FeedStatus::Skipped);
        assert_eq!(feed_status(&row(15, "ignored_call"), &positions, &calls, till), FeedStatus::NotACall { no_instrument: true });
        assert_eq!(feed_status(&row(15, "unparsed"), &positions, &calls, till), FeedStatus::NotACall { no_instrument: false });
        assert_eq!(feed_status(&row(15, "sl_update"), &positions, &calls, till), FeedStatus::StopUpdate { price: Some(110.0) });

        let item = FeedItem::of(FeedRow { chat_title: None, edited_at: Some("x".into()), ..row(12, "signal") }, FeedStatus::NotTaken);
        assert_eq!((item.channel.as_str(), item.edited), ("Chat -100", true));
    }

    #[test]
    fn dots_follow_the_system_screens_rules() {
        let all_well = HealthInputs {
            engine_heartbeat_age_secs: Some(0.1),
            feed_connected: true,
            feed_subscriptions: 4,
            feed_last_tick_age_secs: Some(1.0),
            kotak_connected: true,
            kotak_session_healthy: true,
            telegram_connected: true,
            upstox_configured: true,
            upstox_last_success_age_secs: Some(30.0),
        };
        let open = health_set(&all_well, true);
        assert!([&open.engine, &open.feed, &open.kotak, &open.telegram, &open.upstox].iter().all(|h| h.level == Level::Ok));

        let quiet = HealthInputs { feed_last_tick_age_secs: Some(45.0), kotak_connected: false, kotak_session_healthy: false, ..all_well.clone() };
        assert_eq!(health_set(&quiet, true).feed.level, Level::Down);
        assert_eq!(health_set(&quiet, true).kotak.level, Level::Down);
        // After the close neither counts against the app.
        assert_eq!(health_set(&quiet, false).feed.level, Level::Off);
        assert_eq!(health_set(&quiet, false).kotak.level, Level::Off);
        assert_eq!(health_set(&HealthInputs { engine_heartbeat_age_secs: None, ..all_well }, true).engine.level, Level::Down);
    }

    #[test]
    fn the_market_line_follows_the_clock() {
        let at = |h, m| {
            chrono::NaiveDate::from_ymd_opt(2026, 10, 21)
                .unwrap()
                .and_hms_opt(h, m, 0)
                .unwrap()
                .and_local_timezone(shared_domain::ist_offset())
                .unwrap()
        };
        assert_eq!(market_phase(at(9, 0), true), (false, "Market opens 09:15"));
        assert_eq!(market_phase(at(9, 15), true), (true, "Market open · closes 15:30"));
        assert_eq!(market_phase(at(15, 30), true), (false, "Market closed"));
        assert_eq!(market_phase(at(11, 0), false), (false, "Market closed"));
        let stopped = stopped_from_log(r#"{"event":"KILL_SWITCH_ENGAGED","liquidated_positions":3,"abandoned_entries":1}"#, "2026-10-21 11:52:07");
        assert_eq!(stopped, Stopped { at: Some("11:52".into()), positions_exited: 3, entries_cancelled: 1 });
    }
}
