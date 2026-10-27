//! Trade history for the v2 Desk (`/api/v2/history`): trade rows grouped into
//! calls — one per Telegram call or scalp — and the KPIs, P&L curve, grouped
//! P&L and trading days over the calls closed in a range. Read-only.
//!
//! A call's P&L is its sells' net proceeds less the same quantity at the
//! call's average buy cost, charges included on both sides — the rule v1's
//! portfolio applies per leg. A call counts once it is fully sold, on the day
//! of its last sell, so every figure for a range covers the same calls.

use std::collections::{BTreeMap, HashMap};

use chrono::{Days, NaiveDate};
use serde::Serialize;
use shared_domain::INDEX_NAMES;
use telegram_ingester::parser::expires_on;

/// Channel shown for a call with no Telegram chat recorded: anything written
/// before chats were kept, the webhook, and positions adopted from the broker.
pub const UNKNOWN_CHANNEL: &str = "Unknown channel";
/// Channel shown for the Execute screen's own scalps.
pub const SCALPS: &str = "Scalps";
/// Most calls listed in one answer; the KPIs always cover all of them.
const MAX_LISTED_CALLS: usize = 300;

/// One trade row, as the history reads it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Leg {
    pub id: i64,
    pub ticker: String,
    pub action: String,
    pub qty: i64,
    pub executed_price: f64,
    /// IST `YYYY-MM-DD HH:MM:SS`.
    pub timestamp: String,
    pub net_value: f64,
    /// Every charge on the leg, summed.
    pub charges: f64,
    pub signal_id: Option<String>,
    pub raw_message: Option<String>,
    pub exit_reason: Option<String>,
    pub chat_id: Option<i64>,
    pub chat_title: Option<String>,
    pub option_type: Option<String>,
    pub strike: Option<f64>,
}

impl Leg {
    fn is_buy(&self) -> bool {
        self.action.eq_ignore_ascii_case("BUY")
    }

    fn day(&self) -> &str {
        self.timestamp.get(..10).unwrap_or(&self.timestamp)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LegRow {
    /// The trade row's id: newer legs have larger ids.
    pub id: i64,
    pub t: String,
    pub action: String,
    pub qty: i64,
    pub price: f64,
    pub net: f64,
    pub charges: f64,
    pub reason: Option<String>,
}

/// One Telegram call or scalp, with its legs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Call {
    pub key: String,
    pub channel: String,
    pub chat_id: Option<i64>,
    /// The Telegram message it came from (`manual-…` for a scalp).
    pub signal_id: Option<String>,
    pub index: String,
    /// `CE` or `PE`, when known.
    pub side: Option<String>,
    pub strike: Option<f64>,
    pub opened_at: String,
    /// The last sell, once everything bought has been sold.
    pub closed_at: Option<String>,
    pub bought_qty: i64,
    pub sold_qty: i64,
    /// Realized P&L, charges included. `None` without a buy to cost it against
    /// (a position adopted from the broker).
    pub pnl: Option<f64>,
    pub charges: f64,
    pub exit_reason: Option<String>,
    /// The call's original message.
    pub message: Option<String>,
    pub legs: Vec<LegRow>,
    /// How it ended, once sold — see `desk::outcome`.
    pub outcome: Option<crate::desk::Outcome>,
}

impl Call {
    fn closed_on(&self) -> Option<NaiveDate> {
        let day = self.closed_at.as_deref()?.get(..10)?;
        NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()
    }
}

/// Which call a leg belongs to: its chat, signal id and index — or, for a row
/// with no signal id, its index and day.
fn call_key(leg: &Leg) -> String {
    match &leg.signal_id {
        Some(signal) => format!("{}|{signal}|{}", leg.chat_id.map(|c| c.to_string()).unwrap_or_default(), leg.ticker),
        None => format!("|{}|{}", leg.day(), leg.ticker),
    }
}

/// `CE` or `PE` named in a call's message (`25000 CE`, `22300 CALLS`) — for
/// rows written before the side was stored.
fn side_in(message: &str) -> Option<&'static str> {
    message
        .split(|c: char| !c.is_ascii_alphanumeric())
        .find_map(|word| match word.to_ascii_uppercase().as_str() {
            "CE" | "CALL" | "CALLS" => Some("CE"),
            "PE" | "PUT" | "PUTS" => Some("PE"),
            _ => None,
        })
}

fn channel_of(leg: &Leg) -> String {
    match (&leg.chat_title, leg.chat_id) {
        (Some(title), _) if !title.trim().is_empty() => title.trim().to_string(),
        (_, Some(id)) => format!("Chat {id}"),
        _ if leg.signal_id.as_deref().is_some_and(|s| s.starts_with("manual-")) => SCALPS.to_string(),
        _ => UNKNOWN_CHANNEL.to_string(),
    }
}

/// Every trade row of `mode` (`PAPER` or `LIVE`), oldest first.
pub async fn load_legs(pool: &sqlx::SqlitePool, mode: &str) -> Result<Vec<Leg>, sqlx::Error> {
    sqlx::query_as::<_, Leg>(
        "SELECT id, ticker, action, qty, executed_price, timestamp, net_value,
                brokerage + stt_charge + sebi_fee + stamp_duty + transaction_charge + gst AS charges,
                signal_id, raw_message, exit_reason, chat_id, chat_title, option_type, strike
         FROM paper_trades WHERE mode = ? ORDER BY id ASC",
    )
    .bind(mode)
    .fetch_all(pool)
    .await
}

/// A user's paper fills since their last reset (`epoch`), oldest first, read
/// as the owner's scalps are: one call per position, under "Scalps".
pub async fn load_user_legs(pool: &sqlx::SqlitePool, user_id: i64, epoch: i64) -> Result<Vec<Leg>, sqlx::Error> {
    sqlx::query_as::<_, Leg>(
        "SELECT id, ticker, action, qty, executed_price, timestamp, net_value,
                brokerage + stt_charge + sebi_fee + stamp_duty + transaction_charge + gst AS charges,
                'manual-' || position_id AS signal_id, label AS raw_message, exit_reason,
                NULL AS chat_id, NULL AS chat_title, option_type, strike
         FROM user_trades WHERE user_id = ? AND epoch = ? ORDER BY id ASC",
    )
    .bind(user_id)
    .bind(epoch)
    .fetch_all(pool)
    .await
}

/// Group trade rows into calls, in the order each call first appears.
pub fn calls(legs: Vec<Leg>) -> Vec<Call> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<Leg>> = HashMap::new();
    for leg in legs {
        let key = call_key(&leg);
        if !groups.contains_key(&key) {
            order.push(key.clone());
        }
        groups.entry(key).or_default().push(leg);
    }

    order
        .into_iter()
        .filter_map(|key| {
            let mut legs = groups.remove(&key)?;
            legs.sort_by(|a, b| (a.timestamp.as_str(), a.id).cmp(&(b.timestamp.as_str(), b.id)));
            let first = legs.first()?;
            let (bought_qty, bought_net) = legs.iter().filter(|l| l.is_buy()).fold((0, 0.0), |(q, n), l| (q + l.qty, n + l.net_value));
            let avg_cost = (bought_qty > 0).then(|| bought_net / bought_qty as f64);
            let sells: Vec<&Leg> = legs.iter().filter(|l| !l.is_buy()).collect();
            let sold_qty: i64 = sells.iter().map(|l| l.qty).sum();
            let pnl = avg_cost.map(|cost| sells.iter().map(|l| l.net_value - l.qty as f64 * cost).sum());
            let closed_at = (bought_qty > 0 && sold_qty >= bought_qty)
                .then(|| sells.last().map(|l| l.timestamp.clone()))
                .flatten();
            let message = legs.iter().find_map(|l| l.raw_message.clone());
            let side = legs
                .iter()
                .find_map(|l| l.option_type.clone())
                .or_else(|| message.as_deref().and_then(side_in).map(str::to_string));
            let mut call = Call {
                channel: channel_of(first),
                chat_id: first.chat_id,
                signal_id: first.signal_id.clone(),
                index: first.ticker.clone(),
                side,
                strike: legs.iter().find_map(|l| l.strike),
                opened_at: first.timestamp.clone(),
                closed_at,
                bought_qty,
                sold_qty,
                pnl,
                charges: legs.iter().map(|l| l.charges).sum(),
                exit_reason: sells.last().and_then(|l| l.exit_reason.clone()),
                message,
                legs: legs
                    .iter()
                    .map(|l| LegRow {
                        id: l.id,
                        t: l.timestamp.clone(),
                        action: l.action.clone(),
                        qty: l.qty,
                        price: l.executed_price,
                        net: l.net_value,
                        charges: l.charges,
                        reason: l.exit_reason.clone(),
                    })
                    .collect(),
                key,
                outcome: None,
            };
            call.outcome = crate::desk::outcome(&call);
            Some(call)
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    Today,
    /// The last `n` IST days, today included.
    Days(u64),
    All,
}

impl Range {
    pub fn parse(raw: &str) -> Option<Range> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "today" => Some(Range::Today),
            "7d" => Some(Range::Days(7)),
            "30d" => Some(Range::Days(30)),
            "all" => Some(Range::All),
            _ => None,
        }
    }

    /// First IST day of the range, `None` for all of it.
    fn from(self, today: NaiveDate) -> Option<NaiveDate> {
        match self {
            Range::Today => Some(today),
            Range::Days(n) => today.checked_sub_days(Days::new(n.saturating_sub(1))),
            Range::All => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    Channel,
    Index,
    Side,
}

impl GroupBy {
    pub fn parse(raw: &str) -> Option<GroupBy> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "channel" => Some(GroupBy::Channel),
            "index" => Some(GroupBy::Index),
            "cepe" => Some(GroupBy::Side),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            GroupBy::Channel => "channel",
            GroupBy::Index => "index",
            GroupBy::Side => "cepe",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Kpis {
    pub net: f64,
    pub calls: usize,
    pub wins: usize,
    pub losses: usize,
    pub win_rate: Option<f64>,
    pub profit_factor: Option<f64>,
    pub avg_per_call: Option<f64>,
    pub max_drawdown: f64,
    pub gross_profit: f64,
    pub gross_loss: f64,
    pub charges: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CurvePoint {
    /// When the call closed.
    pub t: String,
    pub pnl: f64,
    pub peak: f64,
    pub drawdown: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GroupRow {
    pub label: String,
    pub calls: usize,
    pub wins: usize,
    pub net: f64,
    pub gross_profit: f64,
    pub gross_loss: f64,
    /// Gross profit over gross loss; `None` without a losing call.
    pub profit_factor: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DayRow {
    pub date: String,
    pub calls: usize,
    pub wins: usize,
    pub net: f64,
    /// The indices whose options expired that day.
    pub expiries: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub from: Option<String>,
    pub to: String,
    pub kpis: Kpis,
    pub curve: Vec<CurvePoint>,
    pub group: &'static str,
    pub groups: Vec<GroupRow>,
    pub days: Vec<DayRow>,
    /// The range's calls, newest first.
    pub calls: Vec<Call>,
    /// Calls in the range left out of `calls`.
    pub more_calls: usize,
}

/// The KPIs, curve, grouped P&L and days over `calls` closed in `range`.
pub fn report(calls: &[Call], today: NaiveDate, range: Range, group: GroupBy) -> Report {
    let from = range.from(today);
    let mut closed: Vec<(&Call, f64)> = calls
        .iter()
        .filter_map(|c| {
            let day = c.closed_on()?;
            let in_range = from.is_none_or(|f| day >= f) && day <= today;
            in_range.then_some((c, c.pnl?))
        })
        .collect();
    closed.sort_by(|a, b| a.0.closed_at.cmp(&b.0.closed_at));

    let mut curve = Vec::with_capacity(closed.len());
    let (mut running, mut peak, mut max_drawdown) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (call, pnl) in &closed {
        running += pnl;
        peak = peak.max(running);
        max_drawdown = max_drawdown.max(peak - running);
        curve.push(CurvePoint {
            t: call.closed_at.clone().unwrap_or_default(),
            pnl: running,
            peak,
            drawdown: peak - running,
        });
    }

    let wins = closed.iter().filter(|(_, p)| *p > 0.0).count();
    let losses = closed.iter().filter(|(_, p)| *p < 0.0).count();
    let gross_profit: f64 = closed.iter().map(|(_, p)| p.max(0.0)).sum();
    let gross_loss: f64 = closed.iter().map(|(_, p)| (-p).max(0.0)).sum();
    let count = closed.len();
    let kpis = Kpis {
        net: running,
        calls: count,
        wins,
        losses,
        win_rate: (count > 0).then(|| wins as f64 / count as f64 * 100.0),
        profit_factor: (gross_loss > 0.0).then(|| gross_profit / gross_loss),
        avg_per_call: (count > 0).then(|| running / count as f64),
        max_drawdown,
        gross_profit,
        gross_loss,
        charges: closed.iter().map(|(c, _)| c.charges).sum(),
    };

    let mut groups: BTreeMap<String, GroupRow> = BTreeMap::new();
    let mut days: BTreeMap<String, DayRow> = BTreeMap::new();
    for (call, pnl) in &closed {
        let label = match group {
            GroupBy::Channel => call.channel.clone(),
            GroupBy::Index => call.index.clone(),
            GroupBy::Side => call.side.clone().unwrap_or_else(|| "Other".to_string()),
        };
        let row = groups.entry(label.clone()).or_insert(GroupRow {
            label,
            calls: 0,
            wins: 0,
            net: 0.0,
            gross_profit: 0.0,
            gross_loss: 0.0,
            profit_factor: None,
        });
        row.calls += 1;
        row.wins += usize::from(*pnl > 0.0);
        row.net += pnl;
        row.gross_profit += pnl.max(0.0);
        row.gross_loss += (-pnl).max(0.0);
        row.profit_factor = (row.gross_loss > 0.0).then(|| row.gross_profit / row.gross_loss);

        let date = call.closed_at.as_deref().and_then(|t| t.get(..10)).unwrap_or_default().to_string();
        let day = days.entry(date.clone()).or_insert_with(|| DayRow {
            expiries: NaiveDate::parse_from_str(&date, "%Y-%m-%d")
                .map(|d| INDEX_NAMES.iter().copied().filter(|i| expires_on(i, d)).collect())
                .unwrap_or_default(),
            date,
            calls: 0,
            wins: 0,
            net: 0.0,
        });
        day.calls += 1;
        day.wins += usize::from(*pnl > 0.0);
        day.net += pnl;
    }
    let mut groups: Vec<GroupRow> = groups.into_values().collect();
    groups.sort_by(|a, b| b.net.total_cmp(&a.net));

    let listed: Vec<Call> = closed.iter().rev().take(MAX_LISTED_CALLS).map(|(c, _)| (*c).clone()).collect();
    Report {
        from: from.map(|d| d.format("%Y-%m-%d").to_string()),
        to: today.format("%Y-%m-%d").to_string(),
        kpis,
        curve,
        group: group.as_str(),
        groups,
        days: days.into_values().rev().collect(),
        more_calls: count.saturating_sub(listed.len()),
        calls: listed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Row<'a> {
        t: &'a str,
        action: &'a str,
        qty: i64,
        net: f64,
        signal: Option<&'a str>,
        chat: Option<(i64, &'a str)>,
        ticker: &'a str,
        side: Option<&'a str>,
        reason: Option<&'a str>,
        message: Option<&'a str>,
    }

    fn leg(id: i64, r: Row) -> Leg {
        Leg {
            id,
            ticker: r.ticker.into(),
            action: r.action.into(),
            qty: r.qty,
            executed_price: r.net / r.qty as f64,
            timestamp: r.t.into(),
            net_value: r.net,
            charges: 20.0,
            signal_id: r.signal.map(Into::into),
            raw_message: r.message.map(Into::into),
            exit_reason: r.reason.map(Into::into),
            chat_id: r.chat.map(|c| c.0),
            chat_title: r.chat.map(|c| c.1.to_string()),
            option_type: r.side.map(Into::into),
            strike: None,
        }
    }

    fn row<'a>(t: &'a str, action: &'a str, qty: i64, net: f64, signal: Option<&'a str>) -> Row<'a> {
        Row { t, action, qty, net, signal, chat: None, ticker: "NIFTY", side: None, reason: None, message: None }
    }

    fn date(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, d).unwrap()
    }

    #[test]
    fn legs_group_into_calls_with_net_pnl() {
        let a = Some((-1001, "Alpha Calls"));
        let legs = vec![
            // A Telegram call: 2 lots in, one out at target 1, one at target 2.
            leg(1, Row { chat: a, side: Some("CE"), message: Some("BUY NIFTY 25000 CE ABOVE 120"), ..row("2026-10-21 09:20:00", "BUY", 130, 15_620.0, Some("27962")) }),
            leg(2, Row { chat: a, side: Some("CE"), reason: Some("TGT1_PARTIAL"), ..row("2026-10-21 09:40:00", "SELL", 65, 9_050.0, Some("27962")) }),
            leg(3, Row { chat: a, side: Some("CE"), reason: Some("TGT2_HIT"), ..row("2026-10-21 10:05:00", "SELL", 65, 10_350.0, Some("27962")) }),
            // A scalp, still open.
            leg(4, Row { side: Some("PE"), ..row("2026-10-21 11:00:00", "BUY", 65, 6_520.0, Some("manual-x")) }),
            // The same message id in another chat is another call.
            leg(5, Row { chat: Some((-1002, "Beta")), ..row("2026-10-21 12:00:00", "BUY", 65, 6_500.0, Some("27962")) }),
        ];
        let calls = calls(legs);
        assert_eq!(calls.len(), 3);
        let telegram = &calls[0];
        assert_eq!((telegram.channel.as_str(), telegram.side.as_deref()), ("Alpha Calls", Some("CE")));
        assert_eq!(telegram.closed_at.as_deref(), Some("2026-10-21 10:05:00"));
        // 9,050 + 10,350 sold against 15,620 paid.
        assert!((telegram.pnl.unwrap() - 3_780.0).abs() < 1e-9);
        assert_eq!(telegram.exit_reason.as_deref(), Some("TGT2_HIT"));
        assert_eq!(telegram.legs.len(), 3);
        assert_eq!(telegram.charges, 60.0);
        let scalp = &calls[1];
        assert_eq!((scalp.channel.as_str(), scalp.closed_at.as_ref()), (SCALPS, None));
        assert_eq!(calls[2].channel, "Beta");
    }

    #[test]
    fn rows_from_before_chats_were_kept_read_as_unknown_channel() {
        let legs = vec![
            leg(1, Row { message: Some("✅ BUY NIFTY 22300 CALLS CMP 100-102"), ..row("2026-09-01 09:20:00", "BUY", 65, 6_520.0, Some("900")) }),
            leg(2, row("2026-09-01 09:50:00", "SELL", 65, 7_000.0, Some("900"))),
            // No signal id at all: grouped by index and day.
            leg(3, row("2026-09-02 10:00:00", "BUY", 75, 7_500.0, None)),
            leg(4, row("2026-09-02 10:30:00", "SELL", 75, 7_000.0, None)),
            // Sold without a recorded buy (adopted from the broker): no P&L.
            leg(5, Row { ticker: "SENSEX", ..row("2026-09-03 10:30:00", "SELL", 20, 4_000.0, None) }),
        ];
        let calls = calls(legs);
        assert_eq!(calls.len(), 3);
        assert!(calls.iter().all(|c| c.channel == UNKNOWN_CHANNEL));
        assert_eq!(calls[0].side.as_deref(), Some("CE"), "read from the message");
        assert!((calls[1].pnl.unwrap() + 500.0).abs() < 1e-9);
        assert_eq!(calls[2].pnl, None);
    }

    #[test]
    fn a_range_counts_the_calls_closed_in_it() {
        let legs = vec![
            // Opened yesterday, closed today: today's call.
            leg(1, row("2026-10-20 15:00:00", "BUY", 65, 6_500.0, Some("1"))),
            leg(2, row("2026-10-21 09:30:00", "SELL", 65, 7_500.0, Some("1"))),
            // Closed six days ago: inside 7D, outside today.
            leg(3, row("2026-10-15 10:00:00", "BUY", 65, 6_500.0, Some("2"))),
            leg(4, row("2026-10-15 10:30:00", "SELL", 65, 6_000.0, Some("2"))),
            // Closed eight days ago: only in 30D and All.
            leg(5, row("2026-10-13 10:00:00", "BUY", 65, 6_500.0, Some("3"))),
            leg(6, row("2026-10-13 10:30:00", "SELL", 65, 8_000.0, Some("3"))),
            // Half sold: not a closed call yet.
            leg(7, row("2026-10-21 10:00:00", "BUY", 130, 13_000.0, Some("4"))),
            leg(8, row("2026-10-21 10:30:00", "SELL", 65, 7_000.0, Some("4"))),
        ];
        let calls = calls(legs);
        let count = |r| report(&calls, date(21), r, GroupBy::Channel).kpis.calls;
        assert_eq!(count(Range::Today), 1);
        assert_eq!(count(Range::Days(7)), 2);
        assert_eq!(count(Range::Days(30)), 3);
        assert_eq!(count(Range::All), 3);
        assert_eq!(report(&calls, date(21), Range::Days(7), GroupBy::Channel).from.as_deref(), Some("2026-10-15"));
    }

    #[test]
    fn kpis_curve_and_drawdown_follow_the_closed_calls() {
        // +1,000, −1,500, +700, in that order.
        let legs = vec![
            leg(1, row("2026-10-21 09:20:00", "BUY", 65, 6_000.0, Some("1"))),
            leg(2, row("2026-10-21 09:30:00", "SELL", 65, 7_000.0, Some("1"))),
            leg(3, Row { ticker: "SENSEX", side: Some("PE"), ..row("2026-10-21 10:00:00", "BUY", 20, 6_000.0, Some("2")) }),
            leg(4, Row { ticker: "SENSEX", side: Some("PE"), ..row("2026-10-21 10:30:00", "SELL", 20, 4_500.0, Some("2")) }),
            leg(5, row("2026-10-21 11:00:00", "BUY", 65, 6_000.0, Some("3"))),
            leg(6, row("2026-10-21 11:30:00", "SELL", 65, 6_700.0, Some("3"))),
        ];
        let r = report(&calls(legs), date(21), Range::Today, GroupBy::Index);
        let k = &r.kpis;
        assert_eq!((k.calls, k.wins, k.losses), (3, 2, 1));
        assert!((k.net - 200.0).abs() < 1e-9);
        assert!((k.win_rate.unwrap() - 200.0 / 3.0).abs() < 1e-9);
        assert!((k.profit_factor.unwrap() - 1_700.0 / 1_500.0).abs() < 1e-9);
        assert!((k.avg_per_call.unwrap() - 200.0 / 3.0).abs() < 1e-9);
        assert!((k.max_drawdown - 1_500.0).abs() < 1e-9);
        assert_eq!(r.curve.iter().map(|p| p.pnl).collect::<Vec<_>>(), [1_000.0, -500.0, 200.0]);
        assert_eq!(r.curve[1].drawdown, 1_500.0);
        // Grouped by index, best first.
        assert_eq!(r.groups.iter().map(|g| (g.label.as_str(), g.calls, g.net)).collect::<Vec<_>>(), [("NIFTY", 2, 1_700.0), ("SENSEX", 1, -1_500.0)]);
        assert_eq!(r.groups.iter().map(|g| g.profit_factor).collect::<Vec<_>>(), [None, Some(0.0)]);
        assert_eq!(r.days.len(), 1);
        assert_eq!((r.days[0].calls, r.days[0].net), (3, 200.0));
        // 21 Oct 2026 is a Wednesday: nothing expired.
        assert!(r.days[0].expiries.is_empty());
        // Listed newest first.
        assert_eq!(r.calls.first().map(|c| c.closed_at.as_deref()), Some(Some("2026-10-21 11:30:00")));

        let by_side = report(&calls(vec![leg(1, row("2026-10-21 09:20:00", "BUY", 65, 6_000.0, Some("1"))), leg(2, row("2026-10-21 09:30:00", "SELL", 65, 7_000.0, Some("1")))]), date(21), Range::Today, GroupBy::Side);
        assert_eq!(by_side.groups[0].label, "Other");
        assert_eq!(GroupBy::parse("cepe"), Some(GroupBy::Side));
        assert_eq!(Range::parse("7D"), Some(Range::Days(7)));
        assert_eq!(Range::parse("90d"), None);
    }
}
