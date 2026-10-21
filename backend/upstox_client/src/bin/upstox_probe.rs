//! Standalone check of the Upstox endpoints the v2 candle service uses:
//! intraday and historical candles, the instruments files, and a few seconds
//! of the V3 market-data feed. Read-only, like the analytics token itself.
//!
//! ```text
//! UPSTOX_ANALYTICS_TOKEN=... cargo run -p upstox_client --bin upstox_probe
//! ```

use std::collections::BTreeSet;
use std::time::Duration;

use upstox_client::{UpstoxClient, NSE_INSTRUMENTS_URL};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let token = std::env::var("UPSTOX_ANALYTICS_TOKEN").expect("set UPSTOX_ANALYTICS_TOKEN");
    let client = UpstoxClient::new(token.trim()).expect("build client");
    let today = shared_domain::today_ist();
    let index = "NSE_INDEX|Nifty 50";

    match client.intraday_candles(index, 1).await {
        Ok(c) => println!("intraday 1m {index}: {} candles, first {:?}, last {:?}", c.len(), c.first(), c.last()),
        Err(e) => println!("intraday 1m {index}: {e}"),
    }
    let from = today - chrono::Days::new(7);
    let to = today - chrono::Days::new(1);
    match client.historical_candles(index, 5, from, to).await {
        Ok(c) => println!("historical 5m {from}..{to}: {} candles, first {:?}, last {:?}", c.len(), c.first(), c.last()),
        Err(e) => println!("historical 5m: {e}"),
    }

    let derivatives = client
        .index_derivatives(NSE_INSTRUMENTS_URL, &["NIFTY", "BANKNIFTY", "FINNIFTY", "MIDCPNIFTY"])
        .await
        .expect("NSE instruments");
    let nearest = derivatives
        .iter()
        .filter(|d| d.underlying == "NIFTY" && d.kind == "CE" && d.expiry >= today)
        .map(|d| d.expiry)
        .min()
        .expect("a live NIFTY expiry");
    let future = derivatives
        .iter()
        .filter(|d| d.underlying == "NIFTY" && d.kind == "FUT" && d.expiry >= today)
        .min_by_key(|d| d.expiry)
        .expect("a live NIFTY future");
    println!(
        "NSE index derivatives: {} (nearest NIFTY expiry {nearest}, future {} expiring {})",
        derivatives.len(),
        future.instrument_key,
        future.expiry
    );

    let keys: BTreeSet<String> = [index.to_string(), future.instrument_key.clone()].into();
    let (_keys_tx, keys_rx) = tokio::sync::watch::channel(keys);
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(256);
    let feed = tokio::spawn(async move { client.stream_ltps(keys_rx, out_tx).await });
    let deadline = tokio::time::sleep(Duration::from_secs(8));
    tokio::pin!(deadline);
    let mut seen = 0;
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            tick = out_rx.recv() => match tick {
                Some(t) => {
                    seen += 1;
                    if seen <= 6 {
                        println!("feed: {} {} (ltt {})", t.key, t.ltp, t.ltt_ms);
                    }
                }
                None => break,
            },
        }
    }
    println!("feed: {seen} prices in 8 s");
    feed.abort();
    if let Ok(Ok(Err(e))) = tokio::time::timeout(Duration::from_millis(10), feed).await.map(Ok::<_, ()>) {
        println!("feed ended: {e}");
    }
}
