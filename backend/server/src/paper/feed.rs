//! Keeps every held paper contract, and its index, on the price feed. A
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

/// Subscribe on `tx` whatever of `scrips` (feed keys) and `indices` (index
/// names) it hasn't been sent yet.
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

/// Every 30 s, subscribe what the desk holds and the feed hasn't been asked for.
pub fn spawn_keeper(desk: Arc<PaperDesk>, ws_tx: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut sent = Sent::default();
        loop {
            tick.tick().await;
            let Some(tx) = ws_tx.lock().await.clone() else { continue };
            let (scrips, indices) = desk.feed_keys().await;
            sync(&mut sent, &tx, scrips, indices);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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

        // A new bridge hears everything again.
        let (tx2, mut rx2) = mpsc::unbounded_channel();
        sync(&mut sent, &tx2, set(&["nse_fo|3"]), set(&[]));
        assert_eq!(rx2.try_recv().unwrap(), r#"{"action":"subscribe","scrips":"nse_fo|3"}"#);
        assert!(rx2.try_recv().is_err());
    }
}
