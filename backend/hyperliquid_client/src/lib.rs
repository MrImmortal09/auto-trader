//! Hyperliquid client: market data for the perps the app trades.
//!
//! [`InfoClient`] calls the public `info` endpoint (candles, prices, an
//! account's positions and orders); [`feed`] keeps one WebSocket open for
//! live candles and each contract's context (mark, mid, oracle price,
//! funding); [`signing`] signs exchange actions with an API wallet, which can
//! trade but never withdraw.
//!
//! Hyperliquid names a core perp by its coin (`BTC`) and a builder-deployed
//! (HIP-3) perp by its dex and coin (`xyz:GOLD`); every call here takes that
//! name as Hyperliquid spells it. Prices and sizes arrive as decimal strings
//! and are read as `f64` (market data only — never an amount sent back).

pub mod account;
pub mod exchange;
pub mod feed;
pub mod signing;

use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

/// Most candles one `candleSnapshot` answers; a longer range is paged.
pub const MAX_CANDLES_PER_CALL: usize = 5_000;
const API_TIMEOUT: Duration = Duration::from_secs(15);

/// Which Hyperliquid: the real one, or the testnet (free test USDC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Network {
    Mainnet,
    Testnet,
}

impl Network {
    pub fn api_url(self) -> &'static str {
        match self {
            Network::Mainnet => "https://api.hyperliquid.xyz",
            Network::Testnet => "https://api.hyperliquid-testnet.xyz",
        }
    }

    pub fn ws_url(self) -> &'static str {
        match self {
            Network::Mainnet => "wss://api.hyperliquid.xyz/ws",
            Network::Testnet => "wss://api.hyperliquid-testnet.xyz/ws",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Network::Mainnet => "mainnet",
            Network::Testnet => "testnet",
        }
    }

    pub fn parse(s: &str) -> Option<Network> {
        match s {
            "mainnet" => Some(Network::Mainnet),
            "testnet" => Some(Network::Testnet),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HlError {
    #[error("Hyperliquid unreachable: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Hyperliquid answered {status}: {body}")]
    Status { status: u16, body: String },
    #[error("unreadable Hyperliquid answer: {0}")]
    Decode(String),
    #[error("API wallet key: {0}")]
    Key(String),
    #[error("could not encode the action: {0}")]
    Encode(String),
}

/// One OHLCV candle of `coin` at `interval`, opening at `t` (Unix ms).
#[derive(Debug, Clone, PartialEq)]
pub struct Candle {
    pub coin: String,
    /// `1m`, `5m`, `15m`, …
    pub interval: String,
    pub t: i64,
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
    /// Volume in the coin (BTC, not USD).
    pub v: f64,
}

/// A candle as Hyperliquid sends it, REST and WebSocket alike.
#[derive(Debug, Deserialize)]
struct RawCandle {
    t: i64,
    s: String,
    i: String,
    o: String,
    h: String,
    l: String,
    c: String,
    v: String,
}

pub(crate) fn num(s: &str) -> Result<f64, HlError> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite()).ok_or_else(|| HlError::Decode(format!("not a number: {s:?}")))
}

impl TryFrom<RawCandle> for Candle {
    type Error = HlError;
    fn try_from(r: RawCandle) -> Result<Self, HlError> {
        Ok(Candle { coin: r.s, interval: r.i, t: r.t, o: num(&r.o)?, h: num(&r.h)?, l: num(&r.l)?, c: num(&r.c)?, v: num(&r.v)? })
    }
}

/// A perp's live context: what it trades at, and what it costs to hold.
#[derive(Debug, Clone, PartialEq)]
pub struct AssetCtx {
    pub coin: String,
    /// The mark price: what P&L, margin and liquidation are measured at.
    pub mark: f64,
    /// The order book's mid, when it has both sides.
    pub mid: Option<f64>,
    /// The oracle (index) price funding is measured against.
    pub oracle: f64,
    /// The current hourly funding rate (0.0001 = 0.01 % an hour); a long
    /// pays it to shorts when positive.
    pub funding: f64,
    /// Open interest, in the coin.
    pub open_interest: f64,
    /// The price 24 hours ago.
    pub prev_day: f64,
    /// Volume over the last 24 hours, in USD.
    pub day_volume: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCtx {
    mark_px: String,
    mid_px: Option<String>,
    oracle_px: String,
    funding: String,
    open_interest: String,
    prev_day_px: String,
    day_ntl_vlm: String,
}

impl RawCtx {
    fn read(self, coin: String) -> Result<AssetCtx, HlError> {
        Ok(AssetCtx {
            coin,
            mark: num(&self.mark_px)?,
            mid: self.mid_px.as_deref().map(num).transpose()?,
            oracle: num(&self.oracle_px)?,
            funding: num(&self.funding)?,
            open_interest: num(&self.open_interest)?,
            prev_day: num(&self.prev_day_px)?,
            day_volume: num(&self.day_ntl_vlm)?,
        })
    }
}

/// A perp's trading rules, from the dex's `meta`.
#[derive(Debug, Clone, PartialEq)]
pub struct PerpRules {
    pub coin: String,
    /// Its index in the dex's universe; the asset id orders name (see
    /// [`asset_id`]).
    pub index: usize,
    /// Decimals a size may carry: 5 for BTC means 0.00001 BTC.
    pub sz_decimals: u32,
    pub max_leverage: u32,
    /// Only isolated margin is allowed on it.
    pub only_isolated: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAsset {
    name: String,
    sz_decimals: u32,
    max_leverage: u32,
    #[serde(default)]
    only_isolated: Option<bool>,
    #[serde(default)]
    is_delisted: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct RawMeta {
    universe: Vec<RawAsset>,
}

/// The id an order names a perp by: its index on a core perp; for a
/// builder-deployed one, `100000 + dex_index * 10000 + index` (Hyperliquid's
/// "Asset IDs"). `dex_index` is the dex's place in `perpDexs` (0 = core).
pub fn asset_id(dex_index: usize, index: usize) -> u32 {
    if dex_index == 0 {
        index as u32
    } else {
        (100_000 + dex_index * 10_000 + index) as u32
    }
}

/// The dex a coin name belongs to: `xyz` for `xyz:GOLD`, `None` (core) for `BTC`.
pub fn dex_of(coin: &str) -> Option<&str> {
    coin.split_once(':').map(|(dex, _)| dex)
}

/// The public `info` endpoint.
#[derive(Clone)]
pub struct InfoClient {
    http: reqwest::Client,
    network: Network,
}

impl InfoClient {
    pub fn new(network: Network) -> Self {
        let http = reqwest::Client::builder().timeout(API_TIMEOUT).build().unwrap_or_default();
        InfoClient { http, network }
    }

    pub fn network(&self) -> Network {
        self.network
    }

    pub(crate) async fn post(&self, body: Value) -> Result<Value, HlError> {
        let resp = self.http.post(format!("{}/info", self.network.api_url())).json(&body).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(HlError::Status { status: status.as_u16(), body: body.chars().take(300).collect() });
        }
        resp.json::<Value>().await.map_err(|e| HlError::Decode(e.to_string()))
    }

    /// `coin`'s candles at `interval` opening in `[start_ms, end_ms]`,
    /// oldest first. Hyperliquid answers at most [`MAX_CANDLES_PER_CALL`] a
    /// call, so a longer range is read in pages.
    pub async fn candles(&self, coin: &str, interval: &str, start_ms: i64, end_ms: i64) -> Result<Vec<Candle>, HlError> {
        let mut out: Vec<Candle> = Vec::new();
        let mut from = start_ms;
        // A bound on the pages, whatever Hyperliquid answers.
        for _ in 0..20 {
            if from > end_ms {
                break;
            }
            let body = json!({ "type": "candleSnapshot", "req": { "coin": coin, "interval": interval, "startTime": from, "endTime": end_ms } });
            let raw: Vec<RawCandle> = serde_json::from_value(self.post(body).await?).map_err(|e| HlError::Decode(e.to_string()))?;
            let page: Vec<Candle> = raw.into_iter().map(Candle::try_from).collect::<Result<_, _>>()?;
            let full = page.len() >= MAX_CANDLES_PER_CALL;
            let last = page.last().map(|c| c.t);
            for c in page {
                if out.last().is_none_or(|prev| c.t > prev.t) {
                    out.push(c);
                }
            }
            match last {
                Some(t) if full => from = t + 1,
                _ => break,
            }
        }
        Ok(out)
    }

    /// The trading rules of every perp on `dex` (`None`: the core perps),
    /// delisted ones left out.
    pub async fn perp_rules(&self, dex: Option<&str>) -> Result<Vec<PerpRules>, HlError> {
        let raw = self.post(json!({ "type": "meta", "dex": dex.unwrap_or("") })).await?;
        let meta: RawMeta = serde_json::from_value(raw).map_err(|e| HlError::Decode(e.to_string()))?;
        Ok(meta
            .universe
            .into_iter()
            .enumerate()
            .filter(|(_, a)| a.is_delisted != Some(true))
            .map(|(index, a)| PerpRules { coin: a.name, index, sz_decimals: a.sz_decimals, max_leverage: a.max_leverage, only_isolated: a.only_isolated.unwrap_or(false) })
            .collect())
    }

    /// The dexes' names in `perpDexs` order (index 0, the core perps, is
    /// `None`): what [`asset_id`] counts a builder-deployed perp's dex by.
    pub async fn perp_dexes(&self) -> Result<Vec<Option<String>>, HlError> {
        let raw = self.post(json!({ "type": "perpDexs" })).await?;
        let list = raw.as_array().ok_or_else(|| HlError::Decode("perpDexs is not a list".into()))?;
        Ok(list.iter().map(|d| d.get("name").and_then(Value::as_str).map(str::to_string)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_candle_reads_from_hyperliquids_strings() {
        let raw: RawCandle = serde_json::from_str(
            r#"{"t":1795080900000,"T":1795081199999,"s":"xyz:GOLD","i":"5m","o":"3735.1","c":"3736.7","h":"3736.8","l":"3733.1","v":"4.2067","n":30}"#,
        )
        .unwrap();
        let c = Candle::try_from(raw).unwrap();
        assert_eq!((c.coin.as_str(), c.interval.as_str(), c.t), ("xyz:GOLD", "5m", 1_795_080_900_000));
        assert_eq!((c.o, c.h, c.l, c.c, c.v), (3735.1, 3736.8, 3733.1, 3736.7, 4.2067));
        let bad: RawCandle = serde_json::from_str(r#"{"t":1,"s":"BTC","i":"1m","o":"x","c":"1","h":"1","l":"1","v":"1"}"#).unwrap();
        assert!(Candle::try_from(bad).is_err());
    }

    #[test]
    fn a_context_reads_mark_mid_and_funding() {
        let raw: RawCtx = serde_json::from_str(
            r#"{"funding":"-0.0000005529","openInterest":"37613.36768","prevDayPx":"64637.0","dayNtlVlm":"4664630479.8","premium":"-0.0003","oraclePx":"63829.0","markPx":"63791.0","midPx":"63806.5","impactPxs":["63806.0","63809.7"]}"#,
        )
        .unwrap();
        let c = raw.read("BTC".into()).unwrap();
        assert_eq!((c.mark, c.mid, c.oracle, c.prev_day), (63791.0, Some(63806.5), 63829.0, 64637.0));
        assert!((c.funding + 0.000_000_552_9).abs() < 1e-15);
    }

    #[test]
    fn a_builder_perp_is_named_by_its_dex() {
        assert_eq!(asset_id(0, 0), 0);
        assert_eq!(asset_id(0, 1), 1);
        // testnet's `test:ABC` in Hyperliquid's docs: dex 1, index 0.
        assert_eq!(asset_id(1, 0), 110_000);
        assert_eq!(asset_id(1, 3), 110_003);
        assert_eq!(dex_of("xyz:GOLD"), Some("xyz"));
        assert_eq!(dex_of("BTC"), None);
    }
}
