//! One WebSocket to Hyperliquid for live candles and each perp's context.
//!
//! [`run`] connects, subscribes, and hands every candle and context update
//! to its channel until told to stop; a dropped connection is reopened with
//! a backoff, and [`FeedMsg::Up`] / [`FeedMsg::Down`] say when it is live.
//! What it follows can change while it runs: each change is subscribed and
//! unsubscribed on the open connection, and a reconnect subscribes what is
//! wanted then.
//! A ping goes out every [`PING_EVERY`] so Hyperliquid never closes a quiet
//! connection (it does after a minute without traffic).

use std::collections::BTreeSet;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;

use crate::{AssetCtx, Candle, Network, RawCandle, RawCtx};

const PING_EVERY: Duration = Duration::from_secs(30);
/// A connect, or its subscribes, that takes longer than this has stalled
/// (a TLS or WebSocket handshake that never ends): it counts as a failed
/// attempt and is retried.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Nothing at all for this long (not even a pong): the connection is dead.
const SILENT_FOR: Duration = Duration::from_secs(75);
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// What to follow.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Sub {
    /// `coin`'s candles at `interval` (`1m`, `5m`, `15m`, …).
    Candle { coin: String, interval: String },
    /// `coin`'s context: mark, mid, oracle price, funding.
    Ctx { coin: String },
}

impl Sub {
    fn subscription(&self) -> Value {
        match self {
            Sub::Candle { coin, interval } => json!({ "type": "candle", "coin": coin, "interval": interval }),
            Sub::Ctx { coin } => json!({ "type": "activeAssetCtx", "coin": coin }),
        }
    }

    fn message(&self) -> Value {
        json!({ "method": "subscribe", "subscription": self.subscription() })
    }

    fn unsubscribe(&self) -> Value {
        json!({ "method": "unsubscribe", "subscription": self.subscription() })
    }
}

/// What the feed hands on.
#[derive(Debug, Clone, PartialEq)]
pub enum FeedMsg {
    /// Connected and subscribed.
    Up,
    /// The connection dropped; it is being reopened.
    Down(String),
    Candle(Candle),
    Ctx(AssetCtx),
}

#[derive(Deserialize)]
struct Envelope {
    channel: String,
    #[serde(default)]
    data: Value,
}

#[derive(Deserialize)]
struct CtxData {
    coin: String,
    ctx: RawCtx,
}

/// Read one text frame: a candle, a context, or nothing the caller wants
/// (a subscription answer, a pong, an error, which is logged).
pub fn parse(text: &str) -> Option<FeedMsg> {
    let env: Envelope = serde_json::from_str(text).ok()?;
    match env.channel.as_str() {
        "candle" => {
            let raw: RawCandle = serde_json::from_value(env.data).ok()?;
            Candle::try_from(raw).ok().map(FeedMsg::Candle)
        }
        "activeAssetCtx" => {
            let d: CtxData = serde_json::from_value(env.data).ok()?;
            d.ctx.read(d.coin).ok().map(FeedMsg::Ctx)
        }
        "error" => {
            tracing::warn!("Hyperliquid feed error: {}", env.data);
            None
        }
        _ => None,
    }
}

/// Follow what `subs` holds on `network` (as it changes), handing updates to
/// `tx`, until `stop` turns true or `tx` is dropped.
pub async fn run(network: Network, mut subs: watch::Receiver<BTreeSet<Sub>>, tx: mpsc::Sender<FeedMsg>, mut stop: watch::Receiver<bool>) {
    let mut backoff = BACKOFF_START;
    loop {
        if *stop.borrow() || tx.is_closed() {
            return;
        }
        let why = match tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(network.ws_url())).await {
            Err(_) => "connect timed out".to_string(),
            Ok(Err(e)) => format!("connect: {e}"),
            Ok(Ok((ws, _))) => {
                let (mut sink, mut stream) = ws.split();
                let mut active: BTreeSet<Sub> = subs.borrow_and_update().clone();
                let subscribed = tokio::time::timeout(CONNECT_TIMEOUT, async {
                    for sub in &active {
                        sink.send(Message::Text(sub.message().to_string())).await?;
                    }
                    Ok::<_, tokio_tungstenite::tungstenite::Error>(())
                })
                .await;
                if let Some(why) = match subscribed {
                    Err(_) => Some("subscribe timed out".to_string()),
                    Ok(Err(e)) => Some(format!("subscribe: {e}")),
                    Ok(Ok(())) => None,
                } {
                    why
                } else {
                    if tx.send(FeedMsg::Up).await.is_err() {
                        return;
                    }
                    backoff = BACKOFF_START;
                    let mut ping = tokio::time::interval(PING_EVERY);
                    ping.tick().await;
                    let mut last_heard = tokio::time::Instant::now();
                    loop {
                        tokio::select! {
                            _ = stop.changed() => {
                                if *stop.borrow() {
                                    let _ = sink.send(Message::Close(None)).await;
                                    return;
                                }
                            }
                            changed = subs.changed() => {
                                if changed.is_err() {
                                    let _ = sink.send(Message::Close(None)).await;
                                    return;
                                }
                                let wanted = subs.borrow_and_update().clone();
                                let mut failed = None;
                                for (sub, on) in diff(&active, &wanted) {
                                    let msg = if on { sub.message() } else { sub.unsubscribe() };
                                    if let Err(e) = sink.send(Message::Text(msg.to_string())).await {
                                        failed = Some(format!("subscribe: {e}"));
                                        break;
                                    }
                                }
                                if let Some(why) = failed {
                                    break why;
                                }
                                active = wanted;
                            }
                            _ = ping.tick() => {
                                if last_heard.elapsed() > SILENT_FOR {
                                    break "silent".to_string();
                                }
                                if sink.send(Message::Text(json!({ "method": "ping" }).to_string())).await.is_err() {
                                    break "ping failed".to_string();
                                }
                            }
                            frame = stream.next() => match frame {
                                None => break "closed".to_string(),
                                Some(Err(e)) => break format!("read: {e}"),
                                Some(Ok(Message::Text(text))) => {
                                    last_heard = tokio::time::Instant::now();
                                    if let Some(msg) = parse(&text) {
                                        if tx.send(msg).await.is_err() {
                                            return;
                                        }
                                    }
                                }
                                Some(Ok(Message::Ping(p))) => {
                                    last_heard = tokio::time::Instant::now();
                                    let _ = sink.send(Message::Pong(p)).await;
                                }
                                Some(Ok(Message::Close(_))) => break "closed by Hyperliquid".to_string(),
                                Some(Ok(_)) => last_heard = tokio::time::Instant::now(),
                            }
                        }
                    }
                }
            }
        };
        tracing::warn!(network = network.as_str(), "Hyperliquid feed down: {why} — reconnecting in {:?}", backoff);
        if tx.send(FeedMsg::Down(why)).await.is_err() {
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = stop.changed() => {}
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// What to send to go from following `active` to following `wanted`:
/// each sub with `true` to subscribe it, `false` to unsubscribe it.
fn diff<'a>(active: &'a BTreeSet<Sub>, wanted: &'a BTreeSet<Sub>) -> Vec<(&'a Sub, bool)> {
    active.difference(wanted).map(|s| (s, false)).chain(wanted.difference(active).map(|s| (s, true))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_unsubscribes_what_went_and_subscribes_what_came() {
        let ctx = |c: &str| Sub::Ctx { coin: c.into() };
        let active: BTreeSet<Sub> = [ctx("BTC"), ctx("ETH")].into();
        let wanted: BTreeSet<Sub> = [ctx("ETH"), ctx("xyz:NVDA")].into();
        assert_eq!(diff(&active, &wanted), [(&ctx("BTC"), false), (&ctx("xyz:NVDA"), true)]);
        assert!(diff(&wanted, &wanted).is_empty());
        assert_eq!(ctx("BTC").unsubscribe()["method"], "unsubscribe");
        assert_eq!(ctx("BTC").unsubscribe()["subscription"]["coin"], "BTC");
    }

    #[test]
    fn the_feed_reads_candles_and_contexts_and_skips_the_rest() {
        let candle = parse(r#"{"channel":"candle","data":{"t":1795081140000,"T":1795081199999,"s":"BTC","i":"1m","o":"63828.0","c":"63807.0","h":"63840.0","l":"63789.0","v":"1.22095","n":72}}"#);
        assert!(matches!(candle, Some(FeedMsg::Candle(c)) if c.coin == "BTC" && c.c == 63807.0));
        let ctx = parse(r#"{"channel":"activeAssetCtx","data":{"coin":"xyz:GOLD","ctx":{"funding":"0.0000125","openInterest":"66816.18","prevDayPx":"3720.0","dayNtlVlm":"54648087.1","oraclePx":"3733.5","markPx":"3733.0","midPx":null}}}"#);
        assert!(matches!(ctx, Some(FeedMsg::Ctx(c)) if c.coin == "xyz:GOLD" && c.mark == 3733.0 && c.mid.is_none()));
        assert_eq!(parse(r#"{"channel":"subscriptionResponse","data":{"method":"subscribe"}}"#), None);
        assert_eq!(parse(r#"{"channel":"pong"}"#), None);
        assert_eq!(parse("not json"), None);
    }

    #[test]
    fn a_subscription_names_its_channel() {
        let c = Sub::Candle { coin: "xyz:GOLD".into(), interval: "5m".into() }.message();
        assert_eq!(c["subscription"]["type"], "candle");
        assert_eq!(c["subscription"]["coin"], "xyz:GOLD");
        assert_eq!(Sub::Ctx { coin: "BTC".into() }.message()["subscription"]["type"], "activeAssetCtx");
    }
}
