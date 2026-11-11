//! Keeps every held paper contract, and its underlying, on the price feed:
//! an index option's index by its feed name, a stock option's stock by its
//! cash row's key (one subscription per stock, however many of its options
//! are held, counted inside the 40-contract cap by `PaperDesk::buy`). A
//! contract is bought off the ladder, whose subscriptions the feed drops at
//! the day's close; a position's own subscription stays, so its chart levels
//! and its square-off have prices the next day too. The only paper code that
//! talks to the market-data bridge, and it only subscribes.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::sync::{mpsc, Mutex};

use super::PaperDesk;

const EVERY: Duration = Duration::from_secs(30);

/// What was sent on the current bridge channel. A re-login starts a fresh
/// bridge with a fresh channel, which knows none of it.
#[derive(Default)]
struct Sent {
    channel: Option<mpsc::UnboundedSender<String>>,
    scrips: HashSet<String>,
    indices: HashSet<String>,
}

/// Subscribe on `tx` whatever of `scrips` (feed keys: contracts and stocks'
/// cash rows alike) and `indices` (index names) it hasn't been sent yet.
fn sync(sent: &mut Sent, tx: &mpsc::UnboundedSender<String>, scrips: BTreeSet<String>, indices: BTreeSet<String>) {
    if !sent.channel.as_ref().is_some_and(|c| c.same_channel(tx)) {
        *sent = Sent { channel: Some(tx.clone()), ..Default::default() };
    }
    let fresh: Vec<String> = scrips.into_iter().filter(|k| !sent.scrips.contains(k)).collect();
    // Without `ladder`: the feed keeps these past the day's close.
    if !fresh.is_empty() && tx.send(json!({ "action": "subscribe", "scrips": fresh.join("&") }).to_string()).is_ok() {
        sent.scrips.extend(fresh);
    }
    let fresh: Vec<(String, String)> = indices
        .into_iter()
        .filter(|i| !sent.indices.contains(i))
        .filter_map(|i| shared_domain::kotak_index_feed(&i).map(|(seg, name)| (i, format!("{seg}|{name}"))))
        .collect();
    if fresh.is_empty() {
        return;
    }
    let feeds: Vec<&str> = fresh.iter().map(|(_, f)| f.as_str()).collect();
    if tx.send(json!({ "action": "subscribe_indices", "indices": feeds.join(",") }).to_string()).is_ok() {
        sent.indices.extend(fresh.into_iter().map(|(i, _)| i));
    }
}

/// Every 30 s, and at once when a buy brings a new stock's cash row onto
/// the books (`PaperDesk::buy` wakes it), subscribe what the desk holds and
/// the feed hasn't been asked for.
pub fn spawn_keeper(desk: Arc<PaperDesk>, ws_tx: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut sent = Sent::default();
        loop {
            // A wake while a pass runs is kept (`notify_one` stores it), so
            // the next pass follows straight on.
            tokio::select! {
                _ = tick.tick() => {}
                _ = desk.feed_wake.notified() => {}
            }
            let Some(tx) = ws_tx.lock().await.clone() else { continue };
            let (scrips, indices) = desk.feed_keys().await;
            sync(&mut sent, &tx, scrips, indices);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paper::tests::{desk, nifty, reliance};

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn each_contract_and_index_is_subscribed_once_per_bridge() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sent = Sent::default();
        sync(&mut sent, &tx, set(&["nse_fo|1", "nse_fo|2"]), set(&["NIFTY"]));
        assert_eq!(rx.try_recv().unwrap(), r#"{"action":"subscribe","scrips":"nse_fo|1&nse_fo|2"}"#);
        assert_eq!(rx.try_recv().unwrap(), r#"{"action":"subscribe_indices","indices":"nse_cm|Nifty 50"}"#);
        sync(&mut sent, &tx, set(&["nse_fo|1", "nse_fo|2", "nse_fo|3"]), set(&["NIFTY"]));
        assert_eq!(rx.try_recv().unwrap(), r#"{"action":"subscribe","scrips":"nse_fo|3"}"#);
        assert!(rx.try_recv().is_err());
        // A stock option brings its stock's cash row along, as a plain scrip.
        sync(&mut sent, &tx, set(&["nse_fo|1", "nse_fo|2", "nse_fo|3", "nse_fo|41222", "nse_cm|2885"]), set(&["NIFTY"]));
        assert_eq!(rx.try_recv().unwrap(), r#"{"action":"subscribe","scrips":"nse_cm|2885&nse_fo|41222"}"#);
        assert!(rx.try_recv().is_err());

        // A new bridge hears everything again.
        let (tx2, mut rx2) = mpsc::unbounded_channel();
        sync(&mut sent, &tx2, set(&["nse_fo|3"]), set(&[]));
        assert_eq!(rx2.try_recv().unwrap(), r#"{"action":"subscribe","scrips":"nse_fo|3"}"#);
        assert!(rx2.try_recv().is_err());
    }

    /// The keeper's next message, or a panic once `within` passes — far
    /// shorter than its 30-second pass.
    async fn next(rx: &mut mpsc::UnboundedReceiver<String>, within: Duration) -> String {
        tokio::time::timeout(within, rx.recv()).await.expect("nothing sent in time").expect("channel open")
    }

    #[tokio::test]
    async fn a_buy_that_brings_a_new_stock_wakes_the_keeper_at_once() {
        let (_db, desk) = desk("feed-keeper-wake").await;
        let desk = Arc::new(desk);
        desk.buy(1, &nifty(25_000.0, "CE"), 1, 100.0).await.unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn_keeper(Arc::clone(&desk), Arc::new(Mutex::new(Some(tx))));
        // The first pass runs at once: the index contract and its index.
        let soon = Duration::from_secs(5);
        assert_eq!(next(&mut rx, soon).await, r#"{"action":"subscribe","scrips":"nse_fo|25000CE"}"#);
        assert_eq!(next(&mut rx, soon).await, r#"{"action":"subscribe_indices","indices":"nse_cm|Nifty 50"}"#);
        // A first RELIANCE option: its contract and the stock's cash row go
        // out now, not on the pass 30 s later — a stop on the stock's price
        // has a value to trigger on straight away.
        desk.buy(2, &reliance(1_400.0, "CE"), 1, 40.0).await.unwrap();
        assert_eq!(next(&mut rx, soon).await, r#"{"action":"subscribe","scrips":"nse_cm|2885&nse_fo|R1400CE"}"#);
        // Nothing else until the next pass.
        assert!(tokio::time::timeout(Duration::from_millis(300), rx.recv()).await.is_err());
    }
}
