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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonces_only_ever_rise() {
        let n = Nonces::default();
        assert_eq!(n.next(1_000), 1_000);
        assert_eq!(n.next(1_000), 1_001, "the same millisecond twice");
        assert_eq!(n.next(900), 1_002, "a clock that stepped back");
        assert_eq!(n.next(5_000), 5_000);
    }
}
