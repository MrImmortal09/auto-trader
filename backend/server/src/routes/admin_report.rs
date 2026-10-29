//! `GET /api/admin/users/:id/report` — one user's paper P&L report, for the
//! admin: what they booked over a range of IST days, day by day, with every
//! trade's entry and exit times, each contract's buys and sells, the hours
//! their trades opened in, and the tradebook. Read-only, like `admin_paper`.
//!
//! Money follows `history`: a sell books its net proceeds less the same
//! quantity at its position's average buy cost, charges included on both
//! sides. A sell counts on the day it filled, so a position held overnight
//! books on each day it sold and is listed under each; its win or loss
//! counts once, on the day it was fully sold. Trades from before a user's
//! reset are kept, marked: the admin sees what a reset hides from the user.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::{NaiveDate, NaiveDateTime, Timelike};
use serde::{Deserialize, Serialize};
use serde_json::json;
use shared_domain::INDEX_NAMES;
use telegram_ingester::parser::expires_on;

use crate::auth::Role;
use crate::db;
use crate::desk::{contract_label, Outcome};
use crate::history::{self, Call, CurvePoint, LegRow};
use crate::routes::admin::UserOut;
use crate::routes::admin_paper::{self, PositionOut};
use crate::AppState;

/// Trades listed under the days, newest days first; the figures cover all.
const MAX_LISTED_TRADES: usize = 1500;
/// Fills listed in the tradebook, newest first; the figures cover all.
const MAX_LISTED_FILLS: usize = 2000;

fn refuse(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

fn server_error(what: &str, e: impl std::fmt::Display) -> Response {
    tracing::error!("admin report: {what}: {e}");
    refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("could not read {what}"))
}

fn is_buy(action: &str) -> bool {
    action.eq_ignore_ascii_case("BUY")
}

/// `YYYY-MM-DD` of an IST timestamp.
fn day_of(t: &str) -> &str {
    t.get(..10).unwrap_or(t)
}

fn parse_ts(t: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(t, "%Y-%m-%d %H:%M:%S").ok()
}

/// What a position's units cost: the average fill price, and the average
/// with the buys' charges.
struct Cost {
    avg_price: f64,
    avg_cost: f64,
}

fn cost_of(call: &Call) -> Option<Cost> {
    let (qty, value, net) = call
        .legs
        .iter()
        .filter(|l| is_buy(&l.action))
        .fold((0_i64, 0.0, 0.0), |(q, v, n), l| (q + l.qty, v + l.qty as f64 * l.price, n + l.net));
    (qty > 0).then(|| {
        let q = qty as f64;
        Cost { avg_price: value / q, avg_cost: net / q }
    })
}

/// What one sell booked: before charges, after them, and the charges between
/// — its own and its quantity's share of the buys'. `gross − charges = net`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Booked {
    gross: f64,
    charges: f64,
    net: f64,
}

impl Booked {
    fn of(leg: &LegRow, cost: &Cost) -> Booked {
        let qty = leg.qty as f64;
        let gross = qty * (leg.price - cost.avg_price);
        let net = leg.net - qty * cost.avg_cost;
        Booked { gross, charges: gross - net, net }
    }

    fn add(&mut self, b: Booked) {
        self.gross += b.gross;
        self.charges += b.charges;
        self.net += b.net;
    }
}

/// One trade — a paper position, from its first buy to its last sell — as a
/// day of the report lists it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Trade {
    /// The paper position's id.
    pub id: String,
    pub contract: String,
    /// IST `YYYY-MM-DD HH:MM:SS` of the first buy.
    pub opened_at: String,
    /// The last sell, once everything bought was sold; `None` while held.
    pub closed_at: Option<String>,
    /// From the first buy to the last sell, once closed.
    pub held_secs: Option<i64>,
    pub bought_qty: i64,
    pub sold_qty: i64,
    /// Average fill prices, before charges.
    pub avg_buy: Option<f64>,
    pub avg_sell: Option<f64>,
    /// The whole trade's P&L after charges, once closed.
    pub pnl: Option<f64>,
    /// What its sells on this day booked.
    pub day_gross: f64,
    pub day_charges: f64,
    pub day_net: f64,
    pub outcome: Option<Outcome>,
    pub before_reset: bool,
    /// Every fill of it, oldest first — other days' too.
    pub legs: Vec<LegRow>,
}

/// One IST day of the report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Day {
    pub date: String,
    /// Booked by the day's sells.
    pub gross: f64,
    pub charges: f64,
    pub net: f64,
    /// Bought and sold that day, quantity × price.
    pub turnover: f64,
    pub buys: usize,
    pub sells: usize,
    /// Trades fully sold that day, and how many of them made or lost money.
    pub closed: usize,
    pub wins: usize,
    pub losses: usize,
    /// IST timestamps of the day's first and last fill.
    pub first_at: String,
    pub last_at: String,
    /// The indices whose options expired that day.
    pub expiries: Vec<&'static str>,
    /// Every trade with a fill that day, in the order they opened.
    pub trades: Vec<Trade>,
    /// The day's trades left out of `trades`: past the report's limit.
    pub unlisted: usize,
}

/// One contract's fills over the range, as a broker's P&L statement lists it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContractRow {
    pub contract: String,
    /// Trades on it with a fill in the range.
    pub trades: usize,
    pub buy_qty: i64,
    pub buy_value: f64,
    pub sell_qty: i64,
    pub sell_value: f64,
    pub avg_buy: Option<f64>,
    pub avg_sell: Option<f64>,
    /// Booked by its sells in the range.
    pub gross: f64,
    pub charges: f64,
    pub net: f64,
}

/// The trades closed in the range, by the IST hour they opened in.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HourRow {
    pub hour: u32,
    pub closed: usize,
    pub wins: usize,
    pub net: f64,
}

/// One fill, as the tradebook lists it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FillRow {
    pub id: i64,
    /// IST `YYYY-MM-DD HH:MM:SS`.
    pub t: String,
    /// `BUY` or `SELL`.
    pub action: String,
    pub contract: String,
    pub qty: i64,
    pub price: f64,
    /// Quantity × price.
    pub value: f64,
    /// The fill's own charges.
    pub charges: f64,
    /// What the wallet paid (a buy) or got (a sell), charges included.
    pub net: f64,
    /// A sell's P&L, charges included on both sides.
    pub pnl: Option<f64>,
    pub reason: Option<String>,
    pub position_id: String,
    pub before_reset: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    /// Booked by every sell in the range: `gross − charges = net`.
    pub gross: f64,
    pub charges: f64,
    pub net: f64,
    /// Charges on every fill in the range, bought or sold: what the wallet paid.
    pub charges_paid: f64,
    pub turnover: f64,
    pub buys: usize,
    pub sells: usize,
    /// Trades fully sold in the range, and how they went.
    pub closed: usize,
    pub wins: usize,
    pub losses: usize,
    pub win_rate: Option<f64>,
    /// The winners' total over the losers'; `None` without a loser.
    pub profit_factor: Option<f64>,
    pub avg_per_trade: Option<f64>,
    pub avg_win: Option<f64>,
    pub avg_loss: Option<f64>,
    pub best: Option<f64>,
    pub worst: Option<f64>,
    pub avg_held_secs: Option<i64>,
    /// The deepest fall of the booked P&L from its peak, sell by sell.
    pub max_drawdown: f64,
    /// Days with a fill, and those whose sells booked a profit or a loss.
    pub days_traded: usize,
    pub green_days: usize,
    pub red_days: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    /// The range's first IST day; `None` from the first fill on.
    pub from: Option<String>,
    pub to: String,
    pub summary: Summary,
    /// The booked P&L after each sell, oldest first.
    pub curve: Vec<CurvePoint>,
    /// Newest first.
    pub days: Vec<Day>,
    /// Best first.
    pub contracts: Vec<ContractRow>,
    pub hours: Vec<HourRow>,
    /// Newest first, at most `MAX_LISTED_FILLS`.
    pub fills: Vec<FillRow>,
    pub more_fills: usize,
}

/// A day's figures before its trades are listed.
struct DayAcc {
    booked: Booked,
    turnover: f64,
    buys: usize,
    sells: usize,
    closed: usize,
    wins: usize,
    losses: usize,
    first_at: String,
    last_at: String,
    /// Indices into the calls.
    calls: Vec<usize>,
}

struct ContractAcc {
    row: ContractRow,
    booked: Booked,
    positions: BTreeSet<usize>,
}

fn ratio(a: f64, b: usize) -> Option<f64> {
    (b > 0).then(|| a / b as f64)
}

/// The report over one user's `calls` (every epoch) for the IST days
/// `from..=to`, from their first fill without `from`. `epochs` is each
/// position's epoch; `current` the user's own.
pub fn report(calls: &[Call], epochs: &HashMap<String, i64>, current: i64, from: Option<NaiveDate>, to: NaiveDate) -> Report {
    let from_day = from.map(|d| d.format("%Y-%m-%d").to_string());
    let to_day = to.format("%Y-%m-%d").to_string();
    let in_range = |t: &str| {
        let day = day_of(t);
        from_day.as_deref().is_none_or(|f| day >= f) && day <= to_day.as_str()
    };
    let position_of = |c: &Call| c.signal_id.as_deref().map(|s| s.strip_prefix("manual-").unwrap_or(s)).unwrap_or(c.key.as_str()).to_string();
    let before_reset = |id: &str| epochs.get(id).copied().unwrap_or(current) < current;

    let mut days: BTreeMap<String, DayAcc> = BTreeMap::new();
    let mut contracts: BTreeMap<String, ContractAcc> = BTreeMap::new();
    let mut fills: Vec<FillRow> = Vec::new();
    let mut sells: Vec<(&str, i64, f64)> = Vec::new();
    // Trades fully sold in the range: P&L, how long they were held, the hour they opened.
    let mut closed: Vec<(f64, Option<i64>, Option<u32>)> = Vec::new();
    let mut charges_paid = 0.0;

    for (i, call) in calls.iter().enumerate() {
        let position = position_of(call);
        let marked = before_reset(&position);
        let contract = contract_label(&call.index, call.strike, call.side.as_deref());
        let cost = cost_of(call);
        let mut touched: BTreeSet<&str> = BTreeSet::new();
        for leg in call.legs.iter().filter(|l| in_range(&l.t)) {
            let buy = is_buy(&leg.action);
            let value = leg.qty as f64 * leg.price;
            let booked = if buy { None } else { cost.as_ref().map(|c| Booked::of(leg, c)) };
            let date = day_of(&leg.t);
            let day = days.entry(date.to_string()).or_insert_with(|| DayAcc {
                booked: Booked::default(),
                turnover: 0.0,
                buys: 0,
                sells: 0,
                closed: 0,
                wins: 0,
                losses: 0,
                first_at: leg.t.clone(),
                last_at: leg.t.clone(),
                calls: Vec::new(),
            });
            day.turnover += value;
            if buy {
                day.buys += 1;
            } else {
                day.sells += 1;
            }
            if leg.t < day.first_at {
                day.first_at = leg.t.clone();
            }
            if leg.t > day.last_at {
                day.last_at = leg.t.clone();
            }
            if let Some(b) = booked {
                day.booked.add(b);
                sells.push((&leg.t, leg.id, b.net));
            }
            if touched.insert(date) {
                day.calls.push(i);
            }

            let row = contracts.entry(contract.clone()).or_insert_with(|| ContractAcc {
                row: ContractRow {
                    contract: contract.clone(),
                    trades: 0,
                    buy_qty: 0,
                    buy_value: 0.0,
                    sell_qty: 0,
                    sell_value: 0.0,
                    avg_buy: None,
                    avg_sell: None,
                    gross: 0.0,
                    charges: 0.0,
                    net: 0.0,
                },
                booked: Booked::default(),
                positions: BTreeSet::new(),
            });
            row.positions.insert(i);
            if buy {
                row.row.buy_qty += leg.qty;
                row.row.buy_value += value;
            } else {
                row.row.sell_qty += leg.qty;
                row.row.sell_value += value;
            }
            if let Some(b) = booked {
                row.booked.add(b);
            }

            charges_paid += leg.charges;
            fills.push(FillRow {
                id: leg.id,
                t: leg.t.clone(),
                action: leg.action.to_ascii_uppercase(),
                contract: contract.clone(),
                qty: leg.qty,
                price: leg.price,
                value,
                charges: leg.charges,
                net: leg.net,
                pnl: booked.map(|b| b.net),
                reason: leg.reason.clone(),
                position_id: position.clone(),
                before_reset: marked,
            });
        }

        if let (Some(closed_at), Some(pnl)) = (call.closed_at.as_deref(), call.pnl) {
            if in_range(closed_at) {
                let opened = parse_ts(&call.opened_at);
                let held = opened.zip(parse_ts(closed_at)).map(|(a, b)| (b - a).num_seconds());
                closed.push((pnl, held, opened.map(|t| t.hour())));
                if let Some(day) = days.get_mut(day_of(closed_at)) {
                    day.closed += 1;
                    day.wins += usize::from(pnl > 0.0);
                    day.losses += usize::from(pnl < 0.0);
                }
            }
        }
    }

    // The curve, sell by sell.
    sells.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    let mut curve = Vec::with_capacity(sells.len());
    let (mut running, mut peak, mut max_drawdown) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (t, _, net) in &sells {
        running += net;
        peak = peak.max(running);
        max_drawdown = max_drawdown.max(peak - running);
        curve.push(CurvePoint { t: t.to_string(), pnl: running, peak, drawdown: peak - running });
    }

    // The days, newest first, each with its trades while the limit allows.
    let mut listed = 0;
    let days: Vec<Day> = days
        .into_iter()
        .rev()
        .map(|(date, acc)| {
            let mut order = acc.calls.clone();
            order.sort_by(|&a, &b| (calls[a].opened_at.as_str(), a).cmp(&(calls[b].opened_at.as_str(), b)));
            let fits = listed + order.len() <= MAX_LISTED_TRADES;
            let trades: Vec<Trade> = if fits {
                listed += order.len();
                order.iter().map(|&i| trade_on(&calls[i], &date, &position_of(&calls[i]), &before_reset)).collect()
            } else {
                Vec::new()
            };
            Day {
                expiries: NaiveDate::parse_from_str(&date, "%Y-%m-%d")
                    .map(|d| INDEX_NAMES.iter().copied().filter(|i| expires_on(i, d)).collect())
                    .unwrap_or_default(),
                gross: acc.booked.gross,
                charges: acc.booked.charges,
                net: acc.booked.net,
                turnover: acc.turnover,
                buys: acc.buys,
                sells: acc.sells,
                closed: acc.closed,
                wins: acc.wins,
                losses: acc.losses,
                first_at: acc.first_at,
                last_at: acc.last_at,
                unlisted: if fits { 0 } else { order.len() },
                trades,
                date,
            }
        })
        .collect();

    let mut contracts: Vec<ContractRow> = contracts
        .into_values()
        .map(|acc| {
            let mut row = acc.row;
            row.trades = acc.positions.len();
            row.avg_buy = (row.buy_qty > 0).then(|| row.buy_value / row.buy_qty as f64);
            row.avg_sell = (row.sell_qty > 0).then(|| row.sell_value / row.sell_qty as f64);
            row.gross = acc.booked.gross;
            row.charges = acc.booked.charges;
            row.net = acc.booked.net;
            row
        })
        .collect();
    contracts.sort_by(|a, b| b.net.total_cmp(&a.net).then_with(|| a.contract.cmp(&b.contract)));

    let mut hours: BTreeMap<u32, HourRow> = BTreeMap::new();
    for (pnl, _, hour) in &closed {
        let Some(hour) = *hour else { continue };
        let row = hours.entry(hour).or_insert(HourRow { hour, closed: 0, wins: 0, net: 0.0 });
        row.closed += 1;
        row.wins += usize::from(*pnl > 0.0);
        row.net += pnl;
    }

    let total = days.iter().fold(Booked::default(), |mut t, d| {
        t.add(Booked { gross: d.gross, charges: d.charges, net: d.net });
        t
    });
    let pnls: Vec<f64> = closed.iter().map(|c| c.0).collect();
    let winners: Vec<f64> = pnls.iter().copied().filter(|p| *p > 0.0).collect();
    let losers: Vec<f64> = pnls.iter().copied().filter(|p| *p < 0.0).collect();
    let won: f64 = winners.iter().sum();
    let lost: f64 = -losers.iter().sum::<f64>();
    let held: Vec<i64> = closed.iter().filter_map(|c| c.1).collect();
    let summary = Summary {
        // `+ 0.0` turns the −0.0 an empty sum gives into 0.0.
        gross: total.gross + 0.0,
        charges: total.charges + 0.0,
        net: total.net + 0.0,
        charges_paid: charges_paid + 0.0,
        turnover: days.iter().map(|d| d.turnover).sum::<f64>() + 0.0,
        buys: days.iter().map(|d| d.buys).sum(),
        sells: days.iter().map(|d| d.sells).sum(),
        closed: pnls.len(),
        wins: winners.len(),
        losses: losers.len(),
        win_rate: ratio(winners.len() as f64 * 100.0, pnls.len()),
        profit_factor: (lost > 0.0).then(|| won / lost),
        avg_per_trade: ratio(pnls.iter().sum(), pnls.len()),
        avg_win: ratio(won, winners.len()),
        avg_loss: ratio(-lost, losers.len()),
        best: pnls.iter().copied().reduce(f64::max),
        worst: pnls.iter().copied().reduce(f64::min),
        avg_held_secs: (!held.is_empty()).then(|| held.iter().sum::<i64>() / held.len() as i64),
        max_drawdown,
        days_traded: days.len(),
        green_days: days.iter().filter(|d| d.net > 0.0).count(),
        red_days: days.iter().filter(|d| d.net < 0.0).count(),
    };

    fills.sort_by(|a, b| (b.t.as_str(), b.id).cmp(&(a.t.as_str(), a.id)));
    let more_fills = fills.len().saturating_sub(MAX_LISTED_FILLS);
    fills.truncate(MAX_LISTED_FILLS);

    Report {
        from: from_day,
        to: to_day,
        summary,
        curve,
        days,
        contracts,
        hours: hours.into_values().collect(),
        fills,
        more_fills,
    }
}

/// `call` as `date` lists it: the whole trade, and what its sells that day booked.
fn trade_on(call: &Call, date: &str, position: &str, before_reset: &impl Fn(&str) -> bool) -> Trade {
    let cost = cost_of(call);
    let mut day = Booked::default();
    if let Some(cost) = &cost {
        for leg in call.legs.iter().filter(|l| !is_buy(&l.action) && day_of(&l.t) == date) {
            day.add(Booked::of(leg, cost));
        }
    }
    let (sell_qty, sell_value) = call
        .legs
        .iter()
        .filter(|l| !is_buy(&l.action))
        .fold((0_i64, 0.0), |(q, v), l| (q + l.qty, v + l.qty as f64 * l.price));
    let held_secs = call
        .closed_at
        .as_deref()
        .and_then(parse_ts)
        .zip(parse_ts(&call.opened_at))
        .map(|(closed, opened)| (closed - opened).num_seconds());
    Trade {
        id: position.to_string(),
        contract: contract_label(&call.index, call.strike, call.side.as_deref()),
        opened_at: call.opened_at.clone(),
        closed_at: call.closed_at.clone(),
        held_secs,
        bought_qty: call.bought_qty,
        sold_qty: call.sold_qty,
        avg_buy: cost.as_ref().map(|c| c.avg_price),
        avg_sell: (sell_qty > 0).then(|| sell_value / sell_qty as f64),
        pnl: call.closed_at.as_ref().and(call.pnl),
        day_gross: day.gross + 0.0,
        day_charges: day.charges + 0.0,
        day_net: day.net + 0.0,
        outcome: call.outcome.clone(),
        before_reset: before_reset(position),
        legs: call.legs.clone(),
    }
}

/// Each of `user`'s positions' epoch.
async fn position_epochs(pool: &sqlx::SqlitePool, user: i64) -> Result<HashMap<String, i64>, sqlx::Error> {
    let rows = sqlx::query_as::<_, (String, i64)>("SELECT position_id, MAX(epoch) FROM user_trades WHERE user_id = ? GROUP BY position_id")
        .bind(user)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().collect())
}

/// `YYYY-MM-DD`, or nothing when the parameter is missing or blank.
fn day_param(raw: Option<&str>, name: &str) -> Result<Option<NaiveDate>, Response> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| refuse(StatusCode::BAD_REQUEST, format!("{name} must be YYYY-MM-DD"))),
    }
}

#[derive(Deserialize)]
pub struct ReportQuery {
    /// First IST day, `YYYY-MM-DD`; from the user's first fill without it.
    from: Option<String>,
    /// Last IST day; today without it.
    to: Option<String>,
}

#[derive(Serialize)]
struct Answer {
    /// The user's row, as the Users list shows it, with their paper figures now.
    user: UserOut,
    /// IST day of their first fill, before any reset.
    first_day: Option<String>,
    /// What they hold now, at the live price.
    open: Vec<PositionOut>,
    #[serde(flatten)]
    report: Report,
}

/// `GET /api/admin/users/:id/report?from=YYYY-MM-DD&to=YYYY-MM-DD` — one
/// user's paper P&L over the IST days `from..=to` (`to` defaults to today,
/// `from` to their first fill): the totals, the P&L curve, each day with its
/// trades and their fills, each contract, the hours trades opened in, the
/// tradebook, and what they hold now. Every epoch: trades from before a
/// reset are included, marked.
pub async fn report_handler(State(state): State<AppState>, Path(id): Path<i64>, Query(q): Query<ReportQuery>) -> Response {
    let from = match day_param(q.from.as_deref(), "from") {
        Ok(day) => day,
        Err(e) => return e,
    };
    let to = match day_param(q.to.as_deref(), "to") {
        Ok(day) => day.unwrap_or_else(shared_domain::today_ist),
        Err(e) => return e,
    };
    if from.is_some_and(|f| f > to) {
        return refuse(StatusCode::BAD_REQUEST, "from must be on or before to");
    }
    let mut user = match db::load_user(&state.db_pool, id).await {
        Ok(Some(row)) => UserOut::from(row),
        Ok(None) => return refuse(StatusCode::NOT_FOUND, "No such user"),
        Err(e) => return server_error("the user", e),
    };
    // Only a user trades on paper; the admin's report is empty.
    if user.role == Role::User.as_str() {
        user.paper = match admin_paper::summaries(&state, &[id]).await {
            Ok(mut paper) => paper.remove(&id),
            Err(e) => return server_error("the paper portfolio", e),
        };
    }
    let current = user.paper.as_ref().map_or(0, |p| p.resets);
    let legs = match history::load_user_legs_all(&state.db_pool, id).await {
        Ok(legs) => legs,
        Err(e) => return server_error("the trades", e),
    };
    let epochs = match position_epochs(&state.db_pool, id).await {
        Ok(epochs) => epochs,
        Err(e) => return server_error("the trades", e),
    };
    let first_day = legs.first().map(|l| day_of(&l.timestamp).to_string());
    let report = report(&history::calls(legs), &epochs, current, from, to);
    let open = state.paper.open(id).await.into_iter().map(|p| PositionOut::of(&state, id, p)).collect();
    Json(Answer { user, first_day, open, report }).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::Leg;

    const D1: &str = "2026-10-27";
    const D2: &str = "2026-10-28";

    fn leg(id: i64, t: &str, action: &str, qty: i64, price: f64, charges: f64, position: &str, strike: f64) -> Leg {
        let value = qty as f64 * price;
        Leg {
            id,
            ticker: "NIFTY".into(),
            action: action.into(),
            qty,
            executed_price: price,
            timestamp: t.into(),
            net_value: if action == "BUY" { value + charges } else { value - charges },
            charges,
            signal_id: Some(format!("manual-{position}")),
            raw_message: None,
            exit_reason: (action == "SELL").then(|| "MANUAL_CLOSE".to_string()),
            chat_id: None,
            chat_title: None,
            option_type: Some("CE".into()),
            strike: Some(strike),
        }
    }

    fn date(d: &str) -> NaiveDate {
        NaiveDate::parse_from_str(d, "%Y-%m-%d").unwrap()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    /// A: a winner on day 1, before the reset. B: held overnight, half sold
    /// each day. C: a loser on day 2.
    fn calls() -> (Vec<Call>, HashMap<String, i64>) {
        let legs = vec![
            leg(1, &format!("{D1} 09:20:00"), "BUY", 75, 100.0, 20.0, "a", 25_000.0),
            leg(2, &format!("{D1} 09:40:00"), "SELL", 75, 120.0, 30.0, "a", 25_000.0),
            leg(3, &format!("{D1} 10:00:00"), "BUY", 150, 200.0, 40.0, "b", 25_100.0),
            leg(4, &format!("{D1} 11:00:00"), "SELL", 75, 210.0, 25.0, "b", 25_100.0),
            leg(5, &format!("{D2} 09:30:00"), "SELL", 75, 190.0, 25.0, "b", 25_100.0),
            leg(6, &format!("{D2} 10:00:00"), "BUY", 75, 50.0, 10.0, "c", 25_200.0),
            leg(7, &format!("{D2} 10:30:00"), "SELL", 75, 40.0, 10.0, "c", 25_200.0),
        ];
        let epochs = HashMap::from([("a".to_string(), 0), ("b".to_string(), 1), ("c".to_string(), 1)]);
        (history::calls(legs), epochs)
    }

    #[test]
    fn each_day_books_its_own_sells_and_lists_every_trade_it_touched() {
        let (calls, epochs) = calls();
        let r = report(&calls, &epochs, 1, None, date(D2));
        assert_eq!(r.days.iter().map(|d| d.date.as_str()).collect::<Vec<_>>(), [D2, D1]);
        let (d2, d1) = (&r.days[0], &r.days[1]);

        // Day 1: A's +1,450 and half of B, +705.
        assert!(close(d1.net, 1_450.0 + 705.0));
        assert!(close(d1.gross, 1_500.0 + 750.0) && close(d1.charges, 50.0 + 45.0));
        assert_eq!((d1.buys, d1.sells, d1.closed, d1.wins, d1.losses), (2, 2, 1, 1, 0));
        assert!(close(d1.turnover, 7_500.0 + 9_000.0 + 30_000.0 + 15_750.0));
        assert_eq!((d1.first_at.as_str(), d1.last_at.as_str()), ("2026-10-27 09:20:00", "2026-10-27 11:00:00"));
        assert_eq!(d1.trades.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert!(d1.trades[0].before_reset && !d1.trades[1].before_reset);

        // Day 2: the rest of B, −795, and C, −770. B closed here, whole −90.
        assert!(close(d2.net, -795.0 - 770.0));
        assert_eq!((d2.closed, d2.wins, d2.losses), (2, 0, 2));
        let b = &d2.trades[0];
        assert_eq!((b.id.as_str(), b.legs.len(), b.closed_at.as_deref()), ("b", 3, Some("2026-10-28 09:30:00")));
        assert!(close(b.day_net, -795.0) && close(b.pnl.unwrap(), -90.0));
        assert_eq!(b.held_secs, Some(23 * 3600 + 30 * 60));
        assert!(close(b.avg_buy.unwrap(), 200.0) && close(b.avg_sell.unwrap(), 200.0));
        // Day 1 lists B with what it booked that day, and the whole trade's −90.
        assert!(close(d1.trades[1].day_net, 705.0) && close(d1.trades[1].pnl.unwrap(), -90.0));

        let s = &r.summary;
        assert!(close(s.net, 590.0) && close(s.gross - s.charges, s.net));
        assert!(close(s.charges_paid, 20.0 + 30.0 + 40.0 + 25.0 + 25.0 + 10.0 + 10.0));
        assert_eq!((s.closed, s.wins, s.losses, s.buys, s.sells), (3, 1, 2, 3, 4));
        assert!(close(s.profit_factor.unwrap(), 1_450.0 / 860.0));
        assert_eq!((s.best, s.worst), (Some(1_450.0), Some(-770.0)));
        assert!(close(s.avg_loss.unwrap(), -430.0));
        assert_eq!(s.avg_held_secs, Some((1_200 + 84_600 + 1_800) / 3));
        assert_eq!((s.days_traded, s.green_days, s.red_days), (2, 1, 1));
        // 1,450 → 2,155 → 1,360 → 590: down 1,565 from the peak.
        assert_eq!(r.curve.len(), 4);
        assert!(close(r.curve[3].pnl, 590.0) && close(s.max_drawdown, 1_565.0));

        // Opened at 9: A. At 10: B and C.
        assert_eq!(r.hours.iter().map(|h| (h.hour, h.closed, h.wins)).collect::<Vec<_>>(), [(9, 1, 1), (10, 2, 0)]);
        assert!(close(r.hours[1].net, -860.0));
        // Best contract first.
        assert_eq!(r.contracts.iter().map(|c| c.contract.as_str()).collect::<Vec<_>>(), ["NIFTY 25000 CE", "NIFTY 25100 CE", "NIFTY 25200 CE"]);
        assert_eq!((r.contracts[1].trades, r.contracts[1].buy_qty, r.contracts[1].sell_qty), (1, 150, 150));
        // The tradebook, newest first, a sell with its P&L.
        assert_eq!(r.fills.iter().map(|f| f.id).collect::<Vec<_>>(), [7, 6, 5, 4, 3, 2, 1]);
        assert!(close(r.fills[0].pnl.unwrap(), -770.0) && r.fills[1].pnl.is_none());
        assert!(r.fills[6].before_reset);
    }

    #[test]
    fn a_range_keeps_only_its_own_days() {
        let (calls, epochs) = calls();
        let r = report(&calls, &epochs, 1, Some(date(D2)), date(D2));
        assert_eq!((r.from.as_deref(), r.to.as_str()), (Some(D2), D2));
        assert_eq!(r.days.len(), 1);
        assert!(close(r.summary.net, -1_565.0));
        // B closed in range, though bought before it.
        assert_eq!((r.summary.closed, r.summary.buys, r.summary.sells), (2, 1, 2));
        assert_eq!(r.fills.len(), 3);
        assert_eq!(r.contracts.len(), 2);

        let none = report(&calls, &epochs, 1, Some(date("2026-10-29")), date("2026-10-29"));
        assert!(none.days.is_empty() && none.fills.is_empty() && none.curve.is_empty());
        assert_eq!((none.summary.net, none.summary.win_rate, none.summary.max_drawdown), (0.0, None, 0.0));
    }

    #[tokio::test]
    async fn the_ledger_is_read_across_resets_for_one_user_only() {
        use std::sync::Arc;

        use dashmap::DashMap;

        use crate::db::{init_db, test_support::TempDb};
        use crate::paper::tests::nifty;
        use crate::paper::PaperDesk;

        let db = TempDb::new("admin-report");
        let pool = init_db(&db.url()).await;
        let desk = PaperDesk::load(pool.clone(), Arc::new(DashMap::new())).await.unwrap();
        let c = nifty(25_000.0, "CE");
        let first = desk.buy(1, &c, 2, 100.0).await.unwrap();
        let half = desk.sell(1, &first.position_id, Some(1), "MANUAL_SELL", |_| Some(130.0)).await.unwrap();
        let rest = desk.settle(1, &first.position_id, 90.0, "MANUAL_CLOSE").await.unwrap();
        desk.reset(1).await.unwrap();
        let again = desk.buy(1, &c, 1, 50.0).await.unwrap();
        desk.buy(2, &nifty(25_100.0, "PE"), 1, 80.0).await.unwrap();

        let legs = history::load_user_legs_all(&pool, 1).await.unwrap();
        assert_eq!(legs.len(), 4, "both epochs, and nobody else's");
        let epochs = position_epochs(&pool, 1).await.unwrap();
        assert_eq!((epochs.get(&first.position_id), epochs.get(&again.position_id)), (Some(&0), Some(&1)));

        let today = shared_domain::today_ist();
        let r = report(&history::calls(legs), &epochs, 1, Some(today), today);
        let s = &r.summary;
        assert!(close(s.net, half.net_value + rest.net_value - first.net_value));
        // 75 × (130 − 100) + 75 × (90 − 100), before charges.
        assert!(close(s.gross, 1_500.0) && close(s.gross - s.charges, s.net));
        assert_eq!((s.closed, s.buys, s.sells), (1, 2, 2));
        let trades = &r.days[0].trades;
        assert_eq!(trades.iter().map(|t| (t.id.as_str(), t.before_reset)).collect::<Vec<_>>(), [(first.position_id.as_str(), true), (again.position_id.as_str(), false)]);
        assert_eq!((trades[1].closed_at.as_deref(), trades[1].pnl), (None, None));
    }
}
