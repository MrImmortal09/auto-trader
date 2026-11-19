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
    #[error("Hyperliquid refused it: {0}")]
    Refused(String),
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

/// Hyperliquid's base-tier fees on a core perp, as a fraction of notional.
pub const BASE_TAKER_FEE: f64 = 0.00045;
pub const BASE_MAKER_FEE: f64 = 0.00015;

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
    /// A builder-deployed perp's fee share for its deployer (`None` on a
    /// core perp): its fees scale with it.
    pub deployer_fee_scale: Option<f64>,
    /// Growth mode: 90 % off every fee.
    pub growth_mode: bool,
}

impl PerpRules {
    /// Its taker and maker fees at the base tier, as Hyperliquid computes
    /// them: a builder-deployed perp's are the core rate times
    /// `scale + 1` (a scale under 1) or `2 * scale`, and a tenth of that in
    /// growth mode.
    pub fn fees(&self) -> (f64, f64) {
        let scale = match self.deployer_fee_scale {
            None => 1.0,
            Some(s) if s < 1.0 => s + 1.0,
            Some(s) => 2.0 * s,
        };
        let growth = if self.growth_mode { 0.1 } else { 1.0 };
        (BASE_TAKER_FEE * scale * growth, BASE_MAKER_FEE * scale * growth)
    }
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
    #[serde(default)]
    deployer_fee_scale: Option<String>,
    #[serde(default)]
    growth_mode: Option<String>,
}

impl RawAsset {
    fn rules(self, index: usize, builder: bool) -> Result<PerpRules, HlError> {
        let deployer_fee_scale = match (builder, self.deployer_fee_scale.as_deref()) {
            (false, _) => None,
            (true, None) => Some(1.0),
            (true, Some(s)) => Some(num(s)?),
        };
        Ok(PerpRules {
            coin: self.name,
            index,
            sz_decimals: self.sz_decimals,
            max_leverage: self.max_leverage,
            only_isolated: self.only_isolated.unwrap_or(false),
            deployer_fee_scale,
            growth_mode: self.growth_mode.as_deref() == Some("enabled"),
        })
    }
}

#[derive(Debug, Deserialize)]
struct RawMeta {
    universe: Vec<RawAsset>,
}

/// A dex in `perpDexs`: the core perps (`name: None`) or a builder's.
#[derive(Debug, Clone, PartialEq)]
pub struct Dex {
    pub name: Option<String>,
    /// What the builder calls it (`XYZ`, `Paragon`); `None` on the core perps.
    pub full_name: Option<String>,
}

/// One perp as `metaAndAssetCtxs` lists it: its rules and its context now.
#[derive(Debug, Clone, PartialEq)]
pub struct PerpListing {
    pub rules: PerpRules,
    pub ctx: Option<AssetCtx>,
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
        let builder = dex.is_some_and(|d| !d.is_empty());
        meta.universe
            .into_iter()
            .enumerate()
            .filter(|(_, a)| a.is_delisted != Some(true))
            .map(|(index, a)| a.rules(index, builder))
            .collect()
    }

    /// Every live perp on `dex` (`None`: the core perps) with its context
    /// now (24 h volume, mark, funding), delisted ones left out.
    pub async fn perp_listings(&self, dex: Option<&str>) -> Result<Vec<PerpListing>, HlError> {
        let raw = self.post(json!({ "type": "metaAndAssetCtxs", "dex": dex.unwrap_or("") })).await?;
        read_listings(raw, dex.is_some_and(|d| !d.is_empty()))
    }

    /// Every perp's mid on `dex` (`None`: the core perps), by coin as
    /// Hyperliquid names it.
    pub async fn all_mids(&self, dex: Option<&str>) -> Result<std::collections::HashMap<String, f64>, HlError> {
        let raw = self.post(json!({ "type": "allMids", "dex": dex.unwrap_or("") })).await?;
        let map = raw.as_object().ok_or_else(|| HlError::Decode("allMids is not an object".into()))?;
        Ok(map.iter().filter_map(|(coin, v)| v.as_str().and_then(|s| num(s).ok()).map(|px| (coin.clone(), px))).collect())
    }

    /// The dexes in `perpDexs` order (index 0, the core perps, has no
    /// name): what [`asset_id`] counts a builder-deployed perp's dex by.
    pub async fn perp_dexes(&self) -> Result<Vec<Dex>, HlError> {
        let raw = self.post(json!({ "type": "perpDexs" })).await?;
        let list = raw.as_array().ok_or_else(|| HlError::Decode("perpDexs is not a list".into()))?;
        Ok(list
            .iter()
            .map(|d| Dex {
                name: d.get("name").and_then(Value::as_str).map(str::to_string),
                full_name: d.get("fullName").and_then(Value::as_str).map(str::to_string),
            })
            .collect())
    }
}

fn read_listings(raw: Value, builder: bool) -> Result<Vec<PerpListing>, HlError> {
    let pair = raw.as_array().filter(|a| a.len() == 2).ok_or_else(|| HlError::Decode("metaAndAssetCtxs is not [meta, ctxs]".into()))?;
    let meta: RawMeta = serde_json::from_value(pair[0].clone()).map_err(|e| HlError::Decode(e.to_string()))?;
    let ctxs: Vec<Option<RawCtx>> = serde_json::from_value(pair[1].clone()).map_err(|e| HlError::Decode(e.to_string()))?;
    let mut out = Vec::new();
    for (index, (asset, ctx)) in meta.universe.into_iter().zip(ctxs.into_iter().chain(std::iter::repeat_with(|| None))).enumerate() {
        if asset.is_delisted == Some(true) {
            continue;
        }
        let rules = asset.rules(index, builder)?;
        let ctx = ctx.and_then(|c| c.read(rules.coin.clone()).ok());
        out.push(PerpListing { rules, ctx });
    }
    Ok(out)
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

    #[test]
    fn fees_follow_the_deployers_scale_and_growth_mode() {
        let rules = |scale: Option<f64>, growth: bool| PerpRules { coin: "x".into(), index: 0, sz_decimals: 2, max_leverage: 10, only_isolated: false, deployer_fee_scale: scale, growth_mode: growth };
        let close = |(a, b): (f64, f64), (c, d): (f64, f64)| (a - c).abs() < 1e-12 && (b - d).abs() < 1e-12;
        assert!(close(rules(None, false).fees(), (0.00045, 0.00015)), "a core perp");
        assert!(close(rules(Some(1.0), false).fees(), (0.0009, 0.0003)), "trade.xyz's gold: double");
        assert!(close(rules(Some(0.5), false).fees(), (0.000675, 0.000225)));
        assert!(close(rules(Some(1.0), true).fees(), (0.00009, 0.00003)), "growth mode: a tenth");
        assert!(close(rules(Some(2.0), false).fees(), (0.0018, 0.0006)));
    }

    #[test]
    fn listings_skip_delisted_perps_and_keep_their_index() {
        let raw: Value = serde_json::from_str(
            r#"[{"universe":[
                {"szDecimals":4,"name":"xyz:XYZ100","maxLeverage":30,"marginTableId":30,"growthMode":"enabled","deployerFeeScale":"1.0"},
                {"szDecimals":2,"name":"xyz:OLD","maxLeverage":10,"isDelisted":true,"deployerFeeScale":"1.0"},
                {"szDecimals":4,"name":"xyz:GOLD","maxLeverage":25,"deployerFeeScale":"1.0","onlyIsolated":true,"marginMode":"noCross"}
              ]},
              [{"funding":"0.00000625","openInterest":"9609.9","prevDayPx":"27036.0","dayNtlVlm":"609683800.4","oraclePx":"26702.0","markPx":"26704.0","midPx":"26705.5"},
               null,
               {"funding":"0.0000125","openInterest":"66816.18","prevDayPx":"3720.0","dayNtlVlm":"54648087.1","oraclePx":"3733.5","markPx":"3733.0","midPx":null}]]"#,
        )
        .unwrap();
        let l = read_listings(raw, true).unwrap();
        assert_eq!(l.iter().map(|x| (x.rules.coin.as_str(), x.rules.index)).collect::<Vec<_>>(), [("xyz:XYZ100", 0), ("xyz:GOLD", 2)]);
        assert!(l[0].rules.growth_mode && !l[1].rules.growth_mode && l[1].rules.only_isolated);
        assert_eq!(l[0].ctx.as_ref().map(|c| c.day_volume), Some(609_683_800.4));
        assert_eq!(l[1].ctx.as_ref().map(|c| c.mid), Some(None));
    }
}
