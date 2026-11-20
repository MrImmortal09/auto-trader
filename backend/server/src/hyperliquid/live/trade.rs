//! LIVE orders on Hyperliquid, the owner's alone (the owner's picks,
//! 2026-11-19 and 2026-11-20):
//!
//! - A market order, and every close, is an IOC limit at the trading
//!   network's own mid ± 0.5 %. A close that doesn't fully fill is tried
//!   again up to three more times at a fresh mid, still ± 0.5 %, and then
//!   reported loudly.
//! - A limit is a GTC order resting on Hyperliquid.
//! - Stops and targets are Hyperliquid's own TP/SL triggers (market when
//!   they fire), resting on Hyperliquid: they fire while this server is down.
//! - A line order is a trigger order that opens a position once the mark
//!   crosses it, until cancelled (no expiry in LIVE).
//! - Isolated margin at the ticket's leverage, set before an order opens a
//!   position; never more than the app's 20x.
//!
//! Hyperliquid holds the account: nothing about a LIVE order is kept here.
//! After every action the account is read again ([`LiveDesk::account`]).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use hyperliquid_client::exchange::OrderStatus;
use hyperliquid_client::account::HlOpenOrder;
use hyperliquid_client::signing::{float_to_wire, CancelAction, CancelWire, Grouping, LeverageAction, ModifyAction, ModifyWire, OrderAction, OrderTypeWire, OrderWire, Tif, Wallet};
use hyperliquid_client::{asset_id, HlError, Network};
use serde::Serialize;

use super::{net_str, timed, LiveDesk};
use crate::hyperliquid::paper::{LevelsChange, Refusal};
use crate::hyperliquid::perp::{Side, TRIGGER_SLIP};
use crate::hyperliquid::{round_price, round_size, Market, MAX_LEVERAGE};

/// How far from the mid a market order or close may fill: 0.5 %.
pub const SLIPPAGE: f64 = 0.005;
/// A close that doesn't fully fill is sent again this many more times.
pub const CLOSE_RETRIES: usize = 3;
/// How long a network's perps (asset ids, size grids) are reused.
const ASSETS_TTL: Duration = Duration::from_secs(10 * 60);
/// One exchange action that hasn't answered in this long is reported as
/// unanswered — it may still have gone through, so the account is read.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// A perp as one network's orders name it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NetAsset {
    pub asset: u32,
    pub sz_decimals: u32,
    pub max_leverage: u32,
}

impl NetAsset {
    pub fn size(&self, size: f64) -> f64 {
        round_size(size, self.sz_decimals)
    }
    pub fn price(&self, price: f64) -> f64 {
        round_price(price, self.sz_decimals)
    }
    /// The most leverage taken on it: the network's limit, at most 20x.
    pub fn leverage_cap(&self) -> u32 {
        self.max_leverage.min(MAX_LEVERAGE)
    }
}

/// A network's perps, by coin, with when they were read.
pub type NetAssets = (Instant, HashMap<String, NetAsset>);

/// A LIVE order as the ticket sends it.
#[derive(Debug, Clone)]
pub struct LiveOrder {
    pub market: &'static Market,
    pub side: Side,
    /// In the coin.
    pub size: f64,
    pub leverage: u32,
    /// `None`: a market order (IOC at the mid ± 0.5 %).
    pub limit: Option<f64>,
    pub reduce_only: bool,
    pub stop: Option<f64>,
    pub targets: Vec<(f64, Option<f64>)>,
}

/// One fill, in the paper desk's words (`market`, `action`, `size`, `price`).
#[derive(Debug, Clone, Serialize)]
pub struct LiveFill {
    pub market: String,
    pub action: &'static str,
    pub size: f64,
    pub price: f64,
}

/// What a LIVE order came to: what filled now, and what rests.
#[derive(Debug, Clone, Serialize, Default)]
pub struct LiveOrderAnswer {
    pub filled: Vec<LiveFill>,
    /// The oid resting on Hyperliquid (a limit, or a line order's trigger).
    pub resting: Option<u64>,
    /// Hyperliquid's words for a stop or target it refused, the order kept.
    pub warnings: Vec<String>,
}

fn wire(x: f64, what: &str) -> Result<String, Refusal> {
    float_to_wire(x).ok_or_else(|| Refusal::bad(format!("The {what} {x} can't be sent: too many decimals")))
}

/// `true` buys.
fn is_buy(side: Side) -> bool {
    side == Side::Long
}

/// The IOC price a market order crossing the book at `mid` is sent at.
pub fn ioc_price(a: &NetAsset, buy: bool, mid: f64) -> f64 {
    a.price(if buy { mid * (1.0 + SLIPPAGE) } else { mid * (1.0 - SLIPPAGE) })
}

/// A LIVE order's id: Hyperliquid's oid.
fn oid_of(id: &str) -> Result<u64, Refusal> {
    id.trim().parse().map_err(|_| Refusal::bad(format!("{id} isn't a Hyperliquid order id")))
}

fn hl_refusal(e: HlError) -> Refusal {
    Refusal::conflict(e.to_string())
}

impl LiveDesk {
    /// `coin` as `network`'s orders name it; a perp the network doesn't list
    /// is refused.
    pub(super) async fn net_asset(&self, network: Network, coin: &str) -> Result<NetAsset, Refusal> {
        let mut all = self.assets.lock().await;
        let fresh = all.get(&network).is_some_and(|(at, _)| at.elapsed() < ASSETS_TTL);
        if !fresh {
            let info = &self.info[&network];
            let dexes = timed("the dexes", info.perp_dexes()).await.map_err(Refusal::conflict)?;
            let mut map = HashMap::new();
            for (dex_index, dex) in dexes.iter().enumerate() {
                // The core perps, and each builder dex a listed perp trades on.
                let wanted = match dex.name.as_deref() {
                    None => true,
                    Some(d) => super::live_dexes().contains(&Some(d)),
                };
                if !wanted {
                    continue;
                }
                let rules = timed("the perps", info.perp_rules(dex.name.as_deref())).await.map_err(Refusal::conflict)?;
                for r in rules {
                    map.insert(r.coin.clone(), NetAsset { asset: asset_id(dex_index, r.index), sz_decimals: r.sz_decimals, max_leverage: r.max_leverage });
                }
            }
            all.insert(network, (Instant::now(), map));
        }
        all.get(&network)
            .and_then(|(_, m)| m.get(coin).copied())
            .ok_or_else(|| Refusal::conflict(format!("{coin} isn't listed on Hyperliquid {}", net_str(network))))
    }

    /// `coin`'s mid on `network` now: the network's own book, not the
    /// charts' (always mainnet).
    pub(super) async fn net_mid(&self, network: Network, coin: &str) -> Result<f64, Refusal> {
        let dex = hyperliquid_client::dex_of(coin);
        let mids = timed("the mid", self.info[&network].all_mids(dex)).await.map_err(Refusal::conflict)?;
        mids.get(coin).copied().filter(|p| *p > 0.0).ok_or_else(|| Refusal::conflict(format!("No mid for {coin} on Hyperliquid {} — not trading blind", net_str(network))))
    }

    fn next_nonce(&self) -> u64 {
        self.nonces.next(shared_domain::now_ist().timestamp_millis().max(0) as u64)
    }

    /// The account is about to change: the next read is fresh.
    pub(super) async fn forget(&self, network: Network) {
        if let Some(c) = self.cache.lock().await.get_mut(&network) {
            c.states.clear();
            c.orders = None;
            c.fills = None;
            c.failed = None;
            c.spot = None;
            c.available.clear();
            // What an order changes is read again at once.
            c.tried.retain(|k, _| !(k == "spot" || k.starts_with("available:")));
        }
    }

    /// Send an order, cancel or modify action and read its statuses. An
    /// action that doesn't answer may still have gone through: it says so.
    pub(super) async fn send<A: Serialize>(&self, network: Network, wallet: &Wallet, action: &A) -> Result<Vec<OrderStatus>, Refusal> {
        let nonce = self.next_nonce();
        let sent = tokio::time::timeout(SEND_TIMEOUT, self.exchange[&network].orders(wallet, action, nonce)).await;
        self.forget(network).await;
        match sent {
            Err(_) => Err(Refusal::conflict(format!("Hyperliquid didn't answer in {} s — the order may still have gone through: check the account before sending it again", SEND_TIMEOUT.as_secs()))),
            Ok(Err(HlError::Refused(why))) => Err(Refusal::conflict(format!("Hyperliquid refused it: {why}"))),
            // Refused before it left: nothing was sent.
            Ok(Err(e @ (HlError::Key(_) | HlError::Encode(_)))) => Err(hl_refusal(e)),
            // A gateway error, or an answer that couldn't be read: it may have been taken.
            Ok(Err(e)) => Err(Refusal::conflict(format!("{e} — the order may still have gone through: check the account before sending it again"))),
            Ok(Ok(statuses)) => Ok(statuses),
        }
    }

    /// Isolated margin at `leverage` on `asset`, before an order that opens.
    pub(super) async fn set_leverage(&self, network: Network, wallet: &Wallet, a: &NetAsset, leverage: u32) -> Result<(), Refusal> {
        let nonce = self.next_nonce();
        let action = LeverageAction::isolated(a.asset, leverage);
        match tokio::time::timeout(SEND_TIMEOUT, self.exchange[&network].act(wallet, &action, nonce)).await {
            Err(_) => Err(Refusal::conflict("Hyperliquid didn't answer setting the leverage — nothing sent")),
            Ok(Err(e)) => Err(Refusal::conflict(format!("Leverage not set to {leverage}x isolated ({e}) — nothing sent"))),
            Ok(Ok(())) => Ok(()),
        }
    }

    /// The account's open orders on `coin` (its dex's), read fresh.
    async fn orders_on(&self, network: Network, account: &str, coin: &str) -> Result<Vec<HlOpenOrder>, Refusal> {
        let dex = hyperliquid_client::dex_of(coin);
        let all = timed("the open orders", self.info[&network].open_orders(account, dex)).await.map_err(Refusal::conflict)?;
        Ok(all.into_iter().filter(|o| o.coin == coin).collect())
    }

    /// Isolated `leverage` on `coin` before an order that opens a position.
    /// Hyperliquid keeps one leverage per coin, used by every order on it:
    /// while orders that open rest there, a different one would change
    /// theirs, so it is refused.
    #[allow(clippy::too_many_arguments)]
    async fn ensure_leverage(&self, network: Network, account: &str, wallet: &Wallet, a: &NetAsset, coin: &str, label: &str, leverage: u32) -> Result<(), Refusal> {
        let resting = self.orders_on(network, account, coin).await?.into_iter().filter(|o| !o.reduce_only).count();
        if resting > 0 {
            let (isolated, now) = timed("the leverage", self.info[&network].asset_leverage(account, coin)).await.map_err(Refusal::conflict)?;
            if isolated && now == leverage {
                return Ok(());
            }
            return Err(Refusal::conflict(format!(
                "{label} has {resting} order(s) resting at {now}x{}: leverage is one per coin on Hyperliquid, so use {now}x or cancel them first",
                if isolated { "" } else { " cross" }
            )));
        }
        self.set_leverage(network, wallet, a, leverage).await
    }

    /// One stop for the whole position: `size` (all that is held) at
    /// `price`, placed before every older stop on the coin is cancelled —
    /// never a moment with less covered. A stop that can't be cancelled
    /// (it fired, or is gone) is said, not failed on.
    #[allow(clippy::too_many_arguments)]
    async fn set_full_stop(&self, network: Network, wallet: &Wallet, a: &NetAsset, long: bool, size: f64, price: f64, old: &[HlOpenOrder]) -> Result<Vec<String>, Refusal> {
        self.place_guard(network, wallet, a, !long, size, price, "sl").await?;
        let mut warnings = Vec::new();
        let cancels: Vec<CancelWire> = old.iter().map(|o| CancelWire { a: a.asset, o: o.oid }).collect();
        if !cancels.is_empty() {
            match self.send(network, wallet, &CancelAction::new(cancels)).await {
                Ok(st) => warnings.extend(st.iter().filter_map(|s| match s {
                    OrderStatus::Error(why) => Some(format!("An older stop wasn't cancelled: {why}")),
                    _ => None,
                })),
                Err(r) => warnings.push(format!("The older stops weren't cancelled: {}", r.message)),
            }
        }
        Ok(warnings)
    }

    /// The signed-in owner's LIVE wallet on `network`, with its account.
    pub(super) async fn signer(&self, network: Network) -> Result<(String, Wallet), Refusal> {
        self.wallet(network).await
    }

    /// The position the account holds in `coin` on `network` now, read
    /// fresh: (size signed, + long / − short, its leverage).
    pub(super) async fn held(&self, network: Network, account: &str, coin: &str) -> Result<Option<(f64, u32)>, Refusal> {
        let dex = hyperliquid_client::dex_of(coin);
        let state = timed("the position", self.info[&network].account_state(account, dex)).await.map_err(Refusal::conflict)?;
        Ok(state.positions.iter().find(|p| p.coin == coin && p.szi != 0.0).map(|p| (p.szi, p.leverage)))
    }

    /// A market order or a limit, with its stop and targets. Market: an IOC
    /// at the network's mid ± 0.5 %. Isolated leverage is set first when it
    /// opens a position. Levels ride only an order that opens; a market
    /// order that adds leaves one stop over the whole position.
    pub async fn order(&self, network: Network, o: &LiveOrder) -> Result<LiveOrderAnswer, Refusal> {
        let (account, wallet) = self.signer(network).await?;
        let m = o.market;
        let a = self.net_asset(network, m.coin).await?;
        let size = a.size(o.size);
        if size <= 0.0 {
            return Err(Refusal::bad(format!("A {} size carries at most {} decimals and must be more than nothing", m.label, a.sz_decimals)));
        }
        let buy = is_buy(o.side);
        let held = self.held(network, &account, m.coin).await?;
        let adds = held.is_some_and(|(szi, _)| (szi > 0.0) == buy);
        let opens = !o.reduce_only && held.is_none_or(|(szi, _)| (szi > 0.0) == buy || size > szi.abs() + 1e-12);
        if opens && !m.tradable() {
            return Err(close_only(m));
        }
        let leverage = match held {
            // An add rides at the position's own leverage.
            Some((_, lev)) if adds => lev,
            _ => o.leverage,
        };
        if opens && !(1..=a.leverage_cap()).contains(&leverage) {
            return Err(Refusal::bad(format!("Leverage must be 1x to {}x on {}", a.leverage_cap(), m.label)));
        }
        let mid = self.net_mid(network, m.coin).await?;
        let price = match o.limit {
            None => ioc_price(&a, buy, mid),
            Some(p) if p.is_finite() && p > 0.0 => a.price(p),
            Some(_) => return Err(Refusal::bad("A limit's price must be a positive number")),
        };
        if !o.reduce_only && size * price < crate::hyperliquid::perp::MIN_NOTIONAL {
            return Err(Refusal::bad(format!("An order must be worth at least ${:.0}", crate::hyperliquid::perp::MIN_NOTIONAL)));
        }
        let market_order = o.limit.is_none();
        // Levels ride only an order that opens (paper ignores them on a fill
        // that only reduces).
        let (stop, targets) = if opens {
            check_sides(o.side, price_ref(o.limit, mid), o.stop, &o.targets)?;
            entry_levels(o.stop, &o.targets)?
        } else {
            (None, Vec::new())
        };
        // A resting add can't carry levels of its own: Hyperliquid would stack
        // a second, fixed-size stop. The position's stop covers it once moved.
        if adds && !market_order && (stop.is_some() || !targets.is_empty()) {
            return Err(Refusal::bad("A LIVE limit that adds to a position takes no stop or target of its own: once it fills, set the position's stop again and it covers all of it"));
        }
        if opens && !adds && held.is_none() {
            self.ensure_leverage(network, &account, &wallet, &a, m.coin, m.label, leverage).await?;
        }
        let tif = if market_order { Tif::Ioc } else { Tif::Gtc };
        let entry = OrderWire { a: a.asset, b: buy, p: wire(price, "price")?, s: wire(size, "size")?, r: o.reduce_only, t: OrderTypeWire::limit(tif), c: None };
        // A limit carries its stop and target as children (`normalTpsl`),
        // placed by Hyperliquid once it fills. A market order goes alone —
        // children wait for a full fill, which an IOC may not get — and its
        // stop and target follow, sized to what filled.
        let mut orders = vec![entry];
        if !market_order {
            orders.extend(levels_wire(&a, buy, size, stop, &targets)?);
        }
        let grouping = if orders.len() > 1 { Grouping::NormalTpsl } else { Grouping::Na };
        let statuses = self.send(network, &wallet, &OrderAction::new(orders, grouping)).await?;
        let mut answer = LiveOrderAnswer::default();
        for (i, st) in statuses.iter().enumerate() {
            match (i, st) {
                (0, OrderStatus::Filled { size, avg_px, .. }) => answer.filled.push(LiveFill { market: m.key.to_string(), action: if buy { "BUY" } else { "SELL" }, size: *size, price: *avg_px }),
                (0, OrderStatus::Resting { oid }) => answer.resting = Some(*oid),
                (0, OrderStatus::Error(why)) => return Err(Refusal::conflict(format!("{} not {}: {why}", m.label, if market_order { "filled" } else { "placed" }))),
                (_, OrderStatus::Error(why)) => answer.warnings.push(why.clone()),
                _ => {}
            }
        }
        let filled: f64 = answer.filled.iter().map(|f| f.size).sum();
        if market_order && filled > 0.0 {
            if let Err(r) = self.guard_after_fill(network, &account, &wallet, &a, m, buy, a.size(filled), adds, stop, &targets, &mut answer).await {
                answer.warnings.push(format!("The position's stop and target weren't set: {}", r.message));
            }
        }
        if !answer.warnings.is_empty() {
            tracing::error!(network = net_str(network), market = m.key, warnings = ?answer.warnings, "Hyperliquid LIVE order: a stop or target wasn't placed");
        }
        tracing::warn!(network = net_str(network), market = m.key, buy, size, price, ?answer, "Hyperliquid LIVE order");
        Ok(answer)
    }

    /// After a market order filled `filled`: guards that no longer close the
    /// position (it flipped) are cancelled; its stop then covers all of it —
    /// the order's own stop, or for an add with none the stop it had,
    /// re-placed for the new size; its target goes on for what filled.
    #[allow(clippy::too_many_arguments)]
    async fn guard_after_fill(&self, network: Network, account: &str, wallet: &Wallet, a: &NetAsset, m: &'static Market, buy: bool, filled: f64, adds: bool, stop: Option<f64>, targets: &[(f64, Option<f64>)], answer: &mut LiveOrderAnswer) -> Result<(), Refusal> {
        let Some((szi, _)) = self.held(network, account, m.coin).await? else {
            return Ok(());
        };
        let long = szi > 0.0;
        let guards: Vec<HlOpenOrder> = self.orders_on(network, account, m.coin).await?.into_iter().filter(|o| o.is_trigger && o.reduce_only).collect();
        // A guard on the side that would add to the position is a flipped one's: gone.
        let stale: Vec<CancelWire> = guards.iter().filter(|o| o.is_buy == long).map(|o| CancelWire { a: a.asset, o: o.oid }).collect();
        if !stale.is_empty() {
            if let Err(r) = self.send(network, wallet, &CancelAction::new(stale)).await {
                answer.warnings.push(format!("The old side's stops weren't cancelled: {}", r.message));
            }
        }
        let stops: Vec<HlOpenOrder> = guards.into_iter().filter(|o| o.is_buy != long && o.is_stop()).collect();
        let stop_at = stop.map(|p| a.price(p)).or(if adds { stops.first().and_then(|o| o.trigger_px) } else { None });
        if let Some(p) = stop_at {
            match self.set_full_stop(network, wallet, a, long, szi.abs(), p, &stops).await {
                Ok(w) => answer.warnings.extend(w),
                Err(r) => answer.warnings.push(format!("Stop not placed — the position has none for its new size: {}", r.message)),
            }
        }
        // `buy` opened (or added to) the position this way: its targets close it.
        let opened = if adds || (szi > 0.0) == buy { filled.min(szi.abs()) } else { 0.0 };
        if opened > 0.0 {
            // `levels_wire` takes the side the position is on and closes it from the other.
            for o in levels_wire(a, long, opened, None, targets)? {
                match self.send(network, wallet, &OrderAction::new(vec![o], Grouping::Na)).await {
                    Ok(st) => {
                        if let Some(OrderStatus::Error(why)) = st.first() {
                            answer.warnings.push(format!("Target not placed: {why}"));
                        }
                    }
                    Err(r) => answer.warnings.push(format!("Target not placed: {}", r.message)),
                }
            }
        }
        Ok(())
    }

    /// Close `size` of the position in `m` (all of it when `None`) with a
    /// reduce-only IOC at the network's mid ∓ 0.5 %; what doesn't fill is
    /// sent again at a fresh mid up to [`CLOSE_RETRIES`] more times.
    pub async fn close(&self, network: Network, m: &'static Market, size: Option<f64>) -> Result<LiveOrderAnswer, Refusal> {
        let (account, wallet) = self.signer(network).await?;
        self.close_coin(network, &account, &wallet, m.coin, m.key, m.label, size).await
    }

    /// [`LiveDesk::close`] by Hyperliquid's coin name: any perp the account
    /// holds, on the app's list or not (the kill switch closes them all).
    #[allow(clippy::too_many_arguments)]
    async fn close_coin(&self, network: Network, account: &str, wallet: &Wallet, coin: &str, key: &str, label: &str, size: Option<f64>) -> Result<LiveOrderAnswer, Refusal> {
        let a = self.net_asset(network, coin).await?;
        let (szi, _) = self.held(network, account, coin).await?.ok_or_else(|| Refusal::not_found(format!("No LIVE {label} position")))?;
        let buy = szi < 0.0;
        let mut left = a.size(size.map_or(szi.abs(), |s| s.min(szi.abs())));
        if left <= 0.0 {
            return Err(Refusal::bad("Close more than nothing"));
        }
        let mut answer = LiveOrderAnswer::default();
        let mut last_error = None;
        for attempt in 0..=CLOSE_RETRIES {
            let mid = self.net_mid(network, coin).await?;
            let price = ioc_price(&a, buy, mid);
            let order = OrderWire { a: a.asset, b: buy, p: wire(price, "price")?, s: wire(left, "size")?, r: true, t: OrderTypeWire::limit(Tif::Ioc), c: None };
            match self.send(network, wallet, &OrderAction::new(vec![order], Grouping::Na)).await?.first() {
                Some(OrderStatus::Filled { size, avg_px, .. }) => {
                    answer.filled.push(LiveFill { market: key.to_string(), action: if buy { "BUY" } else { "SELL" }, size: *size, price: *avg_px });
                    left = a.size(left - size);
                }
                Some(OrderStatus::Error(why)) => last_error = Some(why.clone()),
                other => last_error = Some(format!("unexpected answer {other:?}")),
            }
            if left <= 0.0 {
                break;
            }
            tracing::warn!(network = net_str(network), coin, attempt, left, "Hyperliquid LIVE close not fully filled — trying again at a fresh mid");
        }
        if left > 0.0 {
            let why = last_error.unwrap_or_else(|| "price moved past 0.5 %".into());
            tracing::error!(network = net_str(network), coin, left, "Hyperliquid LIVE close left {left} open after {} tries: {why}", CLOSE_RETRIES + 1);
            return Err(Refusal::conflict(format!("{label} not fully closed: {left} still open after {} tries at the mid ± 0.5 % ({why}) — close it on Hyperliquid", CLOSE_RETRIES + 1)));
        }
        Ok(answer)
    }

    /// The open order `oid` (a child stop or target too), read fresh.
    pub(super) async fn find_order(&self, network: Network, account: &str, oid: u64) -> Result<HlOpenOrder, Refusal> {
        let dexes = self.dexes_on(network).await.map_err(Refusal::conflict)?;
        for dex in dexes {
            let orders = timed("the open orders", self.info[&network].open_orders(account, dex)).await.map_err(Refusal::conflict)?;
            for o in orders {
                if o.oid == oid {
                    return Ok(o);
                }
                if let Some(c) = o.children.iter().find(|c| c.oid == oid) {
                    return Ok(c.clone());
                }
            }
        }
        Err(Refusal::not_found("No such order on Hyperliquid — it may have filled or been cancelled"))
    }

    /// Cancel the open order `id` (an oid): a limit, a line order, a stop
    /// or a target. A partly filled order's stop and target children go
    /// with it on Hyperliquid, so they are placed again for the part that
    /// filled. What needs saying comes back as warnings.
    pub async fn cancel(&self, network: Network, id: &str) -> Result<Vec<String>, Refusal> {
        let oid = oid_of(id)?;
        let (account, wallet) = self.signer(network).await?;
        let o = self.find_order(network, &account, oid).await?;
        let a = self.net_asset(network, &o.coin).await?;
        if let Some(OrderStatus::Error(why)) = self.send(network, &wallet, &CancelAction::new(vec![CancelWire { a: a.asset, o: oid }])).await?.first() {
            return Err(Refusal::conflict(format!("Not cancelled: {why}")));
        }
        Ok(self.guard_filled_part(network, &wallet, &a, &o).await)
    }

    /// `o` (just cancelled, or about to be placed again) filled in part: its
    /// stop and target children, cancelled with it, go on again for the
    /// part that filled, as reduce-only triggers.
    async fn guard_filled_part(&self, network: Network, wallet: &Wallet, a: &NetAsset, o: &HlOpenOrder) -> Vec<String> {
        let part = a.size(o.orig_sz - o.sz);
        if part <= 0.0 || o.children.is_empty() {
            return Vec::new();
        }
        let mut warnings = Vec::new();
        for c in &o.children {
            let Some(at) = c.trigger_px else { continue };
            let tpsl = if c.is_stop() { "sl" } else { "tp" };
            if let Err(r) = self.place_guard(network, wallet, a, c.is_buy, part, at, tpsl).await {
                warnings.push(format!("The part that filled ({part}) has no {}: {}", if tpsl == "sl" { "stop" } else { "target" }, r.message));
            }
        }
        warnings
    }

    /// Move the open order `id` to `price`, keeping its side, size and kind:
    /// a resting limit is modified in place; a stop or target is placed
    /// anew and then the old one cancelled (Hyperliquid's modify can't move
    /// a trigger safely, and two reduce-only guards for a moment are
    /// harmless); a line order is armed again (`update_line`).
    pub async fn move_to(&self, network: Network, id: &str, price: f64) -> Result<(), Refusal> {
        if !(price.is_finite() && price > 0.0) {
            return Err(Refusal::bad("The price must be a positive number"));
        }
        let oid = oid_of(id)?;
        let (account, wallet) = self.signer(network).await?;
        let o = self.find_order(network, &account, oid).await?;
        if o.is_trigger && !o.reduce_only {
            return self.update_line(network, id, Some(price), &LevelsChange::default()).await;
        }
        let a = self.net_asset(network, &o.coin).await?;
        let price = a.price(price);
        if o.is_trigger {
            return self.replace_guard(network, &wallet, &a, &o, price, o.sz).await;
        }
        if !o.reduce_only {
            if let Some(m) = Market::of_coin(&o.coin).filter(|m| !m.tradable()) {
                return Err(close_only(m));
            }
        }
        if o.children.is_empty() {
            let order = OrderWire { a: a.asset, b: o.is_buy, p: wire(price, "price")?, s: wire(o.sz, "size")?, r: o.reduce_only, t: OrderTypeWire::limit(Tif::Gtc), c: None };
            return match self.send(network, &wallet, &ModifyAction::new(vec![ModifyWire { oid, order }])).await?.first() {
                Some(OrderStatus::Error(why)) => Err(Refusal::conflict(format!("Limit not moved: {why}"))),
                _ => Ok(()),
            };
        }
        // A limit with its stop and target: Hyperliquid doesn't say a modify
        // keeps them, so it is cancelled and placed again with them (what
        // filled meanwhile gets its own, `guard_filled_part`).
        if let Some(OrderStatus::Error(why)) = self.send(network, &wallet, &CancelAction::new(vec![CancelWire { a: a.asset, o: oid }])).await?.first() {
            return Err(Refusal::conflict(format!("Limit not moved — it couldn't be cancelled ({why}); it may have filled")));
        }
        for w in self.guard_filled_part(network, &wallet, &a, &o).await {
            tracing::error!(network = net_str(network), coin = o.coin, "Hyperliquid LIVE: {w}");
        }
        let side = if o.is_buy { Side::Long } else { Side::Short };
        let stop = o.children.iter().find(|c| c.is_stop()).and_then(|c| c.trigger_px);
        let targets: Targets = o.children.iter().filter(|c| !c.is_stop()).filter_map(|c| c.trigger_px).map(|t| (t, None)).take(1).collect();
        check_sides(side, price, stop, &targets)?;
        let mut orders = vec![OrderWire { a: a.asset, b: o.is_buy, p: wire(price, "price")?, s: wire(o.sz, "size")?, r: false, t: OrderTypeWire::limit(Tif::Gtc), c: None }];
        orders.extend(levels_wire(&a, o.is_buy, o.sz, stop, &targets)?);
        match self.send(network, &wallet, &OrderAction::new(orders, Grouping::NormalTpsl)).await {
            Ok(st) => match st.first() {
                Some(OrderStatus::Error(why)) => Err(Refusal::conflict(format!("The limit was cancelled but not placed again: {why} — place it again from the ticket"))),
                _ => Ok(()),
            },
            Err(r) => Err(Refusal::conflict(format!("The limit was cancelled but not placed again: {} — check the account", r.message))),
        }
    }

    /// A stop or target `o` moved to `price` (and sized `size`): the new one
    /// placed first, then the old one cancelled — never a moment without a
    /// guard. An old one already gone (it fired, or was cancelled) is fine.
    async fn replace_guard(&self, network: Network, wallet: &Wallet, a: &NetAsset, o: &HlOpenOrder, price: f64, size: f64) -> Result<(), Refusal> {
        let tpsl = if o.is_stop() { "sl" } else { "tp" };
        self.place_guard(network, wallet, a, o.is_buy, size, price, tpsl).await?;
        if let Some(OrderStatus::Error(why)) = self.send(network, wallet, &CancelAction::new(vec![CancelWire { a: a.asset, o: o.oid }])).await?.first() {
            tracing::warn!(network = net_str(network), oid = o.oid, "Hyperliquid LIVE: the old {tpsl} wasn't cancelled after its replacement was placed: {why}");
        }
        Ok(())
    }

    /// Arm a line order on Hyperliquid: a market trigger that opens (or adds
    /// to) a position once the mark crosses `o.limit` (the line) from where
    /// it is now, with its stop and targets as children (`normalTpsl`), live
    /// once it fills. It rests until cancelled.
    pub async fn arm_line(&self, network: Network, o: &LiveOrder) -> Result<LiveOrderAnswer, Refusal> {
        let m = o.market;
        if !m.tradable() {
            return Err(close_only(m));
        }
        let (account, wallet) = self.signer(network).await?;
        let a = self.net_asset(network, m.coin).await?;
        let size = a.size(o.size);
        if size <= 0.0 {
            return Err(Refusal::bad(format!("A {} size carries at most {} decimals and must be more than nothing", m.label, a.sz_decimals)));
        }
        let line = match o.limit {
            Some(p) if p.is_finite() && p > 0.0 => a.price(p),
            _ => return Err(Refusal::bad("The line's price must be a positive number")),
        };
        if size * line < crate::hyperliquid::perp::MIN_NOTIONAL {
            return Err(Refusal::bad(format!("An order must be worth at least ${:.0}", crate::hyperliquid::perp::MIN_NOTIONAL)));
        }
        let buy = is_buy(o.side);
        let held = self.held(network, &account, m.coin).await?;
        let leverage = match held {
            Some((szi, lev)) if (szi > 0.0) == buy => lev,
            _ => o.leverage,
        };
        if !(1..=a.leverage_cap()).contains(&leverage) {
            return Err(Refusal::bad(format!("Leverage must be 1x to {}x on {}", a.leverage_cap(), m.label)));
        }
        check_sides(o.side, line, o.stop, &o.targets)?;
        let (stop, targets) = entry_levels(o.stop, &o.targets)?;
        let adds = held.is_some_and(|(szi, _)| (szi > 0.0) == buy);
        if adds && (stop.is_some() || !targets.is_empty()) {
            return Err(Refusal::bad("A LIVE line order that adds to a position takes no stop or target of its own: once it fills, set the position's stop again and it covers all of it"));
        }
        let mid = self.net_mid(network, m.coin).await?;
        let tpsl = entry_tpsl(buy, line, mid)?;
        if held.is_none() {
            self.ensure_leverage(network, &account, &wallet, &a, m.coin, m.label, leverage).await?;
        }
        let mut orders = vec![trigger_wire(&a, buy, size, line, tpsl, false)?];
        orders.extend(levels_wire(&a, buy, size, stop, &targets)?);
        let grouping = if orders.len() > 1 { Grouping::NormalTpsl } else { Grouping::Na };
        let statuses = self.send(network, &wallet, &OrderAction::new(orders, grouping)).await?;
        let mut answer = LiveOrderAnswer::default();
        for (i, st) in statuses.iter().enumerate() {
            match (i, st) {
                (0, OrderStatus::Resting { oid }) => answer.resting = Some(*oid),
                (0, OrderStatus::Filled { size, avg_px, .. }) => answer.filled.push(LiveFill { market: m.key.to_string(), action: if buy { "BUY" } else { "SELL" }, size: *size, price: *avg_px }),
                (0, OrderStatus::Error(why)) => return Err(Refusal::conflict(format!("{} line order not armed: {why}", m.label))),
                (_, OrderStatus::Error(why)) => answer.warnings.push(why.clone()),
                _ => {}
            }
        }
        tracing::warn!(network = net_str(network), market = m.key, buy, size, line, tpsl, ?answer, "Hyperliquid LIVE line order");
        Ok(answer)
    }

    /// Move an armed line order and/or change its stop and target. On
    /// Hyperliquid a trigger and the children waiting on it can't be moved
    /// in place, so it is cancelled and armed again with the change — from
    /// where the mark is now, so the trigger's direction follows. If the
    /// cancel fails (it fired, or is gone) nothing is armed again.
    pub async fn update_line(&self, network: Network, id: &str, price: Option<f64>, change: &LevelsChange) -> Result<(), Refusal> {
        let oid = oid_of(id)?;
        let (account, wallet) = self.signer(network).await?;
        let o = self.find_order(network, &account, oid).await?;
        if !o.is_trigger || o.reduce_only {
            return Err(Refusal::bad("That order isn't a line order"));
        }
        // Arming it again opens: not on a close-only perp (cancel it instead).
        if let Some(m) = Market::of_coin(&o.coin).filter(|m| !m.tradable()) {
            return Err(close_only(m));
        }
        let a = self.net_asset(network, &o.coin).await?;
        let side = if o.is_buy { Side::Long } else { Side::Short };
        let line = match price {
            Some(p) if p.is_finite() && p > 0.0 => a.price(p),
            Some(_) => return Err(Refusal::bad("The line's price must be a positive number")),
            None => o.trigger_px.unwrap_or(o.limit_px),
        };
        // Its stop and target as they are, then as changed.
        let mut stop = o.children.iter().find(|c| c.is_stop()).and_then(|c| c.trigger_px);
        let mut target = o.children.iter().find(|c| !c.is_stop()).and_then(|c| c.trigger_px);
        if let Some(s) = change.stop {
            stop = s.map(|p| a.price(p));
        }
        if change.remove_target.is_some() {
            target = None;
        }
        if let Some((_, p, _)) = &change.set_target {
            target = Some(a.price(*p));
        }
        let targets: Vec<(f64, Option<f64>)> = target.map(|t| (t, None)).into_iter().collect();
        check_sides(side, line, stop, &targets)?;
        let mid = self.net_mid(network, &o.coin).await?;
        let tpsl = entry_tpsl(o.is_buy, line, mid)?;
        // Cancel first: never two entries armed at once.
        if let Some(OrderStatus::Error(why)) = self.send(network, &wallet, &CancelAction::new(vec![CancelWire { a: a.asset, o: oid }])).await?.first() {
            return Err(Refusal::conflict(format!("Line order not changed — it couldn't be cancelled ({why}); it may have fired")));
        }
        let mut orders = vec![trigger_wire(&a, o.is_buy, o.sz, line, tpsl, false)?];
        orders.extend(levels_wire(&a, o.is_buy, o.sz, stop, &targets)?);
        let grouping = if orders.len() > 1 { Grouping::NormalTpsl } else { Grouping::Na };
        let rearmed = self.send(network, &wallet, &OrderAction::new(orders, grouping)).await;
        match rearmed.as_ref().map(|st| st.first().cloned()) {
            Ok(Some(OrderStatus::Error(why))) => {
                tracing::error!(network = net_str(network), coin = o.coin, "Hyperliquid LIVE line order cancelled but not armed again: {why}");
                Err(Refusal::conflict(format!("The line order was cancelled but not armed again: {why} — arm it again from the ticket")))
            }
            Err(r) => {
                tracing::error!(network = net_str(network), coin = o.coin, "Hyperliquid LIVE line order cancelled but not armed again: {}", r.message);
                Err(Refusal::conflict(format!("The line order was cancelled but not armed again: {} — check the account, then arm it again", r.message)))
            }
            Ok(_) => Ok(()),
        }
    }

    /// Change the LIVE position's stop and targets on Hyperliquid. A stop set
    /// (or moved) is one stop over the whole position as it stands, placed
    /// before every older stop is cancelled; a target is a reduce-only
    /// market trigger, a moved one placed anew before the old goes; a
    /// cleared or removed one is cancelled. Both must be on the right side
    /// of the mid: one past it would fire at once.
    pub async fn set_levels(&self, network: Network, m: &'static Market, change: &LevelsChange) -> Result<Vec<String>, Refusal> {
        let (account, wallet) = self.signer(network).await?;
        let a = self.net_asset(network, m.coin).await?;
        let (szi, _) = self.held(network, &account, m.coin).await?.ok_or_else(|| Refusal::not_found(format!("No LIVE {} position", m.label)))?;
        let long = szi > 0.0;
        let held = szi.abs();
        // The position's own stops and targets: reduce-only triggers on the coin.
        let dex = hyperliquid_client::dex_of(m.coin);
        let open = timed("the open orders", self.info[&network].open_orders(&account, dex)).await.map_err(Refusal::conflict)?;
        let guards: Vec<&HlOpenOrder> = open.iter().filter(|o| o.coin == m.coin && o.is_trigger && o.reduce_only).collect();
        let stops: Vec<HlOpenOrder> = guards.iter().filter(|o| o.is_stop()).map(|o| (*o).clone()).collect();
        let side = if long { Side::Long } else { Side::Short };
        let mut warnings = Vec::new();
        let mid = if change.stop.flatten().is_some() || change.set_target.is_some() { Some(self.net_mid(network, m.coin).await?) } else { None };
        if let Some(stop) = change.stop {
            match stop {
                Some(p) => {
                    let p = a.price(p);
                    if let Some(why) = crate::hyperliquid::perp::wrong_side("stop", side, mid.unwrap_or(p), p) {
                        return Err(Refusal::bad(format!("{why} (the price now)")));
                    }
                    warnings.extend(self.set_full_stop(network, &wallet, &a, long, held, p, &stops).await?);
                }
                None => {
                    for o in &stops {
                        warnings.extend(self.cancel(network, &o.oid.to_string()).await?);
                    }
                }
            }
        }
        if let Some(id) = &change.remove_target {
            warnings.extend(self.cancel(network, id).await?);
        }
        if let Some((id, price, size)) = &change.set_target {
            let price = a.price(*price);
            if let Some(why) = crate::hyperliquid::perp::wrong_side("target", side, mid.unwrap_or(price), price) {
                return Err(Refusal::bad(format!("{why} (the price now)")));
            }
            match id {
                Some(id) => {
                    let oid = oid_of(id)?;
                    let o = guards.iter().find(|o| o.oid == oid).ok_or_else(|| Refusal::not_found("No such target on Hyperliquid"))?;
                    let size = match size {
                        None => o.sz,
                        Some(None) => held,
                        Some(Some(s)) => a.size(*s).min(held),
                    };
                    partial_target_ok(side, size, held, price)?;
                    self.replace_guard(network, &wallet, &a, o, price, size).await?;
                }
                None => {
                    let size = size.flatten().map_or(held, |s| a.size(s).min(held));
                    if size <= 0.0 {
                        return Err(Refusal::bad("A target's size must be more than nothing"));
                    }
                    partial_target_ok(side, size, held, price)?;
                    self.place_guard(network, &wallet, &a, !long, size, price, "tp").await?;
                }
            }
        }
        Ok(warnings)
    }

    /// One reduce-only market trigger (a stop or a target) resting on Hyperliquid.
    #[allow(clippy::too_many_arguments)]
    async fn place_guard(&self, network: Network, wallet: &Wallet, a: &NetAsset, buy: bool, size: f64, trigger: f64, tpsl: &'static str) -> Result<(), Refusal> {
        let order = trigger_wire(a, buy, size, trigger, tpsl, true)?;
        match self.send(network, wallet, &OrderAction::new(vec![order], Grouping::Na)).await?.first() {
            Some(OrderStatus::Error(why)) => Err(Refusal::conflict(format!("{} not placed: {why}", if tpsl == "sl" { "Stop" } else { "Target" }))),
            _ => Ok(()),
        }
    }

    /// The kill switch, on Hyperliquid: cancel every open order and close
    /// every position on every dex read, the app's perps or not. What it
    /// couldn't do, in words; empty when the account is flat.
    pub async fn flatten(&self, network: Network) -> Vec<String> {
        // LIVE routes hold the gate while they answer: an order that passed
        // the kill-switch check just before it was set lands before this
        // reads the account. Later ones see the switch on.
        drop(self.gate.write().await);
        let mut problems = Vec::new();
        let (account, wallet) = match self.signer(network).await {
            Ok(s) => s,
            Err(r) => return vec![r.message],
        };
        let dexes = match self.dexes_on(network).await {
            Ok(d) => d,
            Err(e) => return vec![e],
        };
        // Orders that can open first, so no line or limit fires into the
        // closes; the positions; then the rest (the stops stay until their
        // position is closed — one firing into a close is harmless).
        problems.extend(self.cancel_where(network, &account, &wallet, &dexes, |o| !o.reduce_only).await);
        for &dex in &dexes {
            let state = match timed("the positions", self.info[&network].account_state(&account, dex)).await {
                Ok(s) => s,
                Err(e) => {
                    problems.push(e);
                    continue;
                }
            };
            for p in state.positions.iter().filter(|p| p.szi != 0.0) {
                let label = Market::of_coin(&p.coin).map_or(p.coin.as_str(), |m| m.label);
                let key = Market::of_coin(&p.coin).map_or(p.coin.as_str(), |m| m.key);
                if let Err(r) = self.close_coin(network, &account, &wallet, &p.coin, key, label, None).await {
                    problems.push(r.message);
                }
            }
        }
        problems.extend(self.cancel_where(network, &account, &wallet, &dexes, |_| true).await);
        problems
    }

    /// Cancel the account's open orders on `dexes` that `which` picks; what
    /// couldn't be, in words.
    async fn cancel_where(&self, network: Network, account: &str, wallet: &Wallet, dexes: &[Option<&'static str>], which: impl Fn(&HlOpenOrder) -> bool) -> Vec<String> {
        let mut problems = Vec::new();
        for &dex in dexes {
            let orders = match timed("the open orders", self.info[&network].open_orders(account, dex)).await {
                Ok(o) => o,
                Err(e) => {
                    problems.push(e);
                    continue;
                }
            };
            let mut cancels = Vec::new();
            for o in orders.iter().filter(|o| which(o)) {
                match self.net_asset(network, &o.coin).await {
                    Ok(a) => cancels.push(CancelWire { a: a.asset, o: o.oid }),
                    Err(r) => problems.push(format!("{} order {} not cancelled: {}", o.coin, o.oid, r.message)),
                }
            }
            if cancels.is_empty() {
                continue;
            }
            match self.send(network, wallet, &CancelAction::new(cancels)).await {
                Ok(statuses) => problems.extend(statuses.iter().filter_map(|st| match st {
                    OrderStatus::Error(why) => Some(format!("an order not cancelled: {why}")),
                    _ => None,
                })),
                Err(r) => problems.push(format!("orders not cancelled: {}", r.message)),
            }
        }
        problems
    }

    /// A perp just removed from the list (close-only): the owner's LIVE
    /// orders on it that can open — limits and line orders — are cancelled,
    /// as paper's are. Its stops and targets stay with the position.
    pub async fn cancel_opening(&self, network: Network, coins: &[String]) -> Vec<String> {
        let (account, wallet) = match self.signer(network).await {
            Ok(s) => s,
            Err(r) => return vec![r.message],
        };
        let dexes = match self.dexes_on(network).await {
            Ok(d) => d,
            Err(e) => return vec![e],
        };
        self.cancel_where(network, &account, &wallet, &dexes, |o| !o.reduce_only && coins.iter().any(|c| c == &o.coin)).await
    }
}

/// The refusal for an order that would open or add on a close-only perp.
fn close_only(m: &Market) -> Refusal {
    Refusal::conflict(format!("{} is close-only: it was removed from the app's markets, so only orders that reduce a position are taken", m.label))
}

/// Targets: each a price and a size (`None`: all that is left).
type Targets = Vec<(f64, Option<f64>)>;

/// A target for `size` of a `side` position of `held` at `price`: one for
/// part of it must be worth Hyperliquid's minimum where it may fill, or
/// Hyperliquid refuses it (all of it is exempt).
fn partial_target_ok(side: Side, size: f64, held: f64, price: f64) -> Result<(), Refusal> {
    if size + 1e-12 >= held {
        return Ok(());
    }
    match crate::hyperliquid::perp::partial_target_refusal(side, size, price) {
        Some(why) => Err(Refusal::bad(why)),
        None => Ok(()),
    }
}

/// The stop and target an entry carries on Hyperliquid: children are as
/// big as the entry, so at most one stop and one target, for all of it.
/// More targets, or a sized one, go on once the position is open.
fn entry_levels(stop: Option<f64>, targets: &[(f64, Option<f64>)]) -> Result<(Option<f64>, Targets), Refusal> {
    if targets.len() > 1 || targets.iter().any(|(_, s)| s.is_some()) {
        return Err(Refusal::bad("A LIVE order takes one target, for all of it: add more targets once the position is open"));
    }
    Ok((stop, targets.iter().map(|(p, _)| (*p, None)).collect()))
}

/// The price a position's levels are measured against: a limit's own, else
/// the mid a market order fills near.
fn price_ref(limit: Option<f64>, mid: f64) -> f64 {
    limit.unwrap_or(mid)
}

/// A stop and targets on the right side of `entry` for a `side` position:
/// one past it would fire as soon as the position opens.
fn check_sides(side: Side, entry: f64, stop: Option<f64>, targets: &[(f64, Option<f64>)]) -> Result<(), Refusal> {
    use crate::hyperliquid::perp::wrong_side;
    if let Some(why) = stop.and_then(|p| wrong_side("stop", side, entry, p)) {
        return Err(Refusal::bad(why));
    }
    for (p, _) in targets {
        if let Some(why) = wrong_side("target", side, entry, *p) {
            return Err(Refusal::bad(why));
        }
    }
    Ok(())
}

/// How close to the mid a line may be and still say which way the price
/// must cross it: 0.05 %.
const LINE_GAP: f64 = 0.0005;

/// A line order's trigger kind on Hyperliquid, which sets the way it fires:
/// a buy marked `sl` fires once the mark rises to it and one marked `tp`
/// once it falls to it, a sell the other way round. So a line the price
/// must rise to buys as `sl` and sells as `tp`, and one it must fall to the
/// opposite. A line on (or within 0.05 % of) the mid is refused: which way
/// it crosses isn't known.
pub fn entry_tpsl(buy: bool, line: f64, mid: f64) -> Result<&'static str, Refusal> {
    if (line - mid).abs() <= mid * LINE_GAP {
        return Err(Refusal::conflict(format!("The line at {line} is on the price (the mid is {mid}) — place it above or below")));
    }
    let rising = line > mid;
    Ok(if rising == buy { "sl" } else { "tp" })
}

/// A position's stop and targets as orders sent with its entry
/// (`normalTpsl`): reduce-only market triggers on the other side.
fn levels_wire(a: &NetAsset, buy: bool, size: f64, stop: Option<f64>, targets: &[(f64, Option<f64>)]) -> Result<Vec<OrderWire>, Refusal> {
    let mut out = Vec::new();
    if let Some(sl) = stop {
        out.push(trigger_wire(a, !buy, size, a.price(sl), "sl", true)?);
    }
    for (tp, tsize) in targets {
        out.push(trigger_wire(a, !buy, a.size(tsize.unwrap_or(size)), a.price(*tp), "tp", true)?);
    }
    Ok(out)
}

/// A market trigger: once the mark reaches `trigger`, a market order of
/// `size`. Its limit price is the trigger ± 10 %, the worst it may fill at.
pub fn trigger_wire(a: &NetAsset, buy: bool, size: f64, trigger: f64, tpsl: &'static str, reduce_only: bool) -> Result<OrderWire, Refusal> {
    let worst = a.price(if buy { trigger * (1.0 + TRIGGER_SLIP) } else { trigger * (1.0 - TRIGGER_SLIP) });
    Ok(OrderWire {
        a: a.asset,
        b: buy,
        p: wire(worst, "price")?,
        s: wire(size, "size")?,
        r: reduce_only,
        t: OrderTypeWire::trigger(true, wire(trigger, "trigger price")?, tpsl),
        c: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn btc() -> NetAsset {
        NetAsset { asset: 0, sz_decimals: 5, max_leverage: 40 }
    }

    #[test]
    fn a_market_order_crosses_the_book_by_half_a_percent_on_the_grid() {
        assert_eq!(ioc_price(&btc(), true, 62_000.0), 62_310.0);
        assert_eq!(ioc_price(&btc(), false, 62_000.0), 61_690.0);
        let nvda = NetAsset { asset: 110_005, sz_decimals: 3, max_leverage: 10 };
        assert_eq!(ioc_price(&nvda, true, 145.47), 146.2, "five figures, at most 3 decimals");
        assert_eq!(nvda.leverage_cap(), 10);
        assert_eq!(btc().leverage_cap(), 20);
    }

    #[test]
    fn a_line_fires_the_way_the_price_must_cross_it() {
        // A buy the price must rise to: Hyperliquid's buy "sl" fires on the way up.
        assert_eq!(entry_tpsl(true, 63_000.0, 62_000.0).unwrap(), "sl");
        // A buy the price must fall to: a buy "tp" fires on the way down.
        assert_eq!(entry_tpsl(true, 61_000.0, 62_000.0).unwrap(), "tp");
        // Sells the other way round.
        assert_eq!(entry_tpsl(false, 61_000.0, 62_000.0).unwrap(), "sl");
        assert_eq!(entry_tpsl(false, 63_000.0, 62_000.0).unwrap(), "tp");
        assert!(entry_tpsl(true, 62_020.0, 62_000.0).is_err(), "within 0.05 % of the mid");
    }

    #[test]
    fn an_entry_carries_one_stop_and_one_target_for_all_of_it() {
        let (stop, targets) = entry_levels(Some(60_000.0), &[(65_000.0, None)]).unwrap();
        assert_eq!((stop, targets), (Some(60_000.0), vec![(65_000.0, None)]));
        assert!(entry_levels(None, &[(65_000.0, None), (66_000.0, None)]).is_err(), "two targets");
        assert!(entry_levels(None, &[(65_000.0, Some(0.01))]).is_err(), "a sized target");
        assert_eq!(entry_levels(None, &[]).unwrap(), (None, vec![]));
    }

    #[test]
    fn levels_on_the_wrong_side_are_refused_before_anything_is_sent() {
        assert!(check_sides(Side::Long, 62_000.0, Some(63_000.0), &[]).is_err(), "a long's stop above its entry");
        assert!(check_sides(Side::Short, 62_000.0, None, &[(63_000.0, None)]).is_err(), "a short's target above its entry");
        assert!(check_sides(Side::Long, 62_000.0, Some(61_000.0), &[(64_000.0, Some(0.01))]).is_ok());
    }

    #[test]
    fn a_stop_and_targets_go_with_the_entry_as_reduce_only_market_triggers() {
        let w = levels_wire(&btc(), true, 0.02, Some(60_000.0), &[(65_000.0, Some(0.01)), (66_000.0, None)]).unwrap();
        assert_eq!(w.len(), 3);
        assert!(w.iter().all(|o| !o.b && o.r), "a long's levels sell, reduce-only");
        let sl = w[0].t.trigger.as_ref().unwrap();
        assert_eq!((sl.tpsl, sl.trigger_px.as_str(), sl.is_market), ("sl", "60000", true));
        assert_eq!(w[0].p, "54000", "a market trigger's worst price: 10 % past it");
        assert_eq!((w[1].s.as_str(), w[2].s.as_str()), ("0.01", "0.02"), "a sized target, then all of it");
    }
}
