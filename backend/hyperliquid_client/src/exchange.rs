//! Hyperliquid's `exchange` endpoint: a signed action posted, its answer
//! read. Every action here is signed by an API wallet ([`crate::signing`]),
//! which can trade but never withdraw or transfer.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};

use crate::signing::{sign_l1, Wallet};
use crate::{HlError, Network};

const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(10);

/// A nonce for each signed action: Unix ms, and always more than the last
/// one handed out (Hyperliquid refuses a nonce it has seen).
#[derive(Default)]
pub struct Nonces {
    last: AtomicU64,
}

impl Nonces {
    pub fn next(&self, now_ms: u64) -> u64 {
        let mut prev = self.last.load(Ordering::Relaxed);
        loop {
            let next = now_ms.max(prev + 1);
            match self.last.compare_exchange_weak(prev, next, Ordering::SeqCst, Ordering::Relaxed) {
                Ok(_) => return next,
                Err(seen) => prev = seen,
            }
        }
    }
}

/// What Hyperliquid answered: `ok` with its `response`, or `err` and why.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Ok(Value),
    Err(String),
}

#[derive(Clone)]
pub struct ExchangeClient {
    http: reqwest::Client,
    network: Network,
}

impl ExchangeClient {
    pub fn new(network: Network) -> Self {
        let http = reqwest::Client::builder().timeout(EXCHANGE_TIMEOUT).build().unwrap_or_default();
        ExchangeClient { http, network }
    }

    pub fn network(&self) -> Network {
        self.network
    }

    /// Sign `action` with `wallet` and post it. A transport failure (no
    /// answer) is an `Err`; Hyperliquid refusing it is `Ok(Answer::Err)`.
    pub async fn post<A: Serialize>(&self, wallet: &Wallet, action: &A, nonce: u64) -> Result<Answer, HlError> {
        let signature = sign_l1(wallet, action, nonce, self.network)?;
        let body = json!({ "action": action, "nonce": nonce, "signature": signature, "vaultAddress": null, "expiresAfter": null });
        let resp = self.http.post(format!("{}/exchange", self.network.api_url())).json(&body).send().await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let v: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) if !status.is_success() => return Err(HlError::Status { status: status.as_u16(), body: text.chars().take(300).collect() }),
            Err(e) => return Err(HlError::Decode(format!("exchange answer: {e}"))),
        };
        match v.get("status").and_then(Value::as_str) {
            Some("ok") => Ok(Answer::Ok(v.get("response").cloned().unwrap_or(Value::Null))),
            Some("err") => Ok(Answer::Err(v.get("response").map_or_else(String::new, |r| r.as_str().map_or_else(|| r.to_string(), str::to_string)))),
            _ => Err(HlError::Decode(format!("exchange answer without a status: {}", text.chars().take(300).collect::<String>()))),
        }
    }

    /// Sign and post `action`, and read what each order in it came to.
    /// Hyperliquid refusing the action as a whole is an `Err` with its words.
    pub async fn orders<A: Serialize>(&self, wallet: &Wallet, action: &A, nonce: u64) -> Result<Vec<OrderStatus>, HlError> {
        match self.post(wallet, action, nonce).await? {
            Answer::Err(why) => Err(HlError::Refused(why)),
            Answer::Ok(response) => read_statuses(&response),
        }
    }

    /// Sign and post an action that answers no per-order statuses (leverage).
    pub async fn act<A: Serialize>(&self, wallet: &Wallet, action: &A, nonce: u64) -> Result<(), HlError> {
        match self.post(wallet, action, nonce).await? {
            Answer::Err(why) => Err(HlError::Refused(why)),
            Answer::Ok(_) => Ok(()),
        }
    }

    /// A signed action that does nothing: proves the API wallet is one the
    /// exchange takes, and trades nothing.
    pub async fn noop(&self, wallet: &Wallet, nonce: u64) -> Result<Answer, HlError> {
        #[derive(Serialize)]
        struct Noop {
            #[serde(rename = "type")]
            kind: &'static str,
        }
        self.post(wallet, &Noop { kind: "noop" }, nonce).await
    }
}

/// What one order of an action came to.
#[derive(Debug, Clone, PartialEq)]
pub enum OrderStatus {
    /// Resting on the book (a limit, or a trigger waiting for its price).
    Resting { oid: u64 },
    /// Filled now, all or part of it (an IOC's unfilled rest is cancelled).
    Filled { oid: u64, size: f64, avg_px: f64 },
    /// A cancel or modify that went through.
    Success,
    /// A stop or target sent with its entry (`normalTpsl`), live once the
    /// entry fills: `waitingForFill`, `waitingForTrigger`.
    Waiting(String),
    /// Refused, in Hyperliquid's words.
    Error(String),
}

/// The statuses of an `order`, `cancel` or `batchModify` answer, one per
/// order sent, in order: `{"type":…,"data":{"statuses":[…]}}`.
pub fn read_statuses(response: &Value) -> Result<Vec<OrderStatus>, HlError> {
    let statuses = response
        .get("data")
        .and_then(|d| d.get("statuses"))
        .and_then(Value::as_array)
        .ok_or_else(|| HlError::Decode(format!("an answer without statuses: {}", response.to_string().chars().take(300).collect::<String>())))?;
    statuses
        .iter()
        .map(|st| {
            if st.as_str() == Some("success") {
                return Ok(OrderStatus::Success);
            }
            if let Some(e) = st.get("error").and_then(Value::as_str) {
                return Ok(OrderStatus::Error(e.to_string()));
            }
            if let Some(r) = st.get("resting") {
                let oid = r.get("oid").and_then(Value::as_u64).ok_or_else(|| HlError::Decode(format!("resting without an oid: {st}")))?;
                return Ok(OrderStatus::Resting { oid });
            }
            if let Some(f) = st.get("filled") {
                let oid = f.get("oid").and_then(Value::as_u64).ok_or_else(|| HlError::Decode(format!("filled without an oid: {st}")))?;
                let size = f.get("totalSz").and_then(Value::as_str).map(crate::num).transpose()?.unwrap_or(0.0);
                let avg_px = f.get("avgPx").and_then(Value::as_str).map(crate::num).transpose()?.unwrap_or(0.0);
                return Ok(OrderStatus::Filled { oid, size, avg_px });
            }
            // "waitingForFill" / "waitingForTrigger": a TP/SL child, live once its entry fills.
            if let Some(w) = st.as_str() {
                return Ok(OrderStatus::Waiting(w.to_string()));
            }
            Err(HlError::Decode(format!("an order status this client doesn't know: {st}")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_says_what_each_order_came_to() {
        let v: Value = serde_json::from_str(
            r#"{"type":"order","data":{"statuses":[{"resting":{"oid":77738308}},{"filled":{"totalSz":"0.02","avgPx":"1891.4","oid":77747314}},{"error":"Order could not immediately match against any resting orders. asset=4"},"success","waitingForFill"]}}"#,
        )
        .unwrap();
        assert_eq!(
            read_statuses(&v).unwrap(),
            [
                OrderStatus::Resting { oid: 77738308 },
                OrderStatus::Filled { oid: 77747314, size: 0.02, avg_px: 1891.4 },
                OrderStatus::Error("Order could not immediately match against any resting orders. asset=4".into()),
                OrderStatus::Success,
                OrderStatus::Waiting("waitingForFill".into()),
            ]
        );
        assert!(read_statuses(&serde_json::json!({"type":"order"})).is_err());
    }

    #[test]
    fn nonces_only_ever_rise() {
        let n = Nonces::default();
        assert_eq!(n.next(1_000), 1_000);
        assert_eq!(n.next(1_000), 1_001, "the same millisecond twice");
        assert_eq!(n.next(900), 1_002, "a clock that stepped back");
        assert_eq!(n.next(5_000), 5_000);
    }
}
