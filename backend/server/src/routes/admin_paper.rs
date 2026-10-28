//! `/api/admin/trades` and `/api/admin/positions`, and the paper figures on
//! `/api/admin/users`: every user's paper trades and open positions, for the
//! admin. Read-only — nothing here buys, sells or changes a portfolio (that
//! is `paper`'s), and nothing is written: a user who never traded has no
//! wallet row yet, and reads as a fresh ₹10,00,000.

use std::collections::HashMap;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::json;
use shared_domain::ChartLevels;

use crate::desk::{self, contract_label};
use crate::history;
use crate::paper::{PaperPosition, INITIAL_WALLET};
use crate::AppState;

/// Trades one answer lists by default, and at most.
const DEFAULT_LIMIT: usize = 300;
const MAX_LIMIT: usize = 1000;

fn server_error(what: &str, e: impl std::fmt::Display) -> Response {
    tracing::error!("admin paper: {what}: {e}");
    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": format!("could not read {what}") }))).into_response()
}

/// Each user's wallet, by id: balance and epoch (resets so far).
async fn wallets(pool: &sqlx::SqlitePool) -> Result<HashMap<i64, (f64, i64)>, sqlx::Error> {
    let rows = sqlx::query_as::<_, (i64, f64, i64)>("SELECT user_id, balance, epoch FROM user_wallets").fetch_all(pool).await?;
    Ok(rows.into_iter().map(|(user, balance, epoch)| (user, (balance, epoch))).collect())
}

/// One user's paper figures, as the admin's Users list shows them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PaperSummary {
    /// The wallet now; ₹10,00,000 before the first trade.
    pub balance: f64,
    /// How many times they started over.
    pub resets: i64,
    pub open: usize,
    /// The open positions' P&L at the live price.
    pub open_pnl: f64,
    /// Today's booked plus open P&L, since their last reset — what their
    /// own status bar shows.
    pub day_pnl: f64,
    /// Fills since their last reset.
    pub trades: usize,
    /// IST time of their newest fill, before or after a reset.
    pub last_trade_at: Option<String>,
}

fn open_pnl(state: &AppState, open: &[PaperPosition]) -> f64 {
    // `+ 0.0` turns the −0.0 an empty sum gives into 0.0.
    open.iter()
        .filter_map(|p| state.paper.price(&p.ws_key).map(|ltp| (ltp - p.avg_price) * f64::from(p.qty)))
        .sum::<f64>()
        + 0.0
}

/// The paper figures of `users`, by id.
pub(crate) async fn summaries(state: &AppState, users: &[i64]) -> Result<HashMap<i64, PaperSummary>, sqlx::Error> {
    let wallets = wallets(&state.db_pool).await?;
    let last: HashMap<i64, String> = sqlx::query_as::<_, (i64, String)>("SELECT user_id, MAX(timestamp) FROM user_trades GROUP BY user_id")
        .fetch_all(&state.db_pool)
        .await?
        .into_iter()
        .collect();
    let today = shared_domain::today_ist().format("%Y-%m-%d").to_string();
    let mut out = HashMap::new();
    for &user in users {
        let (balance, epoch) = wallets.get(&user).copied().unwrap_or((INITIAL_WALLET, 0));
        let calls = history::calls(history::load_user_legs(&state.db_pool, user, epoch).await?);
        let open = state.paper.open(user).await;
        let open_pnl = open_pnl(state, &open);
        out.insert(
            user,
            PaperSummary {
                balance,
                resets: epoch,
                open: open.len(),
                open_pnl,
                day_pnl: desk::booked_on(&calls, &today) + open_pnl,
                trades: calls.iter().map(|c| c.legs.len()).sum(),
                last_trade_at: last.get(&user).cloned(),
            },
        );
    }
    Ok(out)
}

/// One row of the users' ledger.
#[derive(Debug, Clone, sqlx::FromRow)]
struct Ledger {
    id: i64,
    user_id: i64,
    epoch: i64,
    position_id: String,
    ticker: String,
    action: String,
    qty: i64,
    executed_price: f64,
    timestamp: String,
    net_value: f64,
    charges: f64,
    exit_reason: Option<String>,
    option_type: Option<String>,
    strike: Option<f64>,
}

/// Every fill of `user` (everyone's when `None`), every epoch, oldest first.
async fn load_ledger(pool: &sqlx::SqlitePool, user: Option<i64>) -> Result<Vec<Ledger>, sqlx::Error> {
    sqlx::query_as::<_, Ledger>(
        "SELECT id, user_id, epoch, position_id, ticker, action, qty, executed_price, timestamp, net_value,
                brokerage + stt_charge + sebi_fee + stamp_duty + transaction_charge + gst AS charges,
                exit_reason, option_type, strike
         FROM user_trades WHERE ?1 IS NULL OR user_id = ?1 ORDER BY id ASC",
    )
    .bind(user)
    .fetch_all(pool)
    .await
}

/// One paper fill, as the admin's Trades list shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TradeOut {
    pub id: i64,
    pub user_id: i64,
    /// IST `YYYY-MM-DD HH:MM:SS`.
    pub t: String,
    /// `BUY` or `SELL`.
    pub action: String,
    pub contract: String,
    pub qty: i64,
    pub price: f64,
    pub charges: f64,
    /// What the wallet paid (a buy) or got (a sell), charges included.
    pub net: f64,
    /// A sell's P&L: its net less the same quantity at its position's
    /// average buy cost, charges included on both sides — the Desk's rule.
    pub pnl: Option<f64>,
    pub reason: Option<String>,
    /// From before the user's last reset: gone from their own views.
    pub before_reset: bool,
}

/// Each fill with a sell's P&L, oldest first. `epochs` is each user's
/// current epoch; a user missing from it hasn't reset.
fn trades_of(rows: Vec<Ledger>, epochs: &HashMap<i64, i64>) -> Vec<TradeOut> {
    let is_buy = |r: &Ledger| r.action.eq_ignore_ascii_case("BUY");
    // Each position's buys: units and what they cost, charges included.
    let mut bought: HashMap<(i64, i64, String), (i64, f64)> = HashMap::new();
    for r in rows.iter().filter(|r| is_buy(r)) {
        let b = bought.entry((r.user_id, r.epoch, r.position_id.clone())).or_default();
        b.0 += r.qty;
        b.1 += r.net_value;
    }
    rows.into_iter()
        .map(|r| {
            let cost = bought
                .get(&(r.user_id, r.epoch, r.position_id.clone()))
                .filter(|(qty, _)| *qty > 0)
                .map(|(qty, net)| net / *qty as f64);
            let pnl = if is_buy(&r) { None } else { cost.map(|c| r.net_value - r.qty as f64 * c) };
            TradeOut {
                id: r.id,
                user_id: r.user_id,
                action: r.action.to_ascii_uppercase(),
                contract: contract_label(&r.ticker, r.strike, r.option_type.as_deref()),
                qty: r.qty,
                price: r.executed_price,
                charges: r.charges,
                net: r.net_value,
                pnl,
                reason: r.exit_reason,
                before_reset: r.epoch < epochs.get(&r.user_id).copied().unwrap_or(0),
                t: r.timestamp,
            }
        })
        .collect()
}

#[derive(Deserialize)]
pub struct TradesQuery {
    user: Option<i64>,
    /// `YYYY-MM-DD`: fills on or after that IST day; every fill without it.
    from: Option<String>,
    limit: Option<usize>,
}

#[derive(Serialize)]
struct TradesAnswer {
    /// Newest first, at most `limit`.
    trades: Vec<TradeOut>,
    /// Fills in the range beyond `limit`.
    more: usize,
    /// Every fill in the range, and the P&L of its sells.
    count: usize,
    realized: f64,
}

/// `GET /api/admin/trades?user=&from=YYYY-MM-DD&limit=` — every user's paper
/// fills (or one user's), newest first, each with a sell's P&L, including
/// those from before a user's reset (marked).
pub async fn trades_handler(State(state): State<AppState>, Query(q): Query<TradesQuery>) -> Response {
    let from = match q.from.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(raw) => match NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
            Ok(day) => Some(day.format("%Y-%m-%d").to_string()),
            Err(_) => return (StatusCode::BAD_REQUEST, Json(json!({ "error": "from must be YYYY-MM-DD" }))).into_response(),
        },
    };
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let epochs: HashMap<i64, i64> = match wallets(&state.db_pool).await {
        Ok(w) => w.into_iter().map(|(user, (_, epoch))| (user, epoch)).collect(),
        Err(e) => return server_error("the wallets", e),
    };
    // A sell's P&L needs its position's buys, which may be older than `from`:
    // the whole ledger is read, then cut to the range.
    let rows = match load_ledger(&state.db_pool, q.user).await {
        Ok(rows) => rows,
        Err(e) => return server_error("the trades", e),
    };
    let mut trades: Vec<TradeOut> = trades_of(rows, &epochs)
        .into_iter()
        .filter(|t| from.as_deref().is_none_or(|day| t.t.as_str() >= day))
        .collect();
    let count = trades.len();
    let realized = trades.iter().filter_map(|t| t.pnl).sum::<f64>() + 0.0;
    trades.reverse();
    let more = count.saturating_sub(limit);
    trades.truncate(limit);
    Json(TradesAnswer { trades, more, count, realized }).into_response()
}

/// One open paper position, as the admin's list shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PositionOut {
    pub user_id: i64,
    pub id: String,
    pub contract: String,
    /// `YYYY-MM-DD`.
    pub expiry: String,
    pub qty: i32,
    pub lots: i32,
    pub avg: f64,
    pub ltp: Option<f64>,
    pub pnl: Option<f64>,
    /// IST `YYYY-MM-DD HH:MM:SS` of the first buy.
    pub opened_at: String,
    #[serde(skip_serializing_if = "ChartLevels::is_empty")]
    pub chart: ChartLevels,
}

/// `GET /api/admin/positions` — every user's open paper positions at the
/// live price, by user, oldest first.
pub async fn positions_handler(State(state): State<AppState>) -> Response {
    let mut rows: Vec<PositionOut> = state
        .paper
        .all_open()
        .await
        .into_iter()
        .map(|(user_id, p)| {
            let ltp = state.paper.price(&p.ws_key);
            PositionOut {
                user_id,
                contract: p.label(),
                expiry: p.expiry.format("%Y-%m-%d").to_string(),
                qty: p.qty,
                lots: p.lots(),
                avg: p.avg_price,
                pnl: ltp.map(|l| (l - p.avg_price) * f64::from(p.qty)),
                ltp,
                opened_at: p.opened_at.clone(),
                chart: p.chart,
                id: p.id,
            }
        })
        .collect();
    rows.sort_by(|a, b| (a.user_id, &a.opened_at).cmp(&(b.user_id, &b.opened_at)));
    Json(rows).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use dashmap::DashMap;

    use crate::db::{init_db, test_support::TempDb};
    use crate::paper::tests::nifty;
    use crate::paper::PaperDesk;

    #[tokio::test]
    async fn every_fill_comes_back_with_a_sells_pnl_and_resets_marked() {
        let db = TempDb::new("admin-paper");
        let pool = init_db(&db.url()).await;
        let desk = PaperDesk::load(pool.clone(), Arc::new(DashMap::new())).await.unwrap();
        let c = nifty(25_000.0, "CE");
        let buy = desk.buy(1, &c, 2, 100.0).await.unwrap();
        let half = desk.sell(1, &buy.position_id, Some(1), "MANUAL_SELL", |_| Some(130.0)).await.unwrap();
        let rest = desk.settle(1, &buy.position_id, 90.0, "MANUAL_CLOSE").await.unwrap();
        desk.reset(1).await.unwrap();
        let again = desk.buy(1, &c, 1, 50.0).await.unwrap();
        let other = desk.buy(2, &nifty(25_100.0, "PE"), 1, 80.0).await.unwrap();

        let epochs: HashMap<i64, i64> = wallets(&pool).await.unwrap().into_iter().map(|(u, (_, e))| (u, e)).collect();
        assert_eq!(epochs.get(&1), Some(&1));
        let all = trades_of(load_ledger(&pool, None).await.unwrap(), &epochs);
        assert_eq!(all.len(), 5);
        let cost = buy.net_value / 150.0;
        assert_eq!((all[0].action.as_str(), all[0].contract.as_str(), all[0].pnl), ("BUY", "NIFTY 25000 CE", None));
        assert!((all[1].pnl.unwrap() - (half.net_value - 75.0 * cost)).abs() < 1e-6);
        assert!((all[2].pnl.unwrap() - (rest.net_value - 75.0 * cost)).abs() < 1e-6);
        assert_eq!(all[2].reason.as_deref(), Some("MANUAL_CLOSE"));
        // The first three are from before user 1's reset; the rest are current.
        assert_eq!(all.iter().map(|t| t.before_reset).collect::<Vec<_>>(), [true, true, true, false, false]);
        assert_eq!((all[3].id, all[4].id), (again.trade_id, other.trade_id));

        let mine = trades_of(load_ledger(&pool, Some(2)).await.unwrap(), &epochs);
        assert_eq!(mine.iter().map(|t| t.user_id).collect::<Vec<_>>(), [2]);
    }
}
