//! Upstox Market Data Feed V3: live last-traded prices over one WebSocket.
//!
//! Read-only, like the rest of this crate. Requests are JSON sent in binary
//! frames; every response is a protobuf `FeedResponse` from Upstox's
//! `MarketDataFeedV3.proto`. Only the `ltpc` mode is used, and only the fields
//! it carries are declared here — prost skips every other field on the wire,
//! so the rest of the schema can grow without breaking this.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use prost::Message as _;
use serde::Deserialize;
use tokio::sync::{mpsc, watch};
use tokio::time::MissedTickBehavior;
use tokio_tungstenite::tungstenite::Message;

use crate::{UpstoxClient, UpstoxError};

/// How often this side pings, so a socket that died quietly is noticed.
const PING_EVERY: Duration = Duration::from_secs(15);

/// `FeedResponse`, as far as `ltpc` mode needs it.
#[derive(Clone, PartialEq, prost::Message)]
pub struct FeedResponse {
    /// 0 `initial_feed`, 1 `live_feed`, 2 `market_info`.
    #[prost(int32, tag = "1")]
    pub r#type: i32,
    #[prost(map = "string, message", tag = "2")]
    pub feeds: HashMap<String, Feed>,
    #[prost(int64, tag = "3")]
    pub current_ts: i64,
}

/// `Feed`. In `ltpc` mode its `FeedUnion` oneof holds `ltpc` (field 1).
#[derive(Clone, PartialEq, prost::Message)]
pub struct Feed {
    #[prost(message, optional, tag = "1")]
    pub ltpc: Option<Ltpc>,
}

/// `LTPC`: last traded price, its time and quantity, and the previous close.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Ltpc {
    #[prost(double, tag = "1")]
    pub ltp: f64,
    /// Last trade time, epoch millis.
    #[prost(int64, tag = "2")]
    pub ltt: i64,
    #[prost(int64, tag = "3")]
    pub ltq: i64,
    #[prost(double, tag = "4")]
    pub cp: f64,
}

/// One last-traded price from the feed.
#[derive(Debug, Clone, PartialEq)]
pub struct LtpTick {
    /// Upstox instrument key, e.g. `NSE_FO|51342`.
    pub key: String,
    pub ltp: f64,
    /// Exchange time of the last trade, epoch millis.
    pub ltt_ms: i64,
}

/// The prices in one feed message: none for a market-info message, and never
/// one that isn't a positive, finite price.
pub fn decode_ltps(frame: &[u8]) -> Result<Vec<LtpTick>, UpstoxError> {
    let resp = FeedResponse::decode(frame).map_err(|e| UpstoxError::Feed(format!("decode: {e}")))?;
    Ok(resp
        .feeds
        .into_iter()
        .filter_map(|(key, feed)| {
            let l = feed.ltpc?;
            (l.ltp.is_finite() && l.ltp > 0.0).then_some(LtpTick { key, ltp: l.ltp, ltt_ms: l.ltt })
        })
        .collect())
}

/// A subscribe / unsubscribe request, which V3 takes as JSON in a binary frame.
fn request(method: &str, keys: &[String], seq: &mut u64) -> Message {
    *seq += 1;
    let body = serde_json::json!({
        "guid": format!("auto-trader-{seq}"),
        "method": method,
        "data": { "mode": "ltpc", "instrumentKeys": keys },
    });
    Message::Binary(body.to_string().into_bytes())
}

fn feed_error(e: impl std::fmt::Display) -> UpstoxError {
    UpstoxError::Feed(e.to_string())
}

impl UpstoxClient {
    /// A single-use `wss://` URL for the V3 market-data feed.
    pub async fn authorize_feed(&self) -> Result<String, UpstoxError> {
        #[derive(Deserialize)]
        struct Authorized {
            authorized_redirect_uri: String,
        }
        let auth: Authorized = self.get_v3("/feed/market-data-feed/authorize", "feed_authorize").await?;
        Ok(auth.authorized_redirect_uri)
    }

    /// Stream the price of every key in `keys` into `out` over one connection,
    /// subscribing and unsubscribing as `keys` changes. Returns `Ok` once
    /// `keys`' sender or `out`'s receiver is dropped — the caller's way to stop
    /// it — and an error when the connection fails. Reconnecting is up to the
    /// caller.
    pub async fn stream_ltps(
        &self,
        mut keys: watch::Receiver<BTreeSet<String>>,
        out: mpsc::Sender<LtpTick>,
    ) -> Result<(), UpstoxError> {
        let url = self.authorize_feed().await?;
        let (ws, _) = tokio_tungstenite::connect_async(url.as_str()).await.map_err(feed_error)?;
        let (mut sink, mut stream) = ws.split();
        let mut subscribed: BTreeSet<String> = BTreeSet::new();
        let mut seq = 0u64;
        let mut undecodable = 0u64;
        let mut ping = tokio::time::interval(PING_EVERY);
        ping.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            let wanted = keys.borrow_and_update().clone();
            let added: Vec<String> = wanted.difference(&subscribed).cloned().collect();
            let removed: Vec<String> = subscribed.difference(&wanted).cloned().collect();
            if !added.is_empty() {
                sink.send(request("sub", &added, &mut seq)).await.map_err(feed_error)?;
            }
            if !removed.is_empty() {
                sink.send(request("unsub", &removed, &mut seq)).await.map_err(feed_error)?;
            }
            subscribed = wanted;

            tokio::select! {
                changed = keys.changed() => {
                    if changed.is_err() {
                        let _ = sink.close().await;
                        return Ok(());
                    }
                }
                _ = ping.tick() => {
                    sink.send(Message::Ping(Vec::new())).await.map_err(feed_error)?;
                }
                msg = stream.next() => match msg {
                    Some(Ok(Message::Binary(frame))) => match decode_ltps(&frame) {
                        Ok(ticks) => {
                            for tick in ticks {
                                if out.send(tick).await.is_err() {
                                    let _ = sink.close().await;
                                    return Ok(());
                                }
                            }
                        }
                        Err(e) => {
                            undecodable += 1;
                            if undecodable == 1 {
                                tracing::warn!(error = %e, "Upstox feed sent a frame that could not be decoded — skipped");
                            }
                        }
                    },
                    Some(Ok(Message::Close(frame))) => {
                        return Err(UpstoxError::Feed(format!("closed by Upstox: {frame:?}")));
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(feed_error(e)),
                    None => return Err(UpstoxError::Feed("connection ended".into())),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(kind: i32, prices: &[(&str, f64)]) -> FeedResponse {
        FeedResponse {
            r#type: kind,
            feeds: prices
                .iter()
                .map(|(key, ltp)| {
                    let ltpc = Ltpc { ltp: *ltp, ltt: 1_792_570_000_000, ltq: 75, cp: 23_000.0 };
                    (key.to_string(), Feed { ltpc: Some(ltpc) })
                })
                .collect(),
            current_ts: 1_792_570_000_500,
        }
    }

    #[test]
    fn a_live_feed_message_decodes_to_its_prices() {
        let frame = response(1, &[("NSE_INDEX|Nifty 50", 22_738.45), ("NSE_FO|51342", 159.55)]).encode_to_vec();
        let mut ticks = decode_ltps(&frame).unwrap();
        ticks.sort_by(|a, b| a.key.cmp(&b.key));
        assert_eq!(ticks, vec![
            LtpTick { key: "NSE_FO|51342".into(), ltp: 159.55, ltt_ms: 1_792_570_000_000 },
            LtpTick { key: "NSE_INDEX|Nifty 50".into(), ltp: 22_738.45, ltt_ms: 1_792_570_000_000 },
        ]);
    }

    #[test]
    fn fields_outside_ltpc_mode_are_skipped_and_bad_prices_dropped() {
        // A zero price is not a price.
        let mut frame = response(1, &[("NSE_FO|1", 0.0), ("NSE_FO|2", 101.5)]).encode_to_vec();
        // `marketInfo` (field 4, length-delimited), which this module doesn't declare.
        frame.extend_from_slice(&[0x22, 0x02, 0x08, 0x01]);
        let ticks = decode_ltps(&frame).unwrap();
        assert_eq!(ticks.len(), 1);
        assert_eq!((ticks[0].key.as_str(), ticks[0].ltp), ("NSE_FO|2", 101.5));

        // A market-info message carries no feeds at all.
        assert!(decode_ltps(&response(2, &[]).encode_to_vec()).unwrap().is_empty());
        // Garbage is an error, not a price.
        assert!(decode_ltps(&[0xff, 0xff, 0xff]).is_err());
    }

    #[test]
    fn requests_are_json_in_binary_frames() {
        let mut seq = 0;
        let Message::Binary(body) = request("sub", &["NSE_INDEX|Nifty 50".to_string()], &mut seq) else {
            panic!("V3 takes requests as binary frames");
        };
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["method"], "sub");
        assert_eq!(json["data"]["mode"], "ltpc");
        assert_eq!(json["data"]["instrumentKeys"][0], "NSE_INDEX|Nifty 50");
        assert_eq!(seq, 1);
    }
}
