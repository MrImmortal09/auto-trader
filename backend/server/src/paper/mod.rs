//! Each signed-in user's own paper portfolio (`/api/v2/me/*`): a wallet, a
//! ledger of fills and the positions open now, kept apart from the owner's
//! desk.
//!
//! Nothing here can reach the broker. `PaperDesk` holds the database pool and
//! the shared price map (read only) and nothing else: no Kotak client, no
//! position monitor, none of the owner's positions, no log or signal channel.
//! A buy or a sell fills at once, in the route, at the live price the route
//! checked — paper has no order to wait on. The watch loop (`engine`) sells
//! on a chart stop or target and at expiry; the feed keeper (`feed`) is the
//! only paper code that talks to the market-data bridge, and it only
//! subscribes.
//!
//! Every fill is one SQLite transaction — the ledger row, the wallet and the
//! positions snapshot together — taken under the one book lock, so the caps
//! and the book in memory never drift from the database. The wallet never
//! goes below zero: a buy's `UPDATE` matches only while the balance covers it.

pub mod engine;
pub mod feed;
pub mod fill;

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use axum::http::StatusCode;
use chrono::NaiveDate;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use shared_domain::{ChartLevel, ChartLevels, LevelTrigger};
use sqlx::SqlitePool;
use tokio::sync::{Mutex, MutexGuard};
use trading_engine::ChargeBreakdown;

use crate::desk::contract_label;

/// What a new or reset portfolio starts with.
pub const INITIAL_WALLET: f64 = 1_000_000.0;
/// Distinct contracts one user may hold at once.
pub const MAX_OPEN_POSITIONS_PER_USER: usize = 10;
/// Distinct contracts all users together may hold: each is a feed
/// subscription kept for as long as it is held, beside the owner's.
pub const MAX_USER_POSITION_SCRIPS: usize = 40;
/// Most lots one tap may buy or sell.
pub const MAX_LOTS_PER_TAP: i32 = crate::routes::execute::MAX_LOTS_PER_TAP;
/// Activity lines kept per user, in memory only.
const MAX_ACTIVITY: usize = 100;

/// A listed index option, as the route resolved it from the scrip master.
#[derive(Debug, Clone, PartialEq)]
pub struct ContractRef {
    pub index: String,
    pub trading_symbol: String,
    pub ws_key: String,
    pub expiry: NaiveDate,
    pub strike: f64,
    /// `CE` or `PE`.
    pub option_type: String,
    pub lot_size: i32,
}

/// One open paper position: one contract, bought in one or more taps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaperPosition {
    pub id: String,
    pub index: String,
    pub trading_symbol: String,
    pub ws_key: String,
    pub expiry: NaiveDate,
    pub strike: f64,
    pub option_type: String,
    pub lot_size: i32,
    pub qty: i32,
    /// The average fill price, before charges.
    pub avg_price: f64,
    #[serde(default, skip_serializing_if = "ChartLevels::is_empty")]
    pub chart: ChartLevels,
    /// IST `YYYY-MM-DD HH:MM:SS` of the first buy.
    pub opened_at: String,
    /// The last fresh price seen: the expiry square-off's fallback when the
    /// feed has gone quiet.
    #[serde(default)]
    pub last_ltp: Option<f64>,
}

impl PaperPosition {
    /// "NIFTY 25000 CE".
    pub fn label(&self) -> String {
        contract_label(&self.index, Some(self.strike), Some(&self.option_type))
    }

    pub fn lots(&self) -> i32 {
        if self.lot_size > 0 { self.qty / self.lot_size } else { self.qty }
    }
}

/// One line of a user's activity: a fill, a level set, or why the desk sold.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Activity {
    /// IST `YYYY-MM-DD HH:MM:SS`.
    pub t: String,
    /// `buy`, `sell`, `levels` or `reset`.
    pub kind: &'static str,
    pub position_id: Option<String>,
    pub message: String,
}

/// Why the desk didn't do what was asked, as the route answers it.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub status: StatusCode,
    pub message: String,
}

impl Refusal {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Refusal { status, message: message.into() }
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    /// Another user's position is just as missing as one that never was.
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not found")
    }

    fn server(what: &str, e: impl std::fmt::Display) -> Self {
        tracing::error!("paper desk: {what}: {e}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, format!("could not {what}"))
    }
}

/// A fill, as the route answers it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Filled {
    /// The ledger row: the status bar's `fills` go by it.
    pub trade_id: i64,
    pub position_id: String,
    /// `BUY` or `SELL`.
    pub action: &'static str,
    pub contract: String,
    pub qty: i32,
    pub price: f64,
    /// What the wallet paid (a buy) or got (a sell), charges included.
    pub net_value: f64,
    /// Units of the contract still held after it.
    pub held: i32,
    /// The wallet after it.
    pub balance: f64,
    pub reason: Option<&'static str>,
}

/// A user's wallet: its balance, and how many times it was reset — only the
/// legs of the current `epoch` show anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Wallet {
    pub balance: f64,
    pub epoch: i64,
}

type Book = HashMap<i64, Vec<PaperPosition>>;

/// The book, held while a user being removed holds nothing — no fill can
/// start for anyone until it is dropped. See [`PaperDesk::hold_flat`].
pub struct HeldFlat<'a>(#[allow(dead_code)] MutexGuard<'a, Book>);

/// One leg about to be written.
struct LegOut<'a> {
    pos: &'a PaperPosition,
    action: &'static str,
    qty: i32,
    price: f64,
    charges: ChargeBreakdown,
    reason: Option<&'static str>,
    label: String,
}

/// No charges: a contract settled at nothing pays none.
fn no_charges() -> ChargeBreakdown {
    ChargeBreakdown {
        gross_value: 0.0,
        brokerage: 0.0,
        stt_charge: 0.0,
        sebi_fee: 0.0,
        stamp_duty: 0.0,
        transaction_charge: 0.0,
        gst: 0.0,
        net_value: 0.0,
    }
}

fn reason_text(reason: &str) -> &'static str {
    match reason {
        "CHART_STOP" => "chart stop",
        "CHART_TARGET" => "chart target",
        "EXPIRY_SQUAREOFF" => "expiry square-off",
        "MANUAL_EXIT_ALL" => "exit all",
        _ => "by hand",
    }
}

pub struct PaperDesk {
    pool: SqlitePool,
    prices: Arc<DashMap<String, f64>>,
    /// Every user's open positions — one lock for every fill, so the caps
    /// across users hold exactly.
    book: Mutex<Book>,
    activity: std::sync::Mutex<HashMap<i64, VecDeque<Activity>>>,
}

impl PaperDesk {
    /// The desk with every user's open positions from their last snapshot.
    pub async fn load(pool: SqlitePool, prices: Arc<DashMap<String, f64>>) -> Result<Self, sqlx::Error> {
        let rows = sqlx::query_as::<_, (i64, String)>("SELECT user_id, json FROM user_positions")
            .fetch_all(&pool)
            .await?;
        let mut book = Book::new();
        for (user_id, json) in rows {
            match serde_json::from_str::<Vec<PaperPosition>>(&json) {
                Ok(list) if !list.is_empty() => {
                    book.insert(user_id, list);
                }
                Ok(_) => {}
                Err(e) => tracing::error!(user_id, "paper desk: unreadable open positions, left out: {e}"),
            }
        }
        let held: usize = book.values().map(Vec::len).sum();
        if held > 0 {
            tracing::info!("Paper desk: {held} open position(s) across {} user(s) restored", book.len());
        }
        Ok(PaperDesk { pool, prices, book: Mutex::new(book), activity: Default::default() })
    }

    /// The live price behind feed key `key`, however old.
    pub fn price(&self, key: &str) -> Option<f64> {
        self.prices.get(key).map(|p| *p).filter(|p| *p > 0.0)
    }

    /// `user_id`'s wallet, opened with [`INITIAL_WALLET`] on first use.
    pub async fn wallet(&self, user_id: i64) -> Result<Wallet, Refusal> {
        let read = async {
            sqlx::query("INSERT OR IGNORE INTO user_wallets (user_id, balance, epoch, updated_at) VALUES (?, ?, 0, ?)")
                .bind(user_id)
                .bind(INITIAL_WALLET)
                .bind(shared_domain::current_ist_timestamp_string())
                .execute(&self.pool)
                .await?;
            sqlx::query_as::<_, (f64, i64)>("SELECT balance, epoch FROM user_wallets WHERE user_id = ?")
                .bind(user_id)
                .fetch_one(&self.pool)
                .await
        };
        let (balance, epoch) = read.await.map_err(|e| Refusal::server("read the wallet", e))?;
        Ok(Wallet { balance, epoch })
    }

    /// `user_id`'s open positions, oldest first.
    pub async fn open(&self, user_id: i64) -> Vec<PaperPosition> {
        self.book.lock().await.get(&user_id).cloned().unwrap_or_default()
    }

    /// Every user's open positions — what the watch loop checks.
    pub async fn all_open(&self) -> Vec<(i64, PaperPosition)> {
        let book = self.book.lock().await;
        book.iter().flat_map(|(user, list)| list.iter().map(|p| (*user, p.clone()))).collect()
    }

    /// The feed keys every held contract ticks on, and the indices they belong
    /// to — what the feed keeper keeps subscribed.
    pub async fn feed_keys(&self) -> (BTreeSet<String>, BTreeSet<String>) {
        let book = self.book.lock().await;
        let scrips = book.values().flatten().map(|p| p.ws_key.clone()).collect();
        let indices = book.values().flatten().map(|p| p.index.clone()).collect();
        (scrips, indices)
    }

    /// Remember the fresh prices the watch loop just saw, for the square-off.
    pub async fn note_prices(&self, seen: &[(i64, String, f64)]) {
        if seen.is_empty() {
            return;
        }
        let mut book = self.book.lock().await;
        for (user_id, id, price) in seen {
            if let Some(p) = book.get_mut(user_id).and_then(|l| l.iter_mut().find(|p| p.id == *id)) {
                p.last_ltp = Some(*price);
            }
        }
    }

    /// The book, held, if `user_id` holds nothing; `None` while they hold
    /// something. Removing a user takes this first, so they can't open a
    /// position while their rows are being deleted.
    pub async fn hold_flat(&self, user_id: i64) -> Option<HeldFlat<'_>> {
        let book = self.book.lock().await;
        (!book.get(&user_id).is_some_and(|l| !l.is_empty())).then_some(HeldFlat(book))
    }

    /// Drop what is kept in memory for a removed user.
    pub fn forget(&self, user_id: i64) {
        self.activity.lock().unwrap_or_else(|e| e.into_inner()).remove(&user_id);
    }

    /// `user_id`'s recent activity, newest first.
    pub fn activity(&self, user_id: i64) -> Vec<Activity> {
        let activity = self.activity.lock().unwrap_or_else(|e| e.into_inner());
        activity.get(&user_id).map(|a| a.iter().rev().cloned().collect()).unwrap_or_default()
    }

    fn note(&self, user_id: i64, kind: &'static str, position_id: Option<&str>, message: String) {
        let mut activity = self.activity.lock().unwrap_or_else(|e| e.into_inner());
        let lines = activity.entry(user_id).or_default();
        lines.push_back(Activity {
            t: shared_domain::current_ist_timestamp_string(),
            kind,
            position_id: position_id.map(str::to_string),
            message,
        });
        while lines.len() > MAX_ACTIVITY {
            lines.pop_front();
        }
    }

    /// Buy `lots` of `contract` at `price` for `user_id`: a new position, or
    /// an add to the one on the same contract. The route has checked the
    /// market hours and that `price` is live.
    pub async fn buy(&self, user_id: i64, contract: &ContractRef, lots: i32, price: f64) -> Result<Filled, Refusal> {
        if !(1..=MAX_LOTS_PER_TAP).contains(&lots) {
            return Err(Refusal::bad_request(format!("Lots must be between 1 and {MAX_LOTS_PER_TAP}")));
        }
        if !(price.is_finite() && price > 0.0) || contract.lot_size <= 0 {
            return Err(Refusal::conflict(format!("No live price for {} — not buying blind", contract.trading_symbol)));
        }
        let qty = lots * contract.lot_size;
        let mut book = self.book.lock().await;
        let mut next = book.get(&user_id).cloned().unwrap_or_default();
        let held = next.iter().position(|p| p.trading_symbol == contract.trading_symbol);
        if held.is_none() {
            if next.len() >= MAX_OPEN_POSITIONS_PER_USER {
                return Err(Refusal::conflict(format!(
                    "You hold {MAX_OPEN_POSITIONS_PER_USER} contracts already — close one before opening another"
                )));
            }
            let symbols: HashSet<&str> = book.values().flatten().map(|p| p.trading_symbol.as_str()).collect();
            if !symbols.contains(contract.trading_symbol.as_str()) && symbols.len() >= MAX_USER_POSITION_SCRIPS {
                return Err(Refusal::conflict(
                    "Paper trading is full right now — too many contracts are held across all accounts. Try again later",
                ));
            }
        }
        let index = match held {
            Some(i) => {
                let p = &mut next[i];
                p.avg_price = fill::merge_avg(p.qty, p.avg_price, qty, price);
                p.qty += qty;
                p.last_ltp = Some(price);
                i
            }
            None => {
                next.push(PaperPosition {
                    id: uuid::Uuid::new_v4().to_string(),
                    index: contract.index.clone(),
                    trading_symbol: contract.trading_symbol.clone(),
                    ws_key: contract.ws_key.clone(),
                    expiry: contract.expiry,
                    strike: contract.strike,
                    option_type: contract.option_type.clone(),
                    lot_size: contract.lot_size,
                    qty,
                    avg_price: price,
                    chart: ChartLevels::default(),
                    opened_at: shared_domain::current_ist_timestamp_string(),
                    last_ltp: Some(price),
                });
                next.len() - 1
            }
        };
        let pos = next[index].clone();
        let leg = LegOut {
            pos: &pos,
            action: "BUY",
            qty,
            price,
            charges: fill::buy_fill(&pos.index, qty, price),
            reason: None,
            label: format!("BUY {lots} lot(s) {} @ MKT {price:.2}", pos.label()),
        };
        let net_value = leg.charges.net_value;
        let (trade_id, balance) = self.record(user_id, leg, &next).await?;
        book.insert(user_id, next);
        drop(book);
        self.note(user_id, "buy", Some(&pos.id), format!("Bought {qty} {} at ₹{price:.2}", pos.label()));
        Ok(Filled {
            trade_id,
            position_id: pos.id.clone(),
            action: "BUY",
            contract: pos.label(),
            qty,
            price,
            net_value,
            held: pos.qty,
            balance,
            reason: None,
        })
    }

    /// Sell `lots` of `user_id`'s position `id` — all of it when `None` — at
    /// the price `quote` gives for it. Never more than is held.
    pub async fn sell(
        &self,
        user_id: i64,
        id: &str,
        lots: Option<i32>,
        reason: &'static str,
        quote: impl Fn(&PaperPosition) -> Option<f64>,
    ) -> Result<Filled, Refusal> {
        let mut book = self.book.lock().await;
        let pos = book
            .get(&user_id)
            .and_then(|l| l.iter().find(|p| p.id == id))
            .cloned()
            .ok_or_else(Refusal::not_found)?;
        let qty = match lots {
            None => pos.qty,
            Some(l) if !(1..=MAX_LOTS_PER_TAP).contains(&l) => {
                return Err(Refusal::bad_request(format!("Lots must be between 1 and {MAX_LOTS_PER_TAP}")));
            }
            Some(l) => l * pos.lot_size,
        };
        if qty > pos.qty {
            return Err(Refusal::conflict(format!("You hold only {} lot(s) of {}", pos.lots(), pos.label())));
        }
        let Some(price) = quote(&pos) else {
            return Err(Refusal::conflict(format!("{}'s price isn't live — not selling blind", pos.label())));
        };
        self.sell_held(&mut book, user_id, &pos, qty, price, reason).await
    }

    /// Sell every position `user_id` holds at the price `quote` gives for
    /// each: what was sold, and why each of the rest wasn't.
    pub async fn exit_all(
        &self,
        user_id: i64,
        quote: impl Fn(&PaperPosition) -> Option<f64>,
    ) -> (Vec<Filled>, Vec<String>) {
        let mut book = self.book.lock().await;
        let held = book.get(&user_id).cloned().unwrap_or_default();
        let (mut sold, mut kept) = (Vec::new(), Vec::new());
        for pos in held {
            let Some(price) = quote(&pos) else {
                kept.push(format!("{}'s price isn't live — not selling blind", pos.label()));
                continue;
            };
            match self.sell_held(&mut book, user_id, &pos, pos.qty, price, "MANUAL_EXIT_ALL").await {
                Ok(f) => sold.push(f),
                Err(r) => kept.push(format!("{}: {}", pos.label(), r.message)),
            }
        }
        (sold, kept)
    }

    /// Sell all of `user_id`'s position `id` at `price` — the watch loop's
    /// exit. A position already gone is a plain `not_found`.
    pub async fn settle(&self, user_id: i64, id: &str, price: f64, reason: &'static str) -> Result<Filled, Refusal> {
        let mut book = self.book.lock().await;
        let pos = book
            .get(&user_id)
            .and_then(|l| l.iter().find(|p| p.id == id))
            .cloned()
            .ok_or_else(Refusal::not_found)?;
        self.sell_held(&mut book, user_id, &pos, pos.qty, price, reason).await
    }

    /// Sell `qty` of `pos` (at most what it holds) at `price` (0 settles a
    /// contract that was never priced, with no charges).
    async fn sell_held(
        &self,
        book: &mut Book,
        user_id: i64,
        pos: &PaperPosition,
        qty: i32,
        price: f64,
        reason: &'static str,
    ) -> Result<Filled, Refusal> {
        let mut next = book.get(&user_id).cloned().unwrap_or_default();
        let Some(i) = next.iter().position(|p| p.id == pos.id) else {
            return Err(Refusal::not_found());
        };
        let qty = qty.min(next[i].qty);
        if qty <= 0 || !price.is_finite() || price < 0.0 {
            return Err(Refusal::bad_request("Nothing to sell"));
        }
        let charges = if price > 0.0 { fill::sell_fill(&pos.index, qty, price) } else { no_charges() };
        let held = next[i].qty - qty;
        if held == 0 {
            next.remove(i);
        } else {
            next[i].qty = held;
        }
        let lots = if pos.lot_size > 0 { qty / pos.lot_size } else { qty };
        let leg = LegOut {
            pos,
            action: "SELL",
            qty,
            price,
            charges,
            reason: Some(reason),
            label: format!("SELL {lots} lot(s) {} @ {price:.2}", pos.label()),
        };
        let net_value = leg.charges.net_value;
        let (trade_id, balance) = self.record(user_id, leg, &next).await?;
        if next.is_empty() {
            book.remove(&user_id);
        } else {
            book.insert(user_id, next);
        }
        self.note(
            user_id,
            "sell",
            Some(&pos.id),
            format!("Sold {qty} {} at ₹{price:.2} ({})", pos.label(), reason_text(reason)),
        );
        Ok(Filled {
            trade_id,
            position_id: pos.id.clone(),
            action: "SELL",
            contract: pos.label(),
            qty,
            price,
            net_value,
            held,
            balance,
            reason: Some(reason),
        })
    }

    /// Set, clear or keep the chart stop and target of `user_id`'s position
    /// `id` (`None` keeps a level, `Some(None)` clears it).
    pub async fn set_levels(
        &self,
        user_id: i64,
        id: &str,
        stop: Option<Option<ChartLevel>>,
        target: Option<Option<ChartLevel>>,
    ) -> Result<PaperPosition, Refusal> {
        let mut book = self.book.lock().await;
        let mut next = book.get(&user_id).cloned().unwrap_or_default();
        let Some(pos) = next.iter_mut().find(|p| p.id == id) else {
            return Err(Refusal::not_found());
        };
        for level in stop.iter().chain(target.iter()).flatten() {
            if !(level.price.is_finite() && level.price > 0.0) {
                return Err(Refusal::bad_request("a level's price must be a positive number"));
            }
            if level.trigger == LevelTrigger::Index && shared_domain::kotak_index_feed(&pos.index).is_none() {
                return Err(Refusal::bad_request(format!("{} has no index feed to trigger on", pos.index)));
            }
        }
        let before = pos.chart;
        if let Some(stop) = stop {
            pos.chart.stop = stop;
        }
        if let Some(target) = target {
            pos.chart.target = target;
        }
        let pos = pos.clone();
        if pos.chart != before {
            self.write_snapshot(user_id, &next).await?;
            book.insert(user_id, next);
            drop(book);
            let describe = |l: Option<ChartLevel>| match l {
                Some(l) if l.trigger == LevelTrigger::Index => format!("{} on the index", l.price),
                Some(l) => format!("₹{}", l.price),
                None => "none".to_string(),
            };
            self.note(
                user_id,
                "levels",
                Some(&pos.id),
                format!("{}: stop {}, target {}", pos.label(), describe(pos.chart.stop), describe(pos.chart.target)),
            );
        }
        Ok(pos)
    }

    /// Start `user_id`'s portfolio over: the wallet back to
    /// [`INITIAL_WALLET`] and a new epoch, so earlier legs drop out of every
    /// view (they stay in the ledger). Only with nothing open.
    pub async fn reset(&self, user_id: i64) -> Result<Wallet, Refusal> {
        let book = self.book.lock().await;
        if book.get(&user_id).is_some_and(|l| !l.is_empty()) {
            return Err(Refusal::conflict("Close every position before resetting your portfolio"));
        }
        let now = shared_domain::current_ist_timestamp_string();
        let write = async {
            sqlx::query("INSERT OR IGNORE INTO user_wallets (user_id, balance, epoch, updated_at) VALUES (?, ?, 0, ?)")
                .bind(user_id)
                .bind(INITIAL_WALLET)
                .bind(&now)
                .execute(&self.pool)
                .await?;
            sqlx::query("UPDATE user_wallets SET balance = ?, epoch = epoch + 1, updated_at = ? WHERE user_id = ?")
                .bind(INITIAL_WALLET)
                .bind(&now)
                .bind(user_id)
                .execute(&self.pool)
                .await?;
            sqlx::query_as::<_, (f64, i64)>("SELECT balance, epoch FROM user_wallets WHERE user_id = ?")
                .bind(user_id)
                .fetch_one(&self.pool)
                .await
        };
        let (balance, epoch) = write.await.map_err(|e| Refusal::server("reset the portfolio", e))?;
        drop(book);
        self.activity.lock().unwrap_or_else(|e| e.into_inner()).remove(&user_id);
        self.note(user_id, "reset", None, format!("Portfolio reset to ₹{INITIAL_WALLET:.0}"));
        Ok(Wallet { balance, epoch })
    }

    async fn write_snapshot(&self, user_id: i64, positions: &[PaperPosition]) -> Result<(), Refusal> {
        let json = serde_json::to_string(positions).map_err(|e| Refusal::server("save the positions", e))?;
        sqlx::query(
            "INSERT INTO user_positions (user_id, json, updated_at) VALUES (?, ?, ?)
             ON CONFLICT(user_id) DO UPDATE SET json = excluded.json, updated_at = excluded.updated_at",
        )
        .bind(user_id)
        .bind(json)
        .bind(shared_domain::current_ist_timestamp_string())
        .execute(&self.pool)
        .await
        .map_err(|e| Refusal::server("save the positions", e))?;
        Ok(())
    }

    /// Write one leg in one transaction: the wallet (a buy only while the
    /// balance covers it), the ledger row and the positions snapshot
    /// `positions` as they are after it. The ledger row's id and the balance
    /// after it.
    async fn record(&self, user_id: i64, leg: LegOut<'_>, positions: &[PaperPosition]) -> Result<(i64, f64), Refusal> {
        let json = serde_json::to_string(positions).map_err(|e| Refusal::server("save the positions", e))?;
        let now = shared_domain::current_ist_timestamp_string();
        let c = &leg.charges;
        let mut tx = self.pool.begin().await.map_err(|e| Refusal::server("record the fill", e))?;
        let written = async {
            sqlx::query("INSERT OR IGNORE INTO user_wallets (user_id, balance, epoch, updated_at) VALUES (?, ?, 0, ?)")
                .bind(user_id)
                .bind(INITIAL_WALLET)
                .bind(&now)
                .execute(&mut *tx)
                .await?;
            let moved = if leg.action == "BUY" {
                sqlx::query("UPDATE user_wallets SET balance = balance - ?, updated_at = ? WHERE user_id = ? AND balance >= ?")
                    .bind(c.net_value)
                    .bind(&now)
                    .bind(user_id)
                    .bind(c.net_value)
                    .execute(&mut *tx)
                    .await?
            } else {
                // A sell for less than its charges can't take the wallet below zero.
                sqlx::query("UPDATE user_wallets SET balance = MAX(balance + ?, 0), updated_at = ? WHERE user_id = ?")
                    .bind(c.net_value)
                    .bind(&now)
                    .bind(user_id)
                    .execute(&mut *tx)
                    .await?
            };
            if moved.rows_affected() != 1 {
                return Ok(None);
            }
            let (balance, epoch) = sqlx::query_as::<_, (f64, i64)>("SELECT balance, epoch FROM user_wallets WHERE user_id = ?")
                .bind(user_id)
                .fetch_one(&mut *tx)
                .await?;
            let p = leg.pos;
            let trade_id = sqlx::query(
                "INSERT INTO user_trades (user_id, epoch, position_id, ticker, trading_symbol, action, qty, executed_price,
                     timestamp, gross_value, brokerage, stt_charge, sebi_fee, stamp_duty, transaction_charge, gst, net_value,
                     exit_reason, option_type, strike, expiry, label)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(user_id)
            .bind(epoch)
            .bind(&p.id)
            .bind(&p.index)
            .bind(&p.trading_symbol)
            .bind(leg.action)
            .bind(leg.qty)
            .bind(leg.price)
            .bind(&now)
            .bind(c.gross_value)
            .bind(c.brokerage)
            .bind(c.stt_charge)
            .bind(c.sebi_fee)
            .bind(c.stamp_duty)
            .bind(c.transaction_charge)
            .bind(c.gst)
            .bind(c.net_value)
            .bind(leg.reason)
            .bind(&p.option_type)
            .bind(p.strike)
            .bind(p.expiry.format("%Y-%m-%d").to_string())
            .bind(&leg.label)
            .execute(&mut *tx)
            .await?
            .last_insert_rowid();
            sqlx::query(
                "INSERT INTO user_positions (user_id, json, updated_at) VALUES (?, ?, ?)
                 ON CONFLICT(user_id) DO UPDATE SET json = excluded.json, updated_at = excluded.updated_at",
            )
            .bind(user_id)
            .bind(&json)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
            Ok::<_, sqlx::Error>(Some((trade_id, balance)))
        }
        .await;
        match written {
            Ok(Some(done)) => {
                tx.commit().await.map_err(|e| Refusal::server("record the fill", e))?;
                Ok(done)
            }
            Ok(None) => {
                let _ = tx.rollback().await;
                Err(Refusal::conflict(format!(
                    "Not enough paper funds — this buy needs ₹{:.2} with charges",
                    leg.charges.net_value
                )))
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(Refusal::server("record the fill", e))
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::db::{init_db, test_support::TempDb};

    pub(crate) async fn desk(name: &str) -> (TempDb, PaperDesk) {
        let db = TempDb::new(&format!("paper-{name}"));
        let pool = init_db(&db.url()).await;
        let desk = PaperDesk::load(pool, Arc::new(DashMap::new())).await.unwrap();
        (db, desk)
    }

    pub(crate) fn nifty(strike: f64, side: &str) -> ContractRef {
        ContractRef {
            index: "NIFTY".into(),
            trading_symbol: format!("NIFTY26O27{strike:.0}{side}"),
            ws_key: format!("nse_fo|{strike:.0}{side}"),
            expiry: NaiveDate::from_ymd_opt(2026, 10, 27).unwrap(),
            strike,
            option_type: side.into(),
            lot_size: 75,
        }
    }

    async fn ledger(desk: &PaperDesk, user_id: i64) -> Vec<(String, i64, f64, Option<String>)> {
        sqlx::query_as("SELECT action, qty, executed_price, exit_reason FROM user_trades WHERE user_id = ? ORDER BY id")
            .bind(user_id)
            .fetch_all(&desk.pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_buy_opens_an_add_merges_and_a_sell_never_takes_more_than_is_held() {
        let (_db, desk) = desk("buy-sell").await;
        let c = nifty(25_000.0, "CE");
        let first = desk.buy(7, &c, 1, 100.0).await.unwrap();
        assert_eq!((first.qty, first.held, first.action), (75, 75, "BUY"));
        assert!((first.balance - (INITIAL_WALLET - first.net_value)).abs() < 1e-6);
        let add = desk.buy(7, &c, 1, 120.0).await.unwrap();
        assert_eq!(add.position_id, first.position_id);
        let open = desk.open(7).await;
        assert_eq!(open.len(), 1);
        assert_eq!((open[0].qty, open[0].avg_price), (150, 110.0));

        let at = |p: f64| move |_: &PaperPosition| Some(p);
        let too_many = desk.sell(7, &first.position_id, Some(3), "MANUAL_SELL", at(130.0)).await.unwrap_err();
        assert_eq!(too_many.status, StatusCode::CONFLICT);
        let blind = desk.sell(7, &first.position_id, Some(1), "MANUAL_SELL", |_| None).await.unwrap_err();
        assert_eq!(blind.status, StatusCode::CONFLICT);
        let part = desk.sell(7, &first.position_id, Some(1), "MANUAL_SELL", at(130.0)).await.unwrap();
        assert_eq!((part.qty, part.held), (75, 75));
        let rest = desk.sell(7, &first.position_id, None, "MANUAL_CLOSE", at(90.0)).await.unwrap();
        assert_eq!((rest.qty, rest.held), (75, 0));
        assert!(desk.open(7).await.is_empty());

        let rows = ledger(&desk, 7).await;
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[2], ("SELL".into(), 75, 130.0, Some("MANUAL_SELL".into())));
        assert_eq!(rows[3].3.as_deref(), Some("MANUAL_CLOSE"));
        // The snapshot is empty again, and the wallet is what the legs say.
        let json: String = sqlx::query_scalar("SELECT json FROM user_positions WHERE user_id = 7").fetch_one(&desk.pool).await.unwrap();
        assert_eq!(json, "[]");
        let wallet = desk.wallet(7).await.unwrap();
        let expected = INITIAL_WALLET - first.net_value - add.net_value + part.net_value + rest.net_value;
        assert!((wallet.balance - expected).abs() < 1e-6);
    }

    #[tokio::test]
    async fn the_wallet_never_goes_below_zero_even_when_buys_race() {
        let (_db, desk) = desk("funds").await;
        let desk = Arc::new(desk);
        // 10 lots at ₹650 cost about ₹4,87,600 a tap: two fit, a third doesn't.
        let c = nifty(25_000.0, "CE");
        let taps = (0..4).map(|_| {
            let desk = Arc::clone(&desk);
            let c = c.clone();
            tokio::spawn(async move { desk.buy(9, &c, 10, 650.0).await })
        });
        let results: Vec<_> = futures::future::join_all(taps).await.into_iter().map(|r| r.unwrap()).collect();
        let filled = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(filled, 2);
        for r in results.iter().filter_map(|r| r.as_ref().err()) {
            assert_eq!(r.status, StatusCode::CONFLICT);
            assert!(r.message.starts_with("Not enough paper funds"), "{}", r.message);
        }
        let wallet = desk.wallet(9).await.unwrap();
        assert!(wallet.balance >= 0.0);
        assert_eq!(desk.open(9).await[0].qty, 1_500);
        assert_eq!(ledger(&desk, 9).await.len(), 2);
    }

    #[tokio::test]
    async fn a_user_holds_at_most_ten_contracts_and_all_users_forty() {
        let (_db, desk) = desk("caps").await;
        for k in 0..MAX_OPEN_POSITIONS_PER_USER {
            desk.buy(1, &nifty(24_000.0 + 50.0 * k as f64, "CE"), 1, 1.0).await.unwrap();
        }
        let eleventh = desk.buy(1, &nifty(26_000.0, "CE"), 1, 1.0).await.unwrap_err();
        assert_eq!(eleventh.status, StatusCode::CONFLICT);
        // Adding to a contract already held is always fine.
        desk.buy(1, &nifty(24_000.0, "CE"), 1, 1.0).await.unwrap();
        for user in 2..=4 {
            for k in 0..MAX_OPEN_POSITIONS_PER_USER {
                desk.buy(user, &nifty(30_000.0 + 1_000.0 * user as f64 + 50.0 * k as f64, "PE"), 1, 1.0).await.unwrap();
            }
        }
        let full = desk.buy(5, &nifty(40_000.0, "CE"), 1, 1.0).await.unwrap_err();
        assert!(full.message.starts_with("Paper trading is full"), "{}", full.message);
        // A contract someone already holds adds no subscription.
        desk.buy(5, &nifty(24_000.0, "CE"), 1, 1.0).await.unwrap();
        assert!(desk.buy(5, &nifty(24_000.0, "CE"), 11, 1.0).await.is_err());
        assert!(desk.buy(5, &nifty(24_000.0, "CE"), 0, 1.0).await.is_err());
    }

    #[tokio::test]
    async fn one_user_can_neither_see_nor_touch_anothers_position() {
        let (_db, desk) = desk("isolation").await;
        let a = desk.buy(1, &nifty(25_000.0, "CE"), 1, 100.0).await.unwrap();
        assert!(desk.open(2).await.is_empty());
        let at = |_: &PaperPosition| Some(100.0);
        assert_eq!(desk.sell(2, &a.position_id, None, "MANUAL_CLOSE", at).await.unwrap_err().status, StatusCode::NOT_FOUND);
        assert_eq!(desk.set_levels(2, &a.position_id, Some(None), None).await.unwrap_err().status, StatusCode::NOT_FOUND);
        let (sold, kept) = desk.exit_all(2, at).await;
        assert!(sold.is_empty() && kept.is_empty());
        assert_eq!(desk.open(1).await.len(), 1);
        // The owner's own tables are never written.
        let owner: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM paper_trades").fetch_one(&desk.pool).await.unwrap();
        assert_eq!(owner, 0);
    }

    #[tokio::test]
    async fn a_reset_waits_for_a_flat_book_then_starts_a_new_epoch() {
        let (_db, desk) = desk("reset").await;
        let a = desk.buy(3, &nifty(25_000.0, "CE"), 2, 100.0).await.unwrap();
        assert_eq!(desk.reset(3).await.unwrap_err().status, StatusCode::CONFLICT);
        desk.settle(3, &a.position_id, 80.0, "MANUAL_CLOSE").await.unwrap();
        assert!(desk.wallet(3).await.unwrap().balance < INITIAL_WALLET);
        let wallet = desk.reset(3).await.unwrap();
        assert_eq!(wallet, Wallet { balance: INITIAL_WALLET, epoch: 1 });
        let legs = crate::history::load_user_legs(&desk.pool, 3, 1).await.unwrap();
        assert!(legs.is_empty());
        assert_eq!(crate::history::load_user_legs(&desk.pool, 3, 0).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn open_positions_and_their_levels_come_back_after_a_restart() {
        let db = TempDb::new("paper-restore");
        let pool = init_db(&db.url()).await;
        let desk = PaperDesk::load(pool.clone(), Arc::new(DashMap::new())).await.unwrap();
        let a = desk.buy(4, &nifty(25_000.0, "PE"), 1, 100.0).await.unwrap();
        let stop = ChartLevel { price: 25_150.0, trigger: LevelTrigger::Index };
        desk.set_levels(4, &a.position_id, Some(Some(stop)), None).await.unwrap();
        let before = desk.open(4).await;
        drop(desk);
        let again = PaperDesk::load(pool, Arc::new(DashMap::new())).await.unwrap();
        assert_eq!(again.open(4).await, before);
        assert_eq!(again.open(4).await[0].chart.stop, Some(stop));
        let (scrips, indices) = again.feed_keys().await;
        assert_eq!(scrips.into_iter().collect::<Vec<_>>(), ["nse_fo|25000PE"]);
        assert_eq!(indices.into_iter().collect::<Vec<_>>(), ["NIFTY"]);
    }

    #[tokio::test]
    async fn bad_levels_are_refused_and_a_contract_settled_at_nothing_pays_no_charges() {
        let (_db, desk) = desk("levels").await;
        let a = desk.buy(6, &nifty(25_000.0, "CE"), 1, 100.0).await.unwrap();
        let bad = ChartLevel { price: -1.0, trigger: LevelTrigger::Premium };
        assert_eq!(desk.set_levels(6, &a.position_id, Some(Some(bad)), None).await.unwrap_err().status, StatusCode::BAD_REQUEST);
        let gone = desk.settle(6, &a.position_id, 0.0, "EXPIRY_SQUAREOFF").await.unwrap();
        assert_eq!((gone.net_value, gone.held), (0.0, 0));
        assert_eq!(desk.activity(6)[0].kind, "sell");
        assert_eq!(desk.settle(6, &a.position_id, 0.0, "EXPIRY_SQUAREOFF").await.unwrap_err().status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_users_legs_read_back_as_one_scalp_call_per_position() {
        let (_db, desk) = desk("history").await;
        let c = nifty(25_000.0, "CE");
        let buy = desk.buy(2, &c, 2, 100.0).await.unwrap();
        let half = desk.sell(2, &buy.position_id, Some(1), "MANUAL_SELL", |_| Some(130.0)).await.unwrap();
        let rest = desk.settle(2, &buy.position_id, 90.0, "CHART_STOP").await.unwrap();
        let other = desk.buy(2, &nifty(25_100.0, "PE"), 1, 50.0).await.unwrap();

        let legs = crate::history::load_user_legs(&desk.pool, 2, 0).await.unwrap();
        let calls = crate::history::calls(legs);
        assert_eq!(calls.len(), 2);
        let closed = &calls[0];
        assert_eq!((closed.channel.as_str(), closed.index.as_str(), closed.strike, closed.side.as_deref()), ("Scalps", "NIFTY", Some(25_000.0), Some("CE")));
        assert_eq!((closed.bought_qty, closed.sold_qty), (150, 150));
        assert!(closed.closed_at.is_some());
        // Net proceeds less the same quantity at the average buy cost, charges included.
        let pnl = half.net_value + rest.net_value - buy.net_value;
        assert!((closed.pnl.unwrap() - pnl).abs() < 1e-6);
        assert_eq!(crate::desk::outcome(closed).unwrap().label, "Chart stop");
        assert!(calls[1].closed_at.is_none());
        assert_eq!(calls[1].signal_id.as_deref(), Some(format!("manual-{}", other.position_id).as_str()));
        assert_eq!(crate::desk::last_leg_id(&calls), other.trade_id);
    }

    #[tokio::test]
    async fn a_user_with_open_positions_cant_be_held_flat() {
        let (_db, desk) = desk("hold").await;
        desk.buy(8, &nifty(25_000.0, "CE"), 1, 100.0).await.unwrap();
        assert!(desk.hold_flat(8).await.is_none());
        assert!(desk.hold_flat(9).await.is_some());
    }
}
