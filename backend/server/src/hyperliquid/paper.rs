//! The paper perps desk: the owner's and every user's Hyperliquid perps on
//! paper, keyed by user id, each with a 10,000 USDC wallet.
//!
//! Fills are priced on the live mainnet market (`HlMarketData`): a market
//! order at the book's mid with the taker fee, a resting limit at its own
//! price with the maker fee once the mid reaches it. Every half second the
//! watch loop ([`run`]) pays funding on the hour, liquidates a position its
//! mark has taken to its liquidation price, fires stops and targets on the
//! mark, and fills resting limits and line orders. A price older than
//! [`QUOTE_FRESH_MS`] trades nothing.
//!
//! An account's wallet, its ledger rows and its book are written in one
//! transaction; a write that fails changes nothing in memory either.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::Mutex;

use super::perp::{apply_fill, exit_at, funding_payment, wrong_side, Book, Exit, FillReq, Leg, PerpLevels, PerpLine, PerpOrder, PerpPosition, PerpTarget, Side, MIN_NOTIONAL};
use super::{HlMarketData, Market, MARKETS};

/// What every paper wallet starts with, and starts over with.
pub const START_USDC: f64 = 10_000.0;
/// A price older than this fills nothing.
pub const QUOTE_FRESH_MS: i64 = 10_000;
const EVERY: Duration = Duration::from_millis(500);
const MAX_ACTIVITY: usize = 100;
/// Most targets one position or line order takes.
pub const MAX_TARGETS: usize = 5;
/// Most resting orders and line orders one account keeps at once.
pub const MAX_WAITING: usize = 10;
/// The longest a line order stays armed: a week.
pub const MAX_LINE_MINUTES: i64 = 7 * 24 * 60;
const HOUR_MS: i64 = 3_600_000;

/// Why the desk didn't do what was asked, as the route answers it.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub status: StatusCode,
    pub message: String,
}

impl Refusal {
    pub fn bad(message: impl Into<String>) -> Self {
        Refusal { status: StatusCode::BAD_REQUEST, message: message.into() }
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Refusal { status: StatusCode::CONFLICT, message: message.into() }
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Refusal { status: StatusCode::NOT_FOUND, message: message.into() }
    }
    fn server(what: &str, e: impl std::fmt::Display) -> Self {
        tracing::error!("perp desk: {what}: {e}");
        Refusal { status: StatusCode::INTERNAL_SERVER_ERROR, message: format!("could not {what}") }
    }
}

/// One line of an account's activity.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Activity {
    /// IST `YYYY-MM-DD HH:MM:SS`.
    pub t: String,
    /// `fill`, `order`, `line`, `levels`, `funding` or `reset`.
    pub kind: &'static str,
    pub market: Option<String>,
    pub message: String,
}

/// A perp's live prices to trade on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Px {
    pub mark: f64,
    /// The book's mid; the mark when the book has one side only.
    pub mid: f64,
    pub oracle: f64,
    /// The hourly funding rate.
    pub funding: f64,
}

#[derive(Debug, Clone)]
struct Account {
    book: Book,
    cash: f64,
    epoch: i64,
    activity: VecDeque<Activity>,
}

impl Account {
    fn note(&mut self, t: &str, kind: &'static str, market: Option<&str>, message: String) {
        self.activity.push_front(Activity { t: t.to_string(), kind, market: market.map(str::to_string), message });
        self.activity.truncate(MAX_ACTIVITY);
    }
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn now_ts() -> String {
    shared_domain::current_ist_timestamp_string()
}

fn market(key: &str) -> Result<&'static Market, Refusal> {
    Market::of_key(key).ok_or_else(|| Refusal::bad("market must be BTC, ETH or GOLD"))
}

/// "0.1 BTC-PERP at 63,856.0".
fn fill_words(m: &Market, leg: &Leg) -> String {
    let verb = if leg.action == "BUY" { "Bought" } else { "Sold" };
    let pnl = if leg.realized != 0.0 { format!(", P&L {:+.2} USDC", leg.realized) } else { String::new() };
    format!("{verb} {} {} at {} ({}){pnl}", leg.size, m.label, leg.price, leg.reason.to_lowercase())
}

/// A stop and targets as asked: on the market's price grid, sizes on its
/// size grid, on the right side of `reference` (the price the position is
/// opened at) for a position on `side`.
pub fn checked_levels(m: &Market, side: Side, reference: f64, stop: Option<f64>, targets: &[(f64, Option<f64>)]) -> Result<PerpLevels, Refusal> {
    if targets.len() > MAX_TARGETS {
        return Err(Refusal::bad(format!("At most {MAX_TARGETS} targets")));
    }
    let price = |p: f64, what: &str| -> Result<f64, Refusal> {
        if !(p.is_finite() && p > 0.0) {
            return Err(Refusal::bad(format!("A {what}'s price must be a positive number")));
        }
        Ok(m.round_price(p))
    };
    let stop = match stop {
        None => None,
        Some(p) => {
            let p = price(p, "stop")?;
            if let Some(why) = wrong_side("stop", side, reference, p) {
                return Err(Refusal::bad(why));
            }
            Some(p)
        }
    };
    let mut out = Vec::new();
    for (p, size) in targets {
        let p = price(*p, "target")?;
        if let Some(why) = wrong_side("target", side, reference, p) {
            return Err(Refusal::bad(why));
        }
        let size = match size {
            None => None,
            Some(s) => {
                let s = m.round_size(*s);
                if s <= 0.0 {
                    return Err(Refusal::bad("A target's size must be more than nothing"));
                }
                Some(s)
            }
        };
        out.push(PerpTarget { id: new_id(), price: p, size });
    }
    Ok(PerpLevels { stop, targets: out })
}

/// How an order is to fill.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Market,
    Limit(f64),
}

/// An order as the route took it.
#[derive(Debug, Clone)]
pub struct OrderReq {
    pub market: &'static Market,
    pub side: Side,
    pub size: f64,
    pub leverage: u32,
    pub kind: Kind,
    pub reduce_only: bool,
    pub stop: Option<f64>,
    pub targets: Vec<(f64, Option<f64>)>,
}

/// What an order did: the fills it made now, or the order resting.
#[derive(Debug, Clone, Serialize)]
pub struct OrderAnswer {
    pub filled: Vec<Leg>,
    pub resting: Option<PerpOrder>,
}

/// A change to a position's (or a line order's) stop and targets; anything
/// left out stays.
#[derive(Debug, Clone, Default)]
pub struct LevelsChange {
    /// `Some(None)` clears the stop.
    pub stop: Option<Option<f64>>,
    /// Add a target (no id), or move and resize one; `size: Some(None)` is
    /// "all left", `None` keeps a moved target's size.
    pub set_target: Option<(Option<String>, f64, Option<Option<f64>>)>,
    pub remove_target: Option<String>,
}

/// Apply `change` to `levels` for a holding of `held` (sized targets add up
/// to no more than it) on `m`. Prices go on the grid; a level past the
/// price is taken as asked (it fires on the next tick: a quick exit).
fn change_levels(m: &Market, levels: &mut PerpLevels, change: &LevelsChange, held: f64) -> Result<(), Refusal> {
    let mut next = levels.clone();
    if let Some(stop) = change.stop {
        next.stop = match stop {
            Some(p) if p.is_finite() && p > 0.0 => Some(m.round_price(p)),
            Some(_) => return Err(Refusal::bad("A stop's price must be a positive number")),
            None => None,
        };
    }
    if let Some(id) = &change.remove_target {
        let before = next.targets.len();
        next.targets.retain(|t| &t.id != id);
        if next.targets.len() == before {
            return Err(Refusal::not_found("No such target"));
        }
    }
    if let Some((id, price, size)) = &change.set_target {
        if !(price.is_finite() && *price > 0.0) {
            return Err(Refusal::bad("A target's price must be a positive number"));
        }
        let price = m.round_price(*price);
        let size = match size {
            Some(Some(s)) => {
                let s = m.round_size(*s);
                if s <= 0.0 {
                    return Err(Refusal::bad("A target's size must be more than nothing"));
                }
                Some(Some(s))
            }
            other => *other,
        };
        match id {
            Some(id) => {
                let t = next.targets.iter_mut().find(|t| &t.id == id).ok_or_else(|| Refusal::not_found("No such target"))?;
                t.price = price;
                if let Some(size) = size {
                    t.size = size;
                }
            }
            None => {
                if next.targets.len() >= MAX_TARGETS {
                    return Err(Refusal::conflict(format!("At most {MAX_TARGETS} targets")));
                }
                next.targets.push(PerpTarget { id: new_id(), price, size: size.flatten() });
            }
        }
    }
    let sized: f64 = next.targets.iter().filter_map(|t| t.size).sum();
    if sized > held + 1e-9 {
        return Err(Refusal::conflict(format!("Targets add up to {sized}, more than the {held} held")));
    }
    *levels = next;
    Ok(())
}

/// What one watch tick did to an account.
#[derive(Debug, Default)]
pub struct Stepped {
    pub legs: Vec<Leg>,
    pub notes: Vec<(&'static str, Option<String>, String)>,
    pub changed: bool,
}

/// One watch tick over `book` with `cash` free: funding on a new hour, then
/// each position's liquidation, stop and targets, then resting limits and
/// line orders. `price` gives a perp's fresh prices (`None`: none fresh —
/// nothing of it moves). Pure, so the rules are tested here.
pub fn step_book(book: &mut Book, cash: &mut f64, price: &dyn Fn(&str) -> Option<Px>, now_ms: i64, now: &str) -> Stepped {
    let mut out = Stepped::default();
    // Funding, once an hour (an hour the server was down is not charged).
    let hour = now_ms.div_euclid(HOUR_MS);
    if book.funded_hour != Some(hour) {
        if book.funded_hour.is_some() {
            for p in book.positions.iter_mut() {
                let Some(px) = price(&p.market) else { continue };
                let pay = funding_payment(p, px.oracle, px.funding);
                p.margin -= pay;
                p.funding += pay;
                if pay.abs() >= 0.005 {
                    let label = Market::of_key(&p.market).map_or(p.market.as_str(), |m| m.label);
                    out.notes.push(("funding", Some(p.market.clone()), format!("{label}: funding {} {:.2} USDC", if pay > 0.0 { "paid" } else { "received" }, pay.abs())));
                }
            }
        }
        book.funded_hour = Some(hour);
        out.changed = true;
    }

    let fill = |book: &mut Book, cash: &mut f64, out: &mut Stepped, req: &FillReq| match apply_fill(book, *cash, req, new_id) {
        Ok(f) => {
            *cash += f.cash_delta;
            for leg in &f.legs {
                out.notes.push(("fill", Some(leg.market.clone()), fill_words(req.market, leg)));
            }
            out.legs.extend(f.legs);
            out.changed = true;
            true
        }
        Err(why) => {
            out.notes.push(("fill", Some(req.market.key.to_string()), format!("{} not filled: {why}", req.market.label)));
            out.changed = true;
            false
        }
    };

    // Positions: liquidation, then the stop, then targets.
    let held: Vec<String> = book.positions.iter().map(|p| p.market.clone()).collect();
    for key in held {
        let (Some(m), Some(px)) = (Market::of_key(&key), price(&key)) else { continue };
        let Some(p) = book.position(&key).cloned() else { continue };
        let Some(exit) = exit_at(&p, m, px.mark) else { continue };
        let (size, at, fee, reason) = match &exit {
            Exit::Liquidation => (p.size, px.mark, m.taker_fee, "LIQUIDATION"),
            Exit::Stop => (p.size, px.mid, m.taker_fee, "STOP"),
            Exit::Targets { ids, size } => {
                if let Some(q) = book.position_mut(&key) {
                    q.levels.targets.retain(|t| !ids.contains(&t.id));
                }
                (*size, px.mid, m.taker_fee, "TARGET")
            }
        };
        let req = FillReq { market: m, side: p.side.flip(), size, price: at, fee_rate: fee, leverage: p.leverage, reduce_only: true, reason, now, levels: None };
        fill(book, cash, &mut out, &req);
    }

    // Resting limits: at their price, as a maker, once the mid reaches them.
    let orders = std::mem::take(&mut book.orders);
    for o in orders {
        let (Some(m), Some(px)) = (Market::of_key(&o.market), price(&o.market)) else {
            book.orders.push(o);
            continue;
        };
        if !o.reached(px.mid) {
            book.orders.push(o);
            continue;
        }
        let req = FillReq { market: m, side: o.side, size: o.size, price: o.price, fee_rate: m.maker_fee, leverage: o.leverage, reduce_only: o.reduce_only, reason: "LIMIT", now, levels: Some(&o.levels) };
        out.changed = true;
        if !fill(book, cash, &mut out, &req) {
            out.notes.push(("order", Some(o.market.clone()), format!("{} limit {} {} at {} dropped", m.label, o.side.action().to_lowercase(), o.size, o.price)));
        }
    }

    // Line orders: lapsed ones go; a crossed one buys or sells at the mid.
    let lines = std::mem::take(&mut book.lines);
    for l in lines {
        let Some(m) = Market::of_key(&l.market) else { continue };
        if l.valid_until.as_deref().is_some_and(|until| now >= until) {
            out.notes.push(("line", Some(l.market.clone()), format!("{} line order at {} lapsed, unfilled", m.label, l.price)));
            out.changed = true;
            continue;
        }
        let Some(px) = price(&l.market) else {
            book.lines.push(l);
            continue;
        };
        if !l.crossed(px.mark) {
            book.lines.push(l);
            continue;
        }
        let req = FillReq { market: m, side: l.side, size: l.size, price: px.mid, fee_rate: m.taker_fee, leverage: l.leverage, reduce_only: false, reason: "LINE", now, levels: Some(&l.levels) };
        out.changed = true;
        if !fill(book, cash, &mut out, &req) {
            out.notes.push(("line", Some(l.market.clone()), format!("{} line order at {} crossed but dropped", m.label, l.price)));
        }
    }
    out
}

/// A position as the screens show it: with its mark, P&L and liquidation.
#[derive(Debug, Clone, Serialize)]
pub struct PositionView {
    #[serde(flatten)]
    pub position: PerpPosition,
    pub symbol: String,
    pub label: &'static str,
    pub mark: Option<f64>,
    pub upnl: Option<f64>,
    /// P&L on the margin, in percent.
    pub roe_pct: Option<f64>,
    pub liq: Option<f64>,
    pub notional: Option<f64>,
}

/// One fill in the ledger.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct FillRow {
    pub id: i64,
    pub position_id: String,
    pub market: String,
    pub action: String,
    pub size: f64,
    pub price: f64,
    pub fee: f64,
    pub realized: f64,
    pub funding: f64,
    pub reason: String,
    pub timestamp: String,
}

/// `GET /api/v2/hl/account`: an account's perps.
#[derive(Debug, Clone, Serialize)]
pub struct AccountView {
    /// `paper` (LIVE comes later).
    pub mode: &'static str,
    pub start: f64,
    /// Free USDC: not behind a position.
    pub cash: f64,
    pub margin: f64,
    pub upnl: f64,
    /// Cash, plus each position's margin and P&L at its mark.
    pub equity: f64,
    pub epoch: i64,
    pub positions: Vec<PositionView>,
    pub orders: Vec<PerpOrder>,
    pub lines: Vec<PerpLine>,
    pub activity: Vec<Activity>,
    /// The latest 50 fills since the last reset, newest first.
    pub fills: Vec<FillRow>,
}

pub struct PerpDesk {
    pool: SqlitePool,
    hl: Arc<HlMarketData>,
    accounts: Mutex<HashMap<i64, Account>>,
}

impl PerpDesk {
    /// The desk, with every account that holds, rests or arms anything.
    pub async fn load(pool: SqlitePool, hl: Arc<HlMarketData>) -> Result<Self, sqlx::Error> {
        let rows = sqlx::query_as::<_, (i64, String, Option<f64>, Option<i64>)>(
            "SELECT b.user_id, b.json, w.balance, w.epoch FROM perp_books b LEFT JOIN perp_wallets w ON w.user_id = b.user_id",
        )
        .fetch_all(&pool)
        .await?;
        let mut accounts = HashMap::new();
        for (user_id, json, balance, epoch) in rows {
            match serde_json::from_str::<Book>(&json) {
                Ok(book) if !book.is_flat() => {
                    accounts.insert(user_id, Account { book, cash: balance.unwrap_or(START_USDC), epoch: epoch.unwrap_or(0), activity: VecDeque::new() });
                }
                Ok(_) => {}
                Err(e) => tracing::error!(user_id, "perp desk: unreadable book, left out: {e}"),
            }
        }
        if !accounts.is_empty() {
            tracing::info!("Perp desk: {} paper account(s) with something open restored", accounts.len());
        }
        Ok(PerpDesk { pool, hl, accounts: Mutex::new(accounts) })
    }

    /// `key`'s prices, if fresh.
    pub fn px(&self, key: &str) -> Option<Px> {
        let q = self.hl.quote(key)?;
        let age = shared_domain::now_ist().timestamp_millis() - q.at_ms;
        (age <= QUOTE_FRESH_MS && q.ctx.mark > 0.0).then(|| Px { mark: q.ctx.mark, mid: q.ctx.mid.filter(|m| *m > 0.0).unwrap_or(q.ctx.mark), oracle: q.ctx.oracle, funding: q.ctx.funding })
    }

    fn need_px(&self, m: &Market) -> Result<Px, Refusal> {
        self.px(m.key).ok_or_else(|| Refusal::conflict(format!("{}'s price isn't live — not trading blind", m.label)))
    }

    /// `user_id`'s account, read from the database on first use.
    async fn account<'a>(&self, map: &'a mut HashMap<i64, Account>, user_id: i64) -> Result<&'a mut Account, Refusal> {
        let unknown = !map.contains_key(&user_id);
        if unknown {
            let wallet = sqlx::query_as::<_, (f64, i64)>("SELECT balance, epoch FROM perp_wallets WHERE user_id = ?")
                .bind(user_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| Refusal::server("read the wallet", e))?;
            let book = sqlx::query_scalar::<_, String>("SELECT json FROM perp_books WHERE user_id = ?")
                .bind(user_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| Refusal::server("read the book", e))?
                .and_then(|json| serde_json::from_str::<Book>(&json).ok())
                .unwrap_or_default();
            let (cash, epoch) = wallet.unwrap_or((START_USDC, 0));
            map.entry(user_id).or_insert(Account { book, cash, epoch, activity: VecDeque::new() });
        }
        Ok(map.get_mut(&user_id).expect("just inserted"))
    }

    /// Write `acc` (its wallet and book) and `legs` in one transaction.
    async fn save(&self, user_id: i64, acc: &Account, legs: &[Leg]) -> Result<(), Refusal> {
        let json = serde_json::to_string(&acc.book).map_err(|e| Refusal::server("save the book", e))?;
        let now = now_ts();
        let write = async {
            let mut tx = self.pool.begin().await?;
            sqlx::query(
                "INSERT INTO perp_wallets (user_id, balance, epoch, updated_at) VALUES (?, ?, ?, ?)
                 ON CONFLICT(user_id) DO UPDATE SET balance = excluded.balance, epoch = excluded.epoch, updated_at = excluded.updated_at",
            )
            .bind(user_id)
            .bind(acc.cash)
            .bind(acc.epoch)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
            for leg in legs {
                sqlx::query(
                    "INSERT INTO perp_fills (user_id, epoch, position_id, market, action, size, price, fee, realized, funding, reason, timestamp)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(user_id)
                .bind(acc.epoch)
                .bind(&leg.position_id)
                .bind(&leg.market)
                .bind(leg.action)
                .bind(leg.size)
                .bind(leg.price)
                .bind(leg.fee)
                .bind(leg.realized)
                .bind(leg.funding)
                .bind(&leg.reason)
                .bind(&now)
                .execute(&mut *tx)
                .await?;
            }
            sqlx::query(
                "INSERT INTO perp_books (user_id, json, updated_at) VALUES (?, ?, ?)
                 ON CONFLICT(user_id) DO UPDATE SET json = excluded.json, updated_at = excluded.updated_at",
            )
            .bind(user_id)
            .bind(&json)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
            tx.commit().await
        };
        write.await.map_err(|e| Refusal::server("save the perps", e))
    }

    /// Run `change` on a copy of `user_id`'s account; keep it once written.
    async fn with_account<T>(&self, user_id: i64, change: impl FnOnce(&mut Account) -> Result<(T, Vec<Leg>), Refusal>) -> Result<T, Refusal> {
        let mut map = self.accounts.lock().await;
        let acc = self.account(&mut map, user_id).await?;
        let mut next = acc.clone();
        let (out, legs) = change(&mut next)?;
        self.save(user_id, &next, &legs).await?;
        *acc = next;
        Ok(out)
    }

    /// Send an order: a market order fills now at the mid; a limit fills now
    /// at the mid if the mid is already through it, else rests.
    pub async fn order(&self, user_id: i64, req: OrderReq) -> Result<OrderAnswer, Refusal> {
        let m = req.market;
        let px = self.need_px(m)?;
        let size = m.round_size(req.size);
        if size <= 0.0 {
            return Err(Refusal::bad(format!("A {} size carries at most {} decimals and must be more than nothing", m.label, m.sz_decimals)));
        }
        if !(1..=m.leverage_cap()).contains(&req.leverage) {
            return Err(Refusal::bad(format!("Leverage must be 1x to {}x on {}", m.leverage_cap(), m.label)));
        }
        let limit = match req.kind {
            Kind::Market => None,
            Kind::Limit(p) if p.is_finite() && p > 0.0 => Some(m.round_price(p)),
            Kind::Limit(_) => return Err(Refusal::bad("A limit's price must be a positive number")),
        };
        let reference = limit.unwrap_or(px.mid);
        if !req.reduce_only && size * reference < MIN_NOTIONAL {
            return Err(Refusal::bad(format!("An order must be worth at least ${MIN_NOTIONAL:.0}")));
        }
        let levels = if req.reduce_only { PerpLevels::default() } else { checked_levels(m, req.side, reference, req.stop, &req.targets)? };
        // Through the mid already: it fills now, as a taker.
        let now_fill = limit.is_none_or(|p| match req.side {
            Side::Long => p >= px.mid,
            Side::Short => p <= px.mid,
        });
        let now = now_ts();
        self.with_account(user_id, |acc| {
            if now_fill {
                let fill = FillReq { market: m, side: req.side, size, price: px.mid, fee_rate: m.taker_fee, leverage: req.leverage, reduce_only: req.reduce_only, reason: if limit.is_some() { "LIMIT" } else { "MARKET" }, now: &now, levels: Some(&levels) };
                let f = apply_fill(&mut acc.book, acc.cash, &fill, new_id).map_err(Refusal::conflict)?;
                acc.cash += f.cash_delta;
                for leg in &f.legs {
                    acc.note(&now, "fill", Some(m.key), fill_words(m, leg));
                }
                return Ok((OrderAnswer { filled: f.legs.clone(), resting: None }, f.legs));
            }
            if acc.book.orders.len() + acc.book.lines.len() >= MAX_WAITING {
                return Err(Refusal::conflict(format!("At most {MAX_WAITING} orders and line orders wait at once")));
            }
            if req.reduce_only && acc.book.position(m.key).is_none_or(|p| p.side == req.side) {
                return Err(Refusal::conflict(format!("No {} position for a reduce-only order to reduce", m.label)));
            }
            let order = PerpOrder { id: new_id(), market: m.key.to_string(), side: req.side, size, price: limit.unwrap_or(px.mid), leverage: req.leverage, reduce_only: req.reduce_only, placed_at: now.clone(), levels };
            acc.note(&now, "order", Some(m.key), format!("Limit {} {} {} at {} resting", order.side.action().to_lowercase(), size, m.label, order.price));
            acc.book.orders.push(order.clone());
            Ok((OrderAnswer { filled: Vec::new(), resting: Some(order) }, Vec::new()))
        })
        .await
    }

    /// Move a resting limit to `price`; one moved through the mid fills now.
    pub async fn move_order(&self, user_id: i64, order_id: &str, price: f64) -> Result<OrderAnswer, Refusal> {
        let (key, side, size, leverage, reduce_only, levels) = {
            let mut map = self.accounts.lock().await;
            let acc = self.account(&mut map, user_id).await?;
            let o = acc.book.orders.iter().find(|o| o.id == order_id).ok_or_else(|| Refusal::not_found("No such order"))?;
            (o.market.clone(), o.side, o.size, o.leverage, o.reduce_only, o.levels.clone())
        };
        self.cancel_order(user_id, order_id).await?;
        let m = market(&key)?;
        let stop = levels.stop;
        let targets: Vec<(f64, Option<f64>)> = levels.targets.iter().map(|t| (t.price, t.size)).collect();
        self.order(user_id, OrderReq { market: m, side, size, leverage, kind: Kind::Limit(price), reduce_only, stop, targets }).await
    }

    pub async fn cancel_order(&self, user_id: i64, order_id: &str) -> Result<(), Refusal> {
        let now = now_ts();
        self.with_account(user_id, |acc| {
            let i = acc.book.orders.iter().position(|o| o.id == order_id).ok_or_else(|| Refusal::not_found("No such order"))?;
            let o = acc.book.orders.remove(i);
            let label = Market::of_key(&o.market).map_or(o.market.as_str(), |m| m.label);
            acc.note(&now, "order", Some(&o.market), format!("Limit {} {} {label} at {} cancelled", o.side.action().to_lowercase(), o.size, o.price));
            Ok(((), Vec::new()))
        })
        .await
    }

    /// Close `size` of the market's position (all of it when `None`) at the mid.
    pub async fn close(&self, user_id: i64, m: &'static Market, size: Option<f64>) -> Result<Vec<Leg>, Refusal> {
        let px = self.need_px(m)?;
        let now = now_ts();
        self.with_account(user_id, |acc| {
            let p = acc.book.position(m.key).ok_or_else(|| Refusal::not_found(format!("No {} position", m.label)))?;
            let size = match size {
                None => p.size,
                Some(s) => {
                    let s = m.round_size(s);
                    if s <= 0.0 {
                        return Err(Refusal::bad("Close more than nothing"));
                    }
                    s.min(p.size)
                }
            };
            let req = FillReq { market: m, side: p.side.flip(), size, price: px.mid, fee_rate: m.taker_fee, leverage: p.leverage, reduce_only: true, reason: "CLOSE", now: &now, levels: None };
            let f = apply_fill(&mut acc.book, acc.cash, &req, new_id).map_err(Refusal::conflict)?;
            acc.cash += f.cash_delta;
            for leg in &f.legs {
                acc.note(&now, "fill", Some(m.key), fill_words(m, leg));
            }
            Ok((f.legs.clone(), f.legs))
        })
        .await
    }

    /// Change the market's position's stop and targets.
    pub async fn set_levels(&self, user_id: i64, m: &'static Market, change: LevelsChange) -> Result<PerpLevels, Refusal> {
        let now = now_ts();
        self.with_account(user_id, |acc| {
            let p = acc.book.position_mut(m.key).ok_or_else(|| Refusal::not_found(format!("No {} position", m.label)))?;
            let held = p.size;
            change_levels(m, &mut p.levels, &change, held)?;
            let levels = p.levels.clone();
            acc.note(&now, "levels", Some(m.key), levels_words(m, &levels));
            Ok((levels, Vec::new()))
        })
        .await
    }

    /// Arm a line order: a market order of `size` once the mark crosses
    /// `price` from where it is now. Its stop and targets go on the far side
    /// of the line from the stop, as a position opened there takes them.
    #[allow(clippy::too_many_arguments)]
    pub async fn arm_line(&self, user_id: i64, m: &'static Market, side: Side, size: f64, leverage: u32, price: f64, valid_minutes: Option<i64>, stop: Option<f64>, targets: Vec<(f64, Option<f64>)>) -> Result<PerpLine, Refusal> {
        let px = self.need_px(m)?;
        let size = m.round_size(size);
        if size <= 0.0 {
            return Err(Refusal::bad(format!("A {} size carries at most {} decimals and must be more than nothing", m.label, m.sz_decimals)));
        }
        if !(1..=m.leverage_cap()).contains(&leverage) {
            return Err(Refusal::bad(format!("Leverage must be 1x to {}x on {}", m.leverage_cap(), m.label)));
        }
        if !(price.is_finite() && price > 0.0) {
            return Err(Refusal::bad("The line's price must be a positive number"));
        }
        let price = m.round_price(price);
        if price == px.mark {
            return Err(Refusal::conflict("The mark is on the line already — place it above or below"));
        }
        if size * price < MIN_NOTIONAL {
            return Err(Refusal::bad(format!("An order must be worth at least ${MIN_NOTIONAL:.0}")));
        }
        let valid_until = match valid_minutes {
            None => None,
            Some(n) if (1..=MAX_LINE_MINUTES).contains(&n) => {
                Some((shared_domain::now_ist() + chrono::Duration::minutes(n)).format("%Y-%m-%d %H:%M:%S").to_string())
            }
            Some(_) => return Err(Refusal::bad(format!("A line order lasts 1 to {MAX_LINE_MINUTES} minutes, or until cancelled"))),
        };
        let levels = checked_levels(m, side, price, stop, &targets)?;
        let sized: f64 = levels.targets.iter().filter_map(|t| t.size).sum();
        if sized > size + 1e-9 {
            return Err(Refusal::bad("The targets add up to more than the order"));
        }
        let now = now_ts();
        self.with_account(user_id, |acc| {
            if acc.book.orders.len() + acc.book.lines.len() >= MAX_WAITING {
                return Err(Refusal::conflict(format!("At most {MAX_WAITING} orders and line orders wait at once")));
            }
            let line = PerpLine { id: new_id(), market: m.key.to_string(), side, size, leverage, price, rising: price > px.mark, valid_until, placed_at: now.clone(), levels };
            acc.note(&now, "line", Some(m.key), format!("Line order: {} {size} {} when the mark {} {price}", side.action().to_lowercase(), m.label, if line.rising { "rises to" } else { "falls to" }));
            acc.book.lines.push(line.clone());
            Ok((line, Vec::new()))
        })
        .await
    }

    /// Move a line order (it is armed again from where the mark is now)
    /// and/or change its stop and targets, kept on the right side of it.
    pub async fn update_line(&self, user_id: i64, line_id: &str, price: Option<f64>, change: LevelsChange) -> Result<PerpLine, Refusal> {
        let mark = {
            let mut map = self.accounts.lock().await;
            let acc = self.account(&mut map, user_id).await?;
            let l = acc.book.lines.iter().find(|l| l.id == line_id).ok_or_else(|| Refusal::not_found("No such line order"))?;
            self.need_px(market(&l.market)?)?.mark
        };
        let now = now_ts();
        self.with_account(user_id, |acc| {
            let l = acc.book.lines.iter_mut().find(|l| l.id == line_id).ok_or_else(|| Refusal::not_found("No such line order"))?;
            let m = market(&l.market)?;
            let mut next = l.clone();
            if let Some(p) = price {
                if !(p.is_finite() && p > 0.0) {
                    return Err(Refusal::bad("The line's price must be a positive number"));
                }
                let p = m.round_price(p);
                if p == mark {
                    return Err(Refusal::conflict("The mark is on the line already — place it above or below"));
                }
                next.price = p;
                next.rising = p > mark;
            }
            change_levels(m, &mut next.levels, &change, next.size)?;
            if let Some(stop) = next.levels.stop {
                if let Some(why) = wrong_side("stop", next.side, next.price, stop) {
                    return Err(Refusal::conflict(why));
                }
            }
            for t in &next.levels.targets {
                if let Some(why) = wrong_side("target", next.side, next.price, t.price) {
                    return Err(Refusal::conflict(why));
                }
            }
            *l = next.clone();
            acc.note(&now, "line", Some(m.key), format!("Line order moved: {} {} {} at {}", next.side.action().to_lowercase(), next.size, m.label, next.price));
            Ok((next, Vec::new()))
        })
        .await
    }

    pub async fn cancel_line(&self, user_id: i64, line_id: &str) -> Result<(), Refusal> {
        let now = now_ts();
        self.with_account(user_id, |acc| {
            let i = acc.book.lines.iter().position(|l| l.id == line_id).ok_or_else(|| Refusal::not_found("No such line order"))?;
            let l = acc.book.lines.remove(i);
            let label = Market::of_key(&l.market).map_or(l.market.as_str(), |m| m.label);
            acc.note(&now, "line", Some(&l.market), format!("Line order {} {} {label} at {} cancelled", l.side.action().to_lowercase(), l.size, l.price));
            Ok(((), Vec::new()))
        })
        .await
    }

    /// Start over at [`START_USDC`]: only with nothing open, resting or armed.
    pub async fn reset(&self, user_id: i64) -> Result<(), Refusal> {
        let now = now_ts();
        self.with_account(user_id, |acc| {
            if !acc.book.is_flat() {
                return Err(Refusal::conflict("Close every perp position and cancel every order first"));
            }
            acc.cash = START_USDC;
            acc.epoch += 1;
            acc.book = Book::default();
            acc.activity.clear();
            acc.note(&now, "reset", None, format!("Paper perps wallet started over at {START_USDC:.0} USDC"));
            Ok(((), Vec::new()))
        })
        .await
    }

    /// `user_id`'s account as the screens show it.
    pub async fn view(&self, user_id: i64) -> Result<AccountView, Refusal> {
        let (book, cash, epoch, activity) = {
            let mut map = self.accounts.lock().await;
            let acc = self.account(&mut map, user_id).await?;
            (acc.book.clone(), acc.cash, acc.epoch, acc.activity.iter().cloned().collect::<Vec<_>>())
        };
        let fills = sqlx::query_as::<_, FillRow>(
            "SELECT id, position_id, market, action, size, price, fee, realized, funding, reason, timestamp
             FROM perp_fills WHERE user_id = ? AND epoch = ? ORDER BY id DESC LIMIT 50",
        )
        .bind(user_id)
        .bind(epoch)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Refusal::server("read the fills", e))?;
        let positions: Vec<PositionView> = book
            .positions
            .iter()
            .filter_map(|p| {
                let m = Market::of_key(&p.market)?;
                let mark = self.hl.quote(m.key).map(|q| q.ctx.mark).filter(|v| *v > 0.0);
                let upnl = mark.map(|v| p.upnl(v));
                Some(PositionView {
                    position: p.clone(),
                    symbol: m.symbol(),
                    label: m.label,
                    mark,
                    upnl,
                    roe_pct: upnl.filter(|_| p.margin > 0.0).map(|u| u / p.margin * 100.0),
                    liq: p.liq_price(m),
                    notional: mark.map(|v| p.notional(v)),
                })
            })
            .collect();
        let margin: f64 = positions.iter().map(|p| p.position.margin).sum();
        let upnl: f64 = positions.iter().filter_map(|p| p.upnl).sum();
        Ok(AccountView {
            mode: "paper",
            start: START_USDC,
            cash,
            margin,
            upnl,
            equity: cash + margin + upnl,
            epoch,
            positions,
            orders: book.orders,
            lines: book.lines,
            activity,
            fills,
        })
    }

    /// One watch tick over every account holding, resting or arming anything.
    pub async fn step(&self) {
        let now_ms = shared_domain::now_ist().timestamp_millis();
        let now = now_ts();
        let prices: HashMap<&'static str, Px> = MARKETS.iter().filter_map(|m| self.px(m.key).map(|px| (m.key, px))).collect();
        let price = |key: &str| prices.get(key).copied();
        let mut map = self.accounts.lock().await;
        let ids: Vec<i64> = map.iter().filter(|(_, a)| !a.book.is_flat()).map(|(id, _)| *id).collect();
        for user_id in ids {
            let Some(acc) = map.get_mut(&user_id) else { continue };
            let mut next = acc.clone();
            let stepped = step_book(&mut next.book, &mut next.cash, &price, now_ms, &now);
            if !stepped.changed {
                continue;
            }
            for (kind, market, message) in stepped.notes {
                next.note(&now, kind, market.as_deref(), message);
            }
            match self.save(user_id, &next, &stepped.legs).await {
                Ok(()) => *acc = next,
                Err(r) => tracing::warn!(user_id, "perp desk: tick not saved, tried again next tick: {}", r.message),
            }
        }
    }
}

/// "BTC-PERP: stop 61,000, targets 64,000 (0.05), 65,000 (all left)".
fn levels_words(m: &Market, l: &PerpLevels) -> String {
    let stop = l.stop.map_or("none".to_string(), |s| s.to_string());
    let targets = if l.targets.is_empty() {
        "none".to_string()
    } else {
        l.targets.iter().map(|t| format!("{} ({})", t.price, t.size.map_or("all left".to_string(), |s| s.to_string()))).collect::<Vec<_>>().join(", ")
    };
    format!("{}: stop {stop}, targets {targets}", m.label)
}

/// The watch loop, for as long as the server runs.
pub async fn run(desk: Arc<PerpDesk>) {
    let mut tick = tokio::time::interval(EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        desk.step().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn btc() -> &'static Market {
        Market::of_key("BTC").unwrap()
    }

    fn px(mark: f64) -> Px {
        Px { mark, mid: mark, oracle: mark, funding: 0.0001 }
    }

    fn open_long(book: &mut Book, cash: &mut f64, size: f64, price: f64, levels: Option<&PerpLevels>) {
        let req = FillReq { market: btc(), side: Side::Long, size, price, fee_rate: btc().taker_fee, leverage: 10, reduce_only: false, reason: "MARKET", now: "2026-11-19 10:00:00", levels };
        let f = apply_fill(book, *cash, &req, new_id).unwrap();
        *cash += f.cash_delta;
    }

    const H: i64 = HOUR_MS;

    #[test]
    fn funding_is_paid_once_an_hour_from_the_margin() {
        let (mut book, mut cash) = (Book::default(), START_USDC);
        open_long(&mut book, &mut cash, 1.0, 60_000.0, None);
        let at = |mark: f64| move |_: &str| Some(px(mark));
        // The first tick only starts the clock.
        let s = step_book(&mut book, &mut cash, &at(60_000.0), 10 * H + 5, "t");
        assert!(s.changed && book.position("BTC").unwrap().funding == 0.0);
        // The same hour again: nothing.
        assert!(!step_book(&mut book, &mut cash, &at(60_000.0), 10 * H + 500_000, "t").changed);
        // The next hour: 1 BTC × 60,000 × 0.01 % = $6 out of the margin.
        step_book(&mut book, &mut cash, &at(60_000.0), 11 * H + 5, "t");
        let p = book.position("BTC").unwrap();
        assert!((p.funding - 6.0).abs() < 1e-9 && (p.margin - 5_994.0).abs() < 1e-9);
    }

    #[test]
    fn a_tick_fires_the_stop_and_targets_and_liquidates_on_the_mark() {
        let (mut book, mut cash) = (Book::default(), START_USDC);
        let levels = checked_levels(btc(), Side::Long, 60_000.0, Some(59_000.0), &[(61_000.0, Some(0.4)), (62_000.0, None)]).unwrap();
        open_long(&mut book, &mut cash, 1.0, 60_000.0, Some(&levels));
        book.funded_hour = Some(0);
        let at = |mark: f64| move |_: &str| Some(px(mark));
        // The first target: 0.4 sold at 61,000.
        let s = step_book(&mut book, &mut cash, &at(61_000.0), 0, "t");
        assert_eq!(s.legs.len(), 1);
        assert_eq!((s.legs[0].reason.as_str(), s.legs[0].size), ("TARGET", 0.4));
        let p = book.position("BTC").unwrap();
        assert!((p.size - 0.6).abs() < 1e-9 && p.levels.targets.len() == 1);
        // The stop: the rest.
        let s = step_book(&mut book, &mut cash, &at(58_900.0), 0, "t");
        assert_eq!(s.legs[0].reason, "STOP");
        assert!(book.positions.is_empty());

        // A long at 10x with no stop: liquidated near 54,700, closed at the mark.
        open_long(&mut book, &mut cash, 1.0, 60_000.0, None);
        let liq = book.position("BTC").unwrap().liq_price(btc()).unwrap();
        let before = cash;
        let s = step_book(&mut book, &mut cash, &at(liq - 5.0), 0, "t");
        assert_eq!(s.legs[0].reason, "LIQUIDATION");
        assert!(book.positions.is_empty());
        // What is left of the margin (about the maintenance margin) comes back.
        assert!(cash > before && cash - before < 0.0125 * liq * 1.01, "{}", cash - before);
    }

    #[test]
    fn a_stale_price_moves_nothing() {
        let (mut book, mut cash) = (Book::default(), START_USDC);
        let levels = checked_levels(btc(), Side::Long, 60_000.0, Some(59_000.0), &[]).unwrap();
        open_long(&mut book, &mut cash, 1.0, 60_000.0, Some(&levels));
        book.funded_hour = Some(0);
        let none = |_: &str| None;
        assert!(!step_book(&mut book, &mut cash, &none, 5 * H, "t").changed || book.position("BTC").is_some());
        assert!(book.position("BTC").is_some());
    }

    #[test]
    fn resting_limits_fill_at_their_price_and_lines_at_the_mid_once_crossed() {
        let (mut book, mut cash) = (Book::default(), START_USDC);
        book.funded_hour = Some(0);
        book.orders.push(PerpOrder { id: "o".into(), market: "BTC".into(), side: Side::Long, size: 0.1, price: 59_000.0, leverage: 5, reduce_only: false, placed_at: "t".into(), levels: PerpLevels::default() });
        let at = |mark: f64| move |_: &str| Some(px(mark));
        assert!(!step_book(&mut book, &mut cash, &at(59_500.0), 0, "t").changed);
        let s = step_book(&mut book, &mut cash, &at(58_990.0), 0, "t");
        assert_eq!((s.legs[0].reason.as_str(), s.legs[0].price), ("LIMIT", 59_000.0));
        assert!((s.legs[0].fee - 0.1 * 59_000.0 * btc().maker_fee).abs() < 1e-9, "a maker's fee");
        assert!(book.orders.is_empty());

        let levels = checked_levels(btc(), Side::Short, 58_000.0, Some(58_500.0), &[]).unwrap();
        book.lines.push(PerpLine { id: "l".into(), market: "BTC".into(), side: Side::Short, size: 0.1, leverage: 5, price: 58_000.0, rising: false, valid_until: None, placed_at: "t".into(), levels });
        assert!(step_book(&mut book, &mut cash, &at(58_100.0), 0, "t").legs.is_empty());
        let s = step_book(&mut book, &mut cash, &at(57_990.0), 0, "t");
        assert_eq!(s.legs.last().unwrap().reason, "LINE");
        assert!(book.lines.is_empty());
        // The short line closed the 0.1 long it met first: flat, its stop unused.
        assert!(book.positions.is_empty(), "{:?}", book.positions);
    }

    #[test]
    fn a_lapsed_line_goes_unfilled() {
        let (mut book, mut cash) = (Book::default(), START_USDC);
        book.funded_hour = Some(0);
        book.lines.push(PerpLine { id: "l".into(), market: "BTC".into(), side: Side::Long, size: 0.1, leverage: 5, price: 61_000.0, rising: true, valid_until: Some("2026-11-19 10:30:00".into()), placed_at: "t".into(), levels: PerpLevels::default() });
        let at = |mark: f64| move |_: &str| Some(px(mark));
        assert!(!step_book(&mut book, &mut cash, &at(60_000.0), 0, "2026-11-19 10:29:59").changed);
        let s = step_book(&mut book, &mut cash, &at(61_500.0), 0, "2026-11-19 10:30:00");
        assert!(s.legs.is_empty() && book.lines.is_empty());
    }

    #[test]
    fn levels_are_checked_against_the_entry_and_the_holding() {
        assert!(checked_levels(btc(), Side::Long, 60_000.0, Some(61_000.0), &[]).is_err(), "a long's stop above");
        assert!(checked_levels(btc(), Side::Short, 60_000.0, None, &[(61_000.0, None)]).is_err(), "a short's target above");
        let l = checked_levels(btc(), Side::Long, 60_000.0, Some(59_000.37), &[(61_000.0, Some(0.123456))]).unwrap();
        assert_eq!((l.stop, l.targets[0].size), (Some(59_000.0), Some(0.12345)));
        let mut levels = l.clone();
        // A second sized target past the holding is refused; "all left" fits.
        assert!(change_levels(btc(), &mut levels, &LevelsChange { set_target: Some((None, 62_000.0, Some(Some(0.9)))), ..Default::default() }, 1.0).is_err());
        change_levels(btc(), &mut levels, &LevelsChange { set_target: Some((None, 62_000.0, Some(None))), ..Default::default() }, 1.0).unwrap();
        assert_eq!(levels.targets.len(), 2);
        // A stop dragged past the price is taken (it fires next tick).
        change_levels(btc(), &mut levels, &LevelsChange { stop: Some(Some(70_000.0)), ..Default::default() }, 1.0).unwrap();
        assert_eq!(levels.stop, Some(70_000.0));
        change_levels(btc(), &mut levels, &LevelsChange { stop: Some(None), ..Default::default() }, 1.0).unwrap();
        assert_eq!(levels.stop, None);
    }
}
