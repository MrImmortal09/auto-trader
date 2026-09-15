//! Read-only Upstox client for market data.
//!
//! Authenticates with an **Analytics Token** (generated once a year from the
//! Upstox Developer Apps page → Analytics tab). That token is read-only by
//! design — it cannot place, modify or cancel orders — so nothing in this crate
//! can ever touch money. Order execution stays on Kotak.
//!
//! Endpoints used (all documented at upstox.com/developer/api-documentation):
//! - `GET /v2/option/contract`      — list option contracts → nearest expiry
//! - `GET /v2/option/chain`         — full put/call chain with `oi` + `prev_oi`
//! - `GET /v2/market-quote/quotes`  — full quote (`last_price`, `average_price`)
//! - instruments JSON (gzip)        — to find the current-month index future

use std::collections::HashMap;
use std::fmt;
use std::io::BufReader;
use std::time::Duration;

use chrono::{DateTime, NaiveDate};
use serde::de::{DeserializeOwned, DeserializeSeed, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

const API_BASE: &str = "https://api.upstox.com/v2";
/// Per-request budget for the small JSON API calls.
const API_TIMEOUT: Duration = Duration::from_secs(20);
/// Budget for downloading a multi-MB gzipped instruments file.
const INSTRUMENTS_TIMEOUT: Duration = Duration::from_secs(180);
pub const NSE_INSTRUMENTS_URL: &str =
    "https://assets.upstox.com/market-quote/instruments/exchange/NSE.json.gz";
pub const BSE_INSTRUMENTS_URL: &str =
    "https://assets.upstox.com/market-quote/instruments/exchange/BSE.json.gz";

#[derive(Debug, thiserror::Error)]
pub enum UpstoxError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("upstox api returned HTTP {status}: {body}")]
    Api { status: u16, body: String },
    #[error("decode error: {0}")]
    Decode(String),
}

impl UpstoxError {
    /// 401/403 — the analytics token is missing, revoked, or expired.
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, UpstoxError::Api { status: 401 | 403, .. })
    }
}

/// Accepts a JSON number, a numeric string, or null (→ 0.0). Upstox returns
/// `null` for fields on illiquid strikes, which a plain `f64` would reject.
fn num<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(match Option::<serde_json::Value>::deserialize(d)? {
        Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(serde_json::Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    })
}

/// Like [`num`], but keeps "absent" distinct from zero: null, missing or
/// unparseable → `None`.
fn opt_num<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    Ok(match Option::<serde_json::Value>::deserialize(d)? {
        Some(serde_json::Value::Number(n)) => n.as_f64(),
        Some(serde_json::Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    })
}

#[derive(Deserialize)]
struct Envelope<T> {
    data: T,
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MarketData {
    #[serde(default, deserialize_with = "num")]
    pub ltp: f64,
    #[serde(default, deserialize_with = "num")]
    pub volume: f64,
    #[serde(default, deserialize_with = "num")]
    pub oi: f64,
    /// `None` when Upstox sends null. Callers must not treat that as zero, or
    /// the strike's entire OI would count as a same-day build-up.
    #[serde(default, deserialize_with = "opt_num")]
    pub prev_oi: Option<f64>,
    #[serde(default, deserialize_with = "num")]
    pub close_price: f64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct OptionGreeks {
    #[serde(default, deserialize_with = "num")]
    pub iv: f64,
    #[serde(default, deserialize_with = "num")]
    pub delta: f64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct OptionSide {
    #[serde(default)]
    pub instrument_key: String,
    #[serde(default)]
    pub market_data: Option<MarketData>,
    #[serde(default)]
    pub option_greeks: Option<OptionGreeks>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OptionChainStrike {
    #[serde(default)]
    pub expiry: String,
    #[serde(default, deserialize_with = "num")]
    pub strike_price: f64,
    #[serde(default, deserialize_with = "num")]
    pub underlying_spot_price: f64,
    #[serde(default)]
    pub call_options: Option<OptionSide>,
    #[serde(default)]
    pub put_options: Option<OptionSide>,
}

#[derive(Debug, Clone, Deserialize)]
struct OptionContract {
    #[serde(default)]
    expiry: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Quote {
    /// Pipe form (`NSE_FO|12345`) — the response map itself is keyed by the
    /// colon/trading-symbol form, so match on this field instead.
    #[serde(default)]
    pub instrument_token: String,
    #[serde(default, deserialize_with = "num")]
    pub last_price: f64,
    /// Exchange average traded price for the day, i.e. VWAP.
    #[serde(default, deserialize_with = "num")]
    pub average_price: f64,
    #[serde(default, deserialize_with = "num")]
    pub volume: f64,
    #[serde(default, deserialize_with = "num")]
    pub oi: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FutureContract {
    pub underlying_symbol: String,
    pub instrument_key: String,
    pub trading_symbol: String,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct UpstoxClient {
    http: reqwest::Client,
    token: String,
}

impl UpstoxClient {
    pub fn new(analytics_token: impl Into<String>) -> Result<Self, UpstoxError> {
        // No client-wide total timeout: API calls and the large instruments
        // download get separate per-request budgets.
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self { http, token: analytics_token.into() })
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<T, UpstoxError> {
        let url = reqwest::Url::parse_with_params(&format!("{API_BASE}{path}"), params)
            .map_err(|e| UpstoxError::Decode(e.to_string()))?;
        let resp = self
            .http
            .get(url)
            .timeout(API_TIMEOUT)
            .bearer_auth(&self.token)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(UpstoxError::Api {
                status: status.as_u16(),
                body: body.chars().take(300).collect(),
            });
        }
        serde_json::from_str::<Envelope<T>>(&body)
            .map(|e| e.data)
            .map_err(|e| UpstoxError::Decode(format!("{path}: {e}")))
    }

    /// Nearest option expiry on or after `today` for an underlying such as
    /// `NSE_INDEX|Nifty 50`.
    pub async fn nearest_option_expiry(
        &self,
        underlying_key: &str,
        today: NaiveDate,
    ) -> Result<NaiveDate, UpstoxError> {
        let contracts: Vec<OptionContract> = self
            .get_json("/option/contract", &[("instrument_key", underlying_key)])
            .await?;
        contracts
            .iter()
            .filter_map(|c| NaiveDate::parse_from_str(&c.expiry, "%Y-%m-%d").ok())
            .filter(|d| *d >= today)
            .min()
            .ok_or_else(|| UpstoxError::Decode(format!("no live option expiry for {underlying_key}")))
    }

    pub async fn option_chain(
        &self,
        underlying_key: &str,
        expiry: NaiveDate,
    ) -> Result<Vec<OptionChainStrike>, UpstoxError> {
        let expiry = expiry.format("%Y-%m-%d").to_string();
        self.get_json(
            "/option/chain",
            &[("instrument_key", underlying_key), ("expiry_date", &expiry)],
        )
        .await
    }

    /// Full market quotes, returned keyed by pipe-form instrument key.
    pub async fn quotes(&self, keys: &[String]) -> Result<HashMap<String, Quote>, UpstoxError> {
        if keys.is_empty() {
            return Ok(HashMap::new());
        }
        let joined = keys.join(",");
        let raw: HashMap<String, Quote> = self
            .get_json("/market-quote/quotes", &[("instrument_key", &joined)])
            .await?;
        Ok(raw.into_values().map(|q| (q.instrument_token.clone(), q)).collect())
    }

    /// Downloads an instruments file (`NSE_INSTRUMENTS_URL` / `BSE_INSTRUMENTS_URL`)
    /// and returns the nearest still-trading future for each wanted underlying
    /// symbol (e.g. `NIFTY`, `BANKNIFTY`, `SENSEX`). A contract expiring on
    /// `today` (IST) still counts: it trades until the close.
    ///
    /// The decompressed file is tens of MB, and the production VM has under
    /// 1 GB of RAM with no swap, so it is stream-filtered row by row instead of
    /// being materialised.
    pub async fn nearest_index_futures(
        &self,
        instruments_url: &str,
        wanted: &[&str],
        today: NaiveDate,
    ) -> Result<HashMap<String, FutureContract>, UpstoxError> {
        let resp = self.http.get(instruments_url).timeout(INSTRUMENTS_TIMEOUT).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(UpstoxError::Api { status: status.as_u16(), body: String::new() });
        }
        let gz = resp.bytes().await?;
        let wanted: Vec<String> = wanted.iter().map(|s| s.to_string()).collect();

        tokio::task::spawn_blocking(move || {
            let reader = BufReader::new(flate2::read::GzDecoder::new(&gz[..]));
            let mut de = serde_json::Deserializer::from_reader(reader);
            let rows = FutFilter { wanted: &wanted }
                .deserialize(&mut de)
                .map_err(|e| UpstoxError::Decode(format!("instruments: {e}")))?;
            Ok(pick_nearest_futures(rows, today))
        })
        .await
        .map_err(|e| UpstoxError::Decode(e.to_string()))?
    }
}

// ---------------------------------------------------------------------------
// Instruments stream filter
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct InstrumentRow {
    #[serde(default)]
    segment: String,
    #[serde(default)]
    instrument_type: String,
    #[serde(default)]
    underlying_symbol: String,
    #[serde(default)]
    instrument_key: String,
    #[serde(default)]
    trading_symbol: String,
    #[serde(default, deserialize_with = "num")]
    expiry: f64,
}

struct FutFilter<'a> {
    wanted: &'a [String],
}

impl<'de> DeserializeSeed<'de> for FutFilter<'_> {
    type Value = Vec<InstrumentRow>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for FutFilter<'_> {
    type Value = Vec<InstrumentRow>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an array of instruments")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut out = Vec::new();
        while let Some(row) = seq.next_element::<InstrumentRow>()? {
            if row.instrument_type == "FUT"
                && row.segment.ends_with("_FO")
                && self.wanted.iter().any(|w| *w == row.underlying_symbol)
            {
                out.push(row);
            }
        }
        Ok(out)
    }
}

/// Upstox expiry epochs are 00:00 IST of the expiry date, so compare IST
/// dates, not instants: an epoch-millis `exp <= now` check would drop the
/// near contract on its own expiry day while it is still trading.
fn pick_nearest_futures(rows: Vec<InstrumentRow>, today: NaiveDate) -> HashMap<String, FutureContract> {
    let mut best: HashMap<String, (i64, InstrumentRow)> = HashMap::new();
    for row in rows {
        let exp = row.expiry as i64;
        let Some(exp_date) = DateTime::from_timestamp_millis(exp)
            .map(|t| t.with_timezone(&shared_domain::ist_offset()).date_naive())
        else {
            continue;
        };
        if exp_date < today {
            continue;
        }
        match best.get(&row.underlying_symbol) {
            Some((cur, _)) if *cur <= exp => {}
            _ => {
                best.insert(row.underlying_symbol.clone(), (exp, row));
            }
        }
    }
    best.into_iter()
        .map(|(sym, (_, row))| {
            (
                sym,
                FutureContract {
                    underlying_symbol: row.underlying_symbol,
                    instrument_key: row.instrument_key,
                    trading_symbol: row.trading_symbol,
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_documented_option_chain_sample() {
        let body = r#"{"status":"success","data":[{"expiry":"2025-02-13","pcr":7515.3,"strike_price":21100,
            "underlying_key":"NSE_INDEX|Nifty 50","underlying_spot_price":22976.2,
            "call_options":{"instrument_key":"NSE_FO|51059","market_data":{"ltp":2449.9,"volume":0,"oi":750,
              "close_price":2449.9,"bid_price":1856.65,"bid_qty":1125,"ask_price":1941.65,"ask_qty":1125,"prev_oi":1500},
              "option_greeks":{"vega":4.1731,"theta":-472.8941,"gamma":0.0001,"delta":0.743,"iv":262.31,"pop":40.56}},
            "put_options":{"instrument_key":"NSE_FO|51060","market_data":{"ltp":0.3,"volume":22315725,"oi":5636475,
              "close_price":0.35,"bid_price":0.3,"bid_qty":1979400,"ask_price":0.35,"ask_qty":2152500,"prev_oi":null},
              "option_greeks":null}}]}"#;
        let env: Envelope<Vec<OptionChainStrike>> = serde_json::from_str(body).unwrap();
        let s = &env.data[0];
        assert_eq!(s.strike_price, 21100.0);
        let call = s.call_options.as_ref().unwrap().market_data.as_ref().unwrap();
        assert_eq!(call.oi, 750.0);
        assert_eq!(call.prev_oi, Some(1500.0));
        let put = s.put_options.as_ref().unwrap().market_data.as_ref().unwrap();
        assert_eq!(put.prev_oi, None);
    }

    /// Epoch millis of 00:00 IST on `y-m-d`, the form Upstox uses for expiry.
    fn ist_midnight_ms(y: i32, m: u32, d: u32) -> i64 {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_local_timezone(shared_domain::ist_offset())
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn filters_instruments_stream_to_nearest_future() {
        let sep = ist_midnight_ms(2026, 9, 24);
        let oct = ist_midnight_ms(2026, 10, 29);
        let nov = ist_midnight_ms(2026, 11, 26);
        let aug = ist_midnight_ms(2026, 8, 27);
        let json = format!(r#"[
            {{"segment":"NSE_INDEX","instrument_type":"INDEX","instrument_key":"NSE_INDEX|Nifty 50"}},
            {{"segment":"NSE_FO","instrument_type":"FUT","underlying_symbol":"NIFTY","instrument_key":"NSE_FO|3","trading_symbol":"NIFTY FUT NOV","expiry":{nov}}},
            {{"segment":"NSE_FO","instrument_type":"FUT","underlying_symbol":"NIFTY","instrument_key":"NSE_FO|2","trading_symbol":"NIFTY FUT OCT","expiry":{oct}}},
            {{"segment":"NSE_FO","instrument_type":"FUT","underlying_symbol":"NIFTY","instrument_key":"NSE_FO|1","trading_symbol":"NIFTY FUT SEP","expiry":{sep}}},
            {{"segment":"NSE_FO","instrument_type":"FUT","underlying_symbol":"NIFTY","instrument_key":"NSE_FO|0","trading_symbol":"NIFTY FUT AUG","expiry":{aug}}},
            {{"segment":"NSE_FO","instrument_type":"CE","underlying_symbol":"NIFTY","instrument_key":"NSE_FO|9","expiry":{sep}}},
            {{"segment":"NSE_FO","instrument_type":"FUT","underlying_symbol":"RELIANCE","instrument_key":"NSE_FO|7","expiry":{sep}}}
        ]"#);
        let wanted = vec!["NIFTY".to_string()];
        let rows = |json: &str| {
            let mut de = serde_json::Deserializer::from_str(json);
            FutFilter { wanted: &wanted }.deserialize(&mut de).unwrap()
        };
        assert_eq!(rows(&json).len(), 4);
        let date = |d| NaiveDate::from_ymd_opt(2026, 9, d).unwrap();

        // Before expiry day: the September contract.
        let picked = pick_nearest_futures(rows(&json), date(16));
        assert_eq!(picked["NIFTY"].instrument_key, "NSE_FO|1");
        assert!(!picked.contains_key("RELIANCE"));
        // On expiry day itself September still trades, so it is still picked.
        assert_eq!(pick_nearest_futures(rows(&json), date(24))["NIFTY"].instrument_key, "NSE_FO|1");
        // The day after, it rolls to October.
        assert_eq!(pick_nearest_futures(rows(&json), date(25))["NIFTY"].instrument_key, "NSE_FO|2");
    }
}
