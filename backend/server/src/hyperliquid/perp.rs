//! Perp positions on isolated margin, and what moves them: fills, funding,
//! liquidation, stops, targets, resting limits and line orders. Pure — no
//! clock, no I/O — so every rule here is tested on its own; the paper desk
//! (`paper`) keeps the books and calls in.
//!
//! The rules are Hyperliquid's: one net position per perp, long or short;
//! isolated margin `notional / leverage`; maintenance margin half the
//! initial margin at the perp's max leverage; liquidation where the
//! position's margin plus its P&L falls to the maintenance margin; hourly
//! funding against the oracle price, which a long pays to shorts when the
//! rate is positive; fees on the notional of every fill.

use serde::{Deserialize, Serialize};

use super::Market;

/// A size this small is nothing (sizes carry at most five decimals).
const DUST: f64 = 1e-9;
/// The smallest order Hyperliquid takes, in USD of notional.
pub const MIN_NOTIONAL: f64 = 10.0;

/// Long (bought) or short (sold); for an order, which way it trades: a
/// buy is `Long`, a sell `Short`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Long,
    Short,
}

impl Side {
    /// `+1` long, `-1` short.
    pub fn sign(self) -> f64 {
        match self {
            Side::Long => 1.0,
            Side::Short => -1.0,
        }
    }

    pub fn flip(self) -> Side {
        match self {
            Side::Long => Side::Short,
            Side::Short => Side::Long,
        }
    }

    /// `BUY` or `SELL`: the ledger's word for a fill this way.
    pub fn action(self) -> &'static str {
        match self {
            Side::Long => "BUY",
            Side::Short => "SELL",
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Side::Long => "long",
            Side::Short => "short",
        }
    }
}

/// A target: close `size` of the position (all of what is left when
/// `None`) once the mark reaches `price`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerpTarget {
    pub id: String,
    pub price: f64,
    #[serde(default)]
    pub size: Option<f64>,
}

/// A position's stop and targets, on the perp's price.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PerpLevels {
    #[serde(default)]
    pub stop: Option<f64>,
    #[serde(default)]
    pub targets: Vec<PerpTarget>,
}

impl PerpLevels {
    pub fn is_empty(&self) -> bool {
        self.stop.is_none() && self.targets.is_empty()
    }

    /// Trim the targets so their sizes add up to no more than `held`: the
    /// farthest from `entry` loses first. An "all left" target always fits.
    pub fn trim_to(&mut self, held: f64, side: Side, entry: f64) {
        loop {
            let total: f64 = self.targets.iter().filter_map(|t| t.size).sum();
            if total <= held + DUST {
                return;
            }
            let Some(i) = self
                .targets
                .iter()
                .enumerate()
                .filter(|(_, t)| t.size.is_some())
                .max_by(|(_, a), (_, b)| (side.sign() * (a.price - entry)).total_cmp(&(side.sign() * (b.price - entry))))
                .map(|(i, _)| i)
            else {
                return;
            };
            let over = total - held;
            let size = self.targets[i].size.unwrap_or(0.0);
            if size > over + DUST {
                self.targets[i].size = Some(size - over);
            } else {
                self.targets.remove(i);
            }
        }
    }
}

/// An open perp position on isolated margin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerpPosition {
    pub id: String,
    /// The market's key: `BTC`.
    pub market: String,
    pub side: Side,
    /// In the coin, always positive.
    pub size: f64,
    /// The average entry price.
    pub entry: f64,
    pub leverage: u32,
    /// The isolated margin behind it, in USDC, after funding.
    pub margin: f64,
    /// Funding paid so far (negative: received).
    #[serde(default)]
    pub funding: f64,
    /// Fees paid on its fills so far.
    #[serde(default)]
    pub fees: f64,
    /// IST `YYYY-MM-DD HH:MM:SS` of its first fill.
    pub opened_at: String,
    #[serde(default, skip_serializing_if = "PerpLevels::is_empty")]
    pub levels: PerpLevels,
}

impl PerpPosition {
    pub fn notional(&self, price: f64) -> f64 {
        self.size * price
    }

    /// Unrealized P&L at `mark`.
    pub fn upnl(&self, mark: f64) -> f64 {
        self.side.sign() * self.size * (mark - self.entry)
    }

    /// Where it is liquidated: where its margin plus P&L falls to the
    /// maintenance margin. `None` when no price can do it (a long at 1x).
    pub fn liq_price(&self, m: &Market) -> Option<f64> {
        if self.size <= DUST {
            return None;
        }
        let mm = m.maintenance();
        let per_unit = self.margin / self.size;
        let liq = match self.side {
            Side::Long => (self.entry - per_unit) / (1.0 - mm),
            Side::Short => (self.entry + per_unit) / (1.0 + mm),
        };
        (liq.is_finite() && liq > 0.0).then_some(liq)
    }

    /// Whether the mark has reached its liquidation price.
    pub fn liquidated_at(&self, m: &Market, mark: f64) -> bool {
        match (self.liq_price(m), self.side) {
            (Some(liq), Side::Long) => mark <= liq,
            (Some(liq), Side::Short) => mark >= liq,
            (None, _) => false,
        }
    }
}

/// A resting limit order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerpOrder {
    pub id: String,
    pub market: String,
    /// `Long` buys, `Short` sells.
    pub side: Side,
    pub size: f64,
    pub price: f64,
    pub leverage: u32,
    /// Only ever closes what is held: never opens or flips a position.
    #[serde(default)]
    pub reduce_only: bool,
    pub placed_at: String,
    /// The stop and targets the position it opens (or adds to) takes.
    #[serde(default, skip_serializing_if = "PerpLevels::is_empty")]
    pub levels: PerpLevels,
}

impl PerpOrder {
    /// Whether the book's `mid` has reached it: a buy fills at or below its
    /// price, a sell at or above.
    pub fn reached(&self, mid: f64) -> bool {
        match self.side {
            Side::Long => mid <= self.price,
            Side::Short => mid >= self.price,
        }
    }
}

/// A line order: a market order sent once the mark crosses `price` from the
/// side it was armed on, with the stop and targets its position then takes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerpLine {
    pub id: String,
    pub market: String,
    pub side: Side,
    pub size: f64,
    pub leverage: u32,
    pub price: f64,
    /// Armed below the line: it triggers as the mark rises to it.
    pub rising: bool,
    /// IST `YYYY-MM-DD HH:MM:SS` it lapses at; `None`: until cancelled.
    #[serde(default)]
    pub valid_until: Option<String>,
    pub placed_at: String,
    #[serde(default, skip_serializing_if = "PerpLevels::is_empty")]
    pub levels: PerpLevels,
}

impl PerpLine {
    pub fn crossed(&self, mark: f64) -> bool {
        if self.rising {
            mark >= self.price
        } else {
            mark <= self.price
        }
    }
}

/// One account's perps: its positions (one per market), resting orders and
/// line orders.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Book {
    #[serde(default)]
    pub positions: Vec<PerpPosition>,
    #[serde(default)]
    pub orders: Vec<PerpOrder>,
    #[serde(default)]
    pub lines: Vec<PerpLine>,
    /// The last funding hour paid (Unix hours); `None` before the first.
    #[serde(default)]
    pub funded_hour: Option<i64>,
}

impl Book {
    pub fn position(&self, market: &str) -> Option<&PerpPosition> {
        self.positions.iter().find(|p| p.market == market)
    }

    pub fn position_mut(&mut self, market: &str) -> Option<&mut PerpPosition> {
        self.positions.iter_mut().find(|p| p.market == market)
    }

    /// Nothing open, resting or armed.
    pub fn is_flat(&self) -> bool {
        self.positions.is_empty() && self.orders.is_empty() && self.lines.is_empty()
    }
}

/// One fill, for the ledger.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Leg {
    pub position_id: String,
    pub market: String,
    /// `BUY` or `SELL`.
    pub action: &'static str,
    pub size: f64,
    pub price: f64,
    pub fee: f64,
    /// P&L the fill closed, before the fee (0 for an opening fill).
    pub realized: f64,
    /// The funding paid (negative: received) on what it closed: already
    /// out of the margin, so already in the cash; kept for the P&L.
    pub funding: f64,
    pub reason: String,
}

/// A fill's effect: its legs, and what it did to the cash (margin put up or
/// released, P&L, fees).
#[derive(Debug, Clone, PartialEq)]
pub struct Filled {
    pub legs: Vec<Leg>,
    pub cash_delta: f64,
}

/// What a fill is.
pub struct FillReq<'a> {
    pub market: &'a Market,
    /// `Long` buys, `Short` sells.
    pub side: Side,
    pub size: f64,
    pub price: f64,
    /// The fee rate: the market's taker or maker fee.
    pub fee_rate: f64,
    /// For a new position (an existing one keeps its own).
    pub leverage: u32,
    pub reduce_only: bool,
    pub reason: &'a str,
    pub now: &'a str,
    /// The stop and targets a position it opens or adds to takes (set
    /// ones only: an existing stop is replaced, targets are added).
    pub levels: Option<&'a PerpLevels>,
}

/// Apply a fill to `book` with `cash` free USDC: open, add to, reduce,
/// close or flip the market's position. Refused — nothing changed — when
/// the cash can't put up the margin and fee, a reduce-only fill has nothing
/// to reduce, or the size is nothing. All or nothing: a flip whose new side
/// can't be afforded leaves the old position as it was too.
pub fn apply_fill(book: &mut Book, cash: f64, req: &FillReq, new_id: impl Fn() -> String) -> Result<Filled, String> {
    let mut work = book.clone();
    let filled = fill_into(&mut work, cash, req, new_id)?;
    *book = work;
    Ok(filled)
}

/// [`apply_fill`]'s work, on a book it may leave half-changed when it refuses.
fn fill_into(book: &mut Book, cash: f64, req: &FillReq, new_id: impl Fn() -> String) -> Result<Filled, String> {
    let m = req.market;
    if !(req.size > DUST && req.price.is_finite() && req.price > 0.0) {
        return Err("Nothing to fill".to_string());
    }
    let mut legs = Vec::new();
    let mut cash_delta = 0.0;
    let mut left = req.size;

    // Against the position: reduce it first.
    if let Some(i) = book.positions.iter().position(|p| p.market == m.key && p.side != req.side) {
        let p = &mut book.positions[i];
        let r = left.min(p.size);
        let pnl = p.side.sign() * r * (req.price - p.entry);
        let released = p.margin * r / p.size;
        let fee = r * req.price * req.fee_rate;
        // Isolated: a loss never takes more than the margin behind it.
        let realized = pnl.max(-released);
        let funding = p.funding * r / p.size;
        cash_delta += released + realized - fee;
        p.size -= r;
        p.margin -= released;
        p.funding -= funding;
        p.fees += fee;
        legs.push(Leg { position_id: p.id.clone(), market: m.key.to_string(), action: req.side.action(), size: r, price: req.price, fee, realized, funding, reason: req.reason.to_string() });
        left -= r;
        if p.size <= DUST {
            book.positions.remove(i);
        } else {
            let (side, entry, size) = (p.side, p.entry, p.size);
            p.levels.trim_to(size, side, entry);
        }
    } else if req.reduce_only {
        return Err(format!("No {} position to reduce", m.label));
    }

    if left > DUST && !req.reduce_only {
        let notional = left * req.price;
        let fee = notional * req.fee_rate;
        let existing = book.positions.iter().position(|p| p.market == m.key);
        let leverage = existing.map_or(req.leverage, |i| book.positions[i].leverage).clamp(1, m.leverage_cap());
        let margin = notional / f64::from(leverage);
        if cash + cash_delta < margin + fee - 1e-9 {
            return Err(format!(
                "Not enough USDC: {} at {leverage}x needs ${:.2} margin and ${:.2} fee, ${:.2} is free",
                m.label,
                margin,
                fee,
                (cash + cash_delta).max(0.0)
            ));
        }
        cash_delta -= margin + fee;
        let id = match existing {
            Some(i) => {
                let p = &mut book.positions[i];
                p.entry = (p.entry * p.size + req.price * left) / (p.size + left);
                p.size += left;
                p.margin += margin;
                p.fees += fee;
                if let Some(levels) = req.levels {
                    take_levels(&mut p.levels, levels);
                }
                p.id.clone()
            }
            None => {
                let mut levels = PerpLevels::default();
                if let Some(given) = req.levels {
                    take_levels(&mut levels, given);
                }
                let p = PerpPosition {
                    id: new_id(),
                    market: m.key.to_string(),
                    side: req.side,
                    size: left,
                    entry: req.price,
                    leverage,
                    margin,
                    funding: 0.0,
                    fees: fee,
                    opened_at: req.now.to_string(),
                    levels,
                };
                let id = p.id.clone();
                book.positions.push(p);
                id
            }
        };
        legs.push(Leg { position_id: id, market: m.key.to_string(), action: req.side.action(), size: left, price: req.price, fee, realized: 0.0, funding: 0.0, reason: req.reason.to_string() });
    }
    Ok(Filled { legs, cash_delta })
}

/// `given`'s stop replaces the position's; its targets are added.
fn take_levels(levels: &mut PerpLevels, given: &PerpLevels) {
    if given.stop.is_some() {
        levels.stop = given.stop;
    }
    levels.targets.extend(given.targets.iter().cloned());
}

/// What `p` pays in funding for one hour at hourly `rate` with the oracle at
/// `oracle` (negative: it receives). A long pays a positive rate.
pub fn funding_payment(p: &PerpPosition, oracle: f64, rate: f64) -> f64 {
    p.side.sign() * p.size * oracle * rate
}

/// What the watch loop does to a position at `mark`, most urgent first.
#[derive(Debug, Clone, PartialEq)]
pub enum Exit {
    /// Liquidated: closed at the mark.
    Liquidation,
    /// Its stop: close it all.
    Stop,
    /// Targets reached: close `size` (all when it is the whole position).
    Targets { ids: Vec<String>, size: f64 },
}

/// Whether `p` closes, or partly closes, at `mark`. Stops and targets fire
/// on the mark, as Hyperliquid's triggers do; a stop already past the price
/// when set closes at once (a quick exit by dragging).
pub fn exit_at(p: &PerpPosition, m: &Market, mark: f64) -> Option<Exit> {
    if p.liquidated_at(m, mark) {
        return Some(Exit::Liquidation);
    }
    if let Some(stop) = p.levels.stop {
        let hit = match p.side {
            Side::Long => mark <= stop,
            Side::Short => mark >= stop,
        };
        if hit {
            return Some(Exit::Stop);
        }
    }
    let reached: Vec<&PerpTarget> = p
        .levels
        .targets
        .iter()
        .filter(|t| match p.side {
            Side::Long => mark >= t.price,
            Side::Short => mark <= t.price,
        })
        .collect();
    if reached.is_empty() {
        return None;
    }
    let size = if reached.iter().any(|t| t.size.is_none()) {
        p.size
    } else {
        reached.iter().filter_map(|t| t.size).sum::<f64>().min(p.size)
    };
    Some(Exit::Targets { ids: reached.iter().map(|t| t.id.clone()).collect(), size })
}

/// Why a stop or target at `price` is on the wrong side of `reference` for a
/// position (or an order to open one) on `side`: a long's stop below, its
/// target above; the reverse for a short. Used for line orders, whose levels
/// are set against the line before anything is held.
pub fn wrong_side(kind: &str, side: Side, reference: f64, price: f64) -> Option<String> {
    let above = price > reference;
    let ok = match (kind, side) {
        ("stop", Side::Long) | ("target", Side::Short) => !above,
        _ => above,
    };
    (!ok).then(|| format!("a {} {kind} goes {} the entry", side.word(), if above { "below" } else { "above" }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hyperliquid::MARKETS;

    fn btc() -> &'static Market {
        &MARKETS[0]
    }

    fn req<'a>(side: Side, size: f64, price: f64, reduce_only: bool) -> FillReq<'a> {
        FillReq { market: btc(), side, size, price, fee_rate: btc().taker_fee, leverage: 10, reduce_only, reason: "MARKET", now: "2026-11-19 10:00:00", levels: None }
    }

    fn ids() -> impl Fn() -> String {
        || "p1".to_string()
    }

    #[test]
    fn a_buy_opens_a_long_on_isolated_margin_and_a_sell_closes_it_with_its_pnl() {
        let mut book = Book::default();
        // 0.1 BTC at 60,000 at 10x: $600 margin, $2.70 fee.
        let f = apply_fill(&mut book, 10_000.0, &req(Side::Long, 0.1, 60_000.0, false), ids()).unwrap();
        assert!((f.cash_delta + 602.7).abs() < 1e-9);
        let p = book.position("BTC").unwrap();
        assert_eq!((p.side, p.size, p.entry, p.leverage), (Side::Long, 0.1, 60_000.0, 10));
        assert!((p.margin - 600.0).abs() < 1e-9);
        // Sold at 61,000: +$100, the margin back, a $2.745 fee.
        let f = apply_fill(&mut book, 10_000.0 - 602.7, &req(Side::Short, 0.1, 61_000.0, false), ids()).unwrap();
        assert!((f.legs[0].realized - 100.0).abs() < 1e-9);
        assert!((f.cash_delta - (600.0 + 100.0 - 2.745)).abs() < 1e-9);
        assert!(book.positions.is_empty());
    }

    #[test]
    fn adding_averages_the_entry_and_a_bigger_opposite_fill_flips_the_position() {
        let mut book = Book::default();
        apply_fill(&mut book, 10_000.0, &req(Side::Long, 0.1, 60_000.0, false), ids()).unwrap();
        apply_fill(&mut book, 9_000.0, &req(Side::Long, 0.1, 62_000.0, false), ids()).unwrap();
        let p = book.position("BTC").unwrap();
        assert!((p.entry - 61_000.0).abs() < 1e-9 && (p.size - 0.2).abs() < 1e-12);
        // Sell 0.3: closes the 0.2 long, opens a 0.1 short.
        let f = apply_fill(&mut book, 8_000.0, &req(Side::Short, 0.3, 61_500.0, false), || "p2".to_string()).unwrap();
        assert_eq!(f.legs.len(), 2);
        assert!((f.legs[0].realized - 100.0).abs() < 1e-6);
        let p = book.position("BTC").unwrap();
        assert_eq!((p.id.as_str(), p.side), ("p2", Side::Short));
        assert!((p.size - 0.1).abs() < 1e-12);
    }

    #[test]
    fn reduce_only_never_opens_or_flips_and_cash_bounds_every_open() {
        let mut book = Book::default();
        assert!(apply_fill(&mut book, 10_000.0, &req(Side::Short, 0.1, 60_000.0, true), ids()).is_err());
        apply_fill(&mut book, 10_000.0, &req(Side::Long, 0.1, 60_000.0, false), ids()).unwrap();
        let f = apply_fill(&mut book, 9_000.0, &req(Side::Short, 0.5, 60_000.0, true), ids()).unwrap();
        assert_eq!(f.legs.len(), 1, "only what is held");
        assert!(book.positions.is_empty());
        // $100 can't put up 0.1 BTC at 10x.
        let err = apply_fill(&mut book, 100.0, &req(Side::Long, 0.1, 60_000.0, false), ids()).unwrap_err();
        assert!(err.contains("Not enough USDC"), "{err}");
        assert!(book.positions.is_empty(), "a refusal changes nothing");
    }

    #[test]
    fn a_flip_that_cant_be_afforded_changes_nothing() {
        let mut book = Book::default();
        apply_fill(&mut book, 10_000.0, &req(Side::Long, 0.1, 60_000.0, false), ids()).unwrap();
        let before = book.clone();
        // Sell 2 BTC: closes the 0.1 long, but the 1.9 short at 10x needs
        // $11,400 of margin and only $9,397 (+ the long's $600) is free.
        let err = apply_fill(&mut book, 9_397.3, &req(Side::Short, 2.0, 60_000.0, false), ids()).unwrap_err();
        assert!(err.contains("Not enough USDC"), "{err}");
        assert_eq!(book, before, "the long is still held, its margin still behind it");
        // The same sell, reduce-only, is only the close.
        let f = apply_fill(&mut book, 9_397.3, &req(Side::Short, 2.0, 60_000.0, true), ids()).unwrap();
        assert_eq!(f.legs.len(), 1);
        assert!(book.positions.is_empty());
    }

    #[test]
    fn an_isolated_loss_never_takes_more_than_its_margin() {
        let mut book = Book::default();
        apply_fill(&mut book, 10_000.0, &req(Side::Long, 0.1, 60_000.0, false), ids()).unwrap();
        // A gap far through the liquidation price: −$1,000 on $600 of margin.
        let f = apply_fill(&mut book, 9_397.3, &req(Side::Short, 0.1, 50_000.0, false), ids()).unwrap();
        assert!((f.legs[0].realized + 600.0).abs() < 1e-9);
        assert!((f.cash_delta - (0.0 - f.legs[0].fee)).abs() < 1e-9);
    }

    #[test]
    fn the_liquidation_price_is_where_margin_plus_pnl_meets_maintenance() {
        let m = btc();
        let mut book = Book::default();
        apply_fill(&mut book, 10_000.0, &req(Side::Long, 1.0, 60_000.0, false), ids()).unwrap();
        let p = book.position("BTC").unwrap().clone();
        let liq = p.liq_price(m).unwrap();
        // At the liquidation price, margin + P&L equals 1.25 % of the notional.
        assert!(((p.margin + p.upnl(liq)) - m.maintenance() * p.size * liq).abs() < 1e-6);
        assert!(liq > 54_000.0 && liq < 55_000.0, "{liq}");
        assert!(p.liquidated_at(m, liq - 1.0) && !p.liquidated_at(m, liq + 1.0));
        assert_eq!(exit_at(&p, m, liq - 1.0), Some(Exit::Liquidation));
        // A short's is above.
        let mut short = p.clone();
        short.side = Side::Short;
        let liq = short.liq_price(m).unwrap();
        assert!(liq > 60_000.0 && short.liquidated_at(m, liq + 1.0));
        // A 1x long can't be liquidated.
        let mut safe = p.clone();
        safe.margin = safe.entry * safe.size;
        assert_eq!(safe.liq_price(m), None);
    }

    #[test]
    fn funding_is_paid_by_longs_on_a_positive_rate_and_received_on_a_negative_one() {
        let mut book = Book::default();
        apply_fill(&mut book, 10_000.0, &req(Side::Long, 1.0, 60_000.0, false), ids()).unwrap();
        let p = book.position("BTC").unwrap();
        assert!((funding_payment(p, 60_000.0, 0.0001) - 6.0).abs() < 1e-9);
        let mut short = p.clone();
        short.side = Side::Short;
        assert!((funding_payment(&short, 60_000.0, 0.0001) + 6.0).abs() < 1e-9);
    }

    #[test]
    fn stops_and_targets_fire_on_the_mark_most_urgent_first() {
        let m = btc();
        let mut book = Book::default();
        apply_fill(&mut book, 10_000.0, &req(Side::Long, 1.0, 60_000.0, false), ids()).unwrap();
        let mut p = book.position("BTC").unwrap().clone();
        p.levels = PerpLevels {
            stop: Some(59_000.0),
            targets: vec![
                PerpTarget { id: "t1".into(), price: 61_000.0, size: Some(0.4) },
                PerpTarget { id: "t2".into(), price: 62_000.0, size: None },
            ],
        };
        assert_eq!(exit_at(&p, m, 60_000.0), None);
        assert_eq!(exit_at(&p, m, 58_999.0), Some(Exit::Stop));
        assert_eq!(exit_at(&p, m, 61_000.0), Some(Exit::Targets { ids: vec!["t1".into()], size: 0.4 }));
        assert_eq!(exit_at(&p, m, 62_500.0), Some(Exit::Targets { ids: vec!["t1".into(), "t2".into()], size: 1.0 }), "all left");
        // A short's are mirrored.
        p.side = Side::Short;
        p.levels = PerpLevels { stop: Some(61_000.0), targets: vec![PerpTarget { id: "t".into(), price: 59_000.0, size: None }] };
        assert_eq!(exit_at(&p, m, 61_000.0), Some(Exit::Stop));
        assert_eq!(exit_at(&p, m, 58_900.0), Some(Exit::Targets { ids: vec!["t".into()], size: 1.0 }));
    }

    #[test]
    fn a_partial_close_trims_the_farthest_target_first() {
        let mut levels = PerpLevels {
            stop: None,
            targets: vec![
                PerpTarget { id: "near".into(), price: 61_000.0, size: Some(0.5) },
                PerpTarget { id: "far".into(), price: 63_000.0, size: Some(0.5) },
                PerpTarget { id: "rest".into(), price: 64_000.0, size: None },
            ],
        };
        levels.trim_to(0.7, Side::Long, 60_000.0);
        let sizes: Vec<(&str, Option<f64>)> = levels.targets.iter().map(|t| (t.id.as_str(), t.size)).collect();
        assert_eq!(sizes[0], ("near", Some(0.5)));
        assert!((sizes[1].1.unwrap() - 0.2).abs() < 1e-9 && sizes[1].0 == "far");
        assert_eq!(sizes[2], ("rest", None));
    }

    #[test]
    fn orders_and_lines_trigger_on_their_side() {
        let buy = PerpOrder { id: "o".into(), market: "BTC".into(), side: Side::Long, size: 0.1, price: 60_000.0, leverage: 5, reduce_only: false, placed_at: String::new(), levels: PerpLevels::default() };
        assert!(buy.reached(59_999.0) && !buy.reached(60_001.0));
        let sell = PerpOrder { side: Side::Short, ..buy.clone() };
        assert!(sell.reached(60_001.0) && !sell.reached(59_999.0));
        let line = PerpLine { id: "l".into(), market: "BTC".into(), side: Side::Long, size: 0.1, leverage: 5, price: 61_000.0, rising: true, valid_until: None, placed_at: String::new(), levels: PerpLevels::default() };
        assert!(!line.crossed(60_999.0) && line.crossed(61_000.0));
        let falling = PerpLine { rising: false, ..line };
        assert!(falling.crossed(60_999.0) && !falling.crossed(61_001.0));
        assert!(wrong_side("stop", Side::Long, 61_000.0, 60_000.0).is_none());
        assert!(wrong_side("stop", Side::Long, 61_000.0, 62_000.0).is_some());
        assert!(wrong_side("target", Side::Short, 61_000.0, 60_000.0).is_none());
        assert!(wrong_side("target", Side::Short, 61_000.0, 62_000.0).is_some());
    }
}
