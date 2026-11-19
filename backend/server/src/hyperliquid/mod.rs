//! Hyperliquid perps: the three the app trades, and their market data.
//!
//! Only BTC, ETH and trade.xyz's gold (`xyz:GOLD`, a builder-deployed perp)
//! are charted or priced — no other Hyperliquid market. Their charts are
//! `HL:BTC`, `HL:ETH` and `HL:GOLD` on the same candle routes as everything
//! else. Past bars come from Hyperliquid's `candleSnapshot`; the live ones,
//! and each perp's mark, mid, oracle price and funding, from one WebSocket
//! open only while the `hyperliquid` switch is on.
//!
//! Market data comes from mainnet whatever the trading network: paper fills
//! are priced on the real market. Nothing here holds a key or can trade.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};

use hyperliquid_client::feed::{self, FeedMsg, Sub};
use hyperliquid_client::{AssetCtx, HlError, InfoClient, Network};
use serde::Serialize;
use tokio::sync::{broadcast, mpsc, watch, Mutex};
use tokio::task::JoinHandle;

use crate::candles::bars::{Bar, Tf};

/// The candle intervals followed live: the charts' timeframes.
const TFS: [Tf; 3] = [Tf::M1, Tf::M5, Tf::M15];
/// Room for a burst of bar changes while a stream catches up.
const CHANGES_CAPACITY: usize = 1024;

/// A perp the app trades.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct Market {
    /// The app's name for it: `BTC`, `ETH`, `GOLD`.
    pub key: &'static str,
    /// Hyperliquid's name: `BTC`, `ETH`, `xyz:GOLD`.
    pub coin: &'static str,
    /// What the screens call it: `BTC-PERP`.
    pub label: &'static str,
    pub name: &'static str,
}

/// Every perp the app charts or trades, in the header's order.
pub const MARKETS: [Market; 3] = [
    Market { key: "BTC", coin: "BTC", label: "BTC-PERP", name: "Bitcoin" },
    Market { key: "ETH", coin: "ETH", label: "ETH-PERP", name: "Ether" },
    Market { key: "GOLD", coin: "xyz:GOLD", label: "GOLD-PERP", name: "Gold (trade.xyz)" },
];

/// What a perp's chart symbol starts with.
pub const SYMBOL_PREFIX: &str = "HL:";

impl Market {
    /// Its chart symbol: `HL:BTC`.
    pub fn symbol(&self) -> String {
        format!("{SYMBOL_PREFIX}{}", self.key)
    }

    /// The market `HL:BTC` (any case) names.
    pub fn of_symbol(raw: &str) -> Option<&'static Market> {
        let raw = raw.trim();
        let rest = raw.get(..SYMBOL_PREFIX.len()).filter(|p| p.eq_ignore_ascii_case(SYMBOL_PREFIX)).map(|_| &raw[SYMBOL_PREFIX.len()..])?;
        MARKETS.iter().find(|m| m.key.eq_ignore_ascii_case(rest))
    }

    /// The market Hyperliquid calls `coin`.
    pub fn of_coin(coin: &str) -> Option<&'static Market> {
        MARKETS.iter().find(|m| m.coin == coin)
    }
}

/// Whether `raw` is a perp's chart symbol (or meant to be one: `HL:` and
/// anything after it), so the candle routes hand it here.
pub fn is_hl_symbol(raw: &str) -> bool {
    raw.trim().get(..SYMBOL_PREFIX.len()).is_some_and(|p| p.eq_ignore_ascii_case(SYMBOL_PREFIX))
}

/// A perp's bar that changed, for the candle stream.
#[derive(Debug, Clone)]
pub struct BarChange {
    pub key: &'static str,
    pub tf: Tf,
    pub bar: Bar,
}

/// A perp's live context with when it arrived (Unix ms).
#[derive(Debug, Clone)]
pub struct Quote {
    pub ctx: AssetCtx,
    pub at_ms: i64,
}

#[derive(Default)]
struct Live {
    /// The feed is connected and subscribed.
    up: bool,
    quotes: HashMap<&'static str, Quote>,
    /// Each perp's latest bar at each timeframe.
    last: HashMap<(&'static str, Tf), Bar>,
}

struct Running {
    stop: watch::Sender<bool>,
    feed: JoinHandle<()>,
    reader: JoinHandle<()>,
}

/// Hyperliquid's market data while the `hyperliquid` switch is on.
pub struct HlMarketData {
    info: InfoClient,
    live: StdMutex<Live>,
    changes: broadcast::Sender<BarChange>,
    running: Mutex<Option<Running>>,
}

fn now_ms() -> i64 {
    shared_domain::now_ist().timestamp_millis()
}

fn bar_of(c: &hyperliquid_client::Candle) -> Bar {
    Bar { t: c.t, o: c.o, h: c.h, l: c.l, c: c.c, v: Some(c.v) }
}

impl HlMarketData {
    pub fn new() -> Arc<Self> {
        Arc::new(HlMarketData {
            info: InfoClient::new(Network::Mainnet),
            live: StdMutex::new(Live::default()),
            changes: broadcast::channel(CHANGES_CAPACITY).0,
            running: Mutex::new(None),
        })
    }

    fn live(&self) -> MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Open the feed (`on`) or close it, as the switch now says.
    pub async fn switch(self: &Arc<Self>, on: bool) {
        let mut running = self.running.lock().await;
        if on == running.is_some() {
            return;
        }
        if let Some(r) = running.take() {
            let _ = r.stop.send(true);
            r.feed.abort();
            r.reader.abort();
            *self.live() = Live::default();
            tracing::info!("Hyperliquid feed closed");
            return;
        }
        let subs: Vec<Sub> = MARKETS
            .iter()
            .flat_map(|m| {
                TFS.iter()
                    .map(|tf| Sub::Candle { coin: m.coin.to_string(), interval: tf.as_str().to_string() })
                    .chain([Sub::Ctx { coin: m.coin.to_string() }])
            })
            .collect();
        let (tx, rx) = mpsc::channel(4096);
        let (stop, stop_rx) = watch::channel(false);
        let feed = tokio::spawn(feed::run(Network::Mainnet, subs, tx, stop_rx));
        let reader = tokio::spawn(Arc::clone(self).read(rx));
        *running = Some(Running { stop, feed, reader });
        tracing::info!("Hyperliquid feed opening (BTC, ETH, xyz:GOLD)");
    }

    /// Keep the feed's updates, until it ends.
    async fn read(self: Arc<Self>, mut rx: mpsc::Receiver<FeedMsg>) {
        while let Some(msg) = rx.recv().await {
            match msg {
                FeedMsg::Up => {
                    self.live().up = true;
                    tracing::info!("Hyperliquid feed up");
                }
                FeedMsg::Down(_) => self.live().up = false,
                FeedMsg::Ctx(ctx) => {
                    if let Some(m) = Market::of_coin(&ctx.coin) {
                        self.live().quotes.insert(m.key, Quote { ctx, at_ms: now_ms() });
                    }
                }
                FeedMsg::Candle(c) => {
                    let (Some(m), Some(tf)) = (Market::of_coin(&c.coin), Tf::parse(&c.interval)) else { continue };
                    let bar = bar_of(&c);
                    self.live().last.insert((m.key, tf), bar.clone());
                    let _ = self.changes.send(BarChange { key: m.key, tf, bar });
                }
            }
        }
    }

    /// Whether the feed is connected now.
    pub fn is_up(&self) -> bool {
        self.live().up
    }

    /// `key`'s latest context, if one has arrived since the feed opened.
    pub fn quote(&self, key: &str) -> Option<Quote> {
        self.live().quotes.get(key).cloned()
    }

    /// `key`'s latest bar at `tf`.
    pub fn last_bar(&self, key: &str, tf: Tf) -> Option<Bar> {
        self.live().last.iter().find(|((k, t), _)| *k == key && *t == tf).map(|(_, b)| b.clone())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BarChange> {
        self.changes.subscribe()
    }

    /// `m`'s bars at `tf` opening in `[from_ms, to_ms]`, oldest first —
    /// Hyperliquid's own candles, with the feed's newer bar for the last one
    /// when it has one.
    pub async fn history(&self, m: &Market, tf: Tf, from_ms: i64, to_ms: i64) -> Result<Vec<Bar>, HlError> {
        let mut bars: Vec<Bar> = self.info.candles(m.coin, tf.as_str(), from_ms, to_ms).await?.iter().map(bar_of).collect();
        if let Some(live) = self.last_bar(m.key, tf).filter(|b| b.t >= from_ms && b.t <= to_ms) {
            match bars.last_mut() {
                Some(last) if last.t == live.t => *last = live,
                Some(last) if last.t > live.t => {}
                _ => bars.push(live),
            }
        }
        Ok(bars)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_three_perps_are_named() {
        assert_eq!(Market::of_symbol("HL:BTC").map(|m| m.coin), Some("BTC"));
        assert_eq!(Market::of_symbol("hl:gold").map(|m| m.coin), Some("xyz:GOLD"));
        assert_eq!(Market::of_symbol(" HL:ETH ").map(|m| m.label), Some("ETH-PERP"));
        assert_eq!(Market::of_symbol("HL:SOL"), None);
        assert_eq!(Market::of_symbol("BTC"), None);
        assert_eq!(Market::of_symbol("NIFTY"), None);
        assert!(is_hl_symbol("HL:SOL"), "an unknown perp is still the perps' to refuse");
        assert!(!is_hl_symbol("NIFTY") && !is_hl_symbol("H"));
        assert_eq!(Market::of_coin("xyz:GOLD").map(|m| m.key), Some("GOLD"));
        assert_eq!(Market::of_coin("GOLD"), None, "only trade.xyz's gold");
        assert_eq!(MARKETS.iter().map(Market::symbol).collect::<Vec<_>>(), ["HL:BTC", "HL:ETH", "HL:GOLD"]);
    }
}
