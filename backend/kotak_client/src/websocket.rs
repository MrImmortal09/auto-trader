use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::broadcast;
use shared_domain::telemetry;
use tracing::{error, info, warn};

/// One price as the bridge decoded it, stamped when this server read it.
#[derive(Debug, Clone)]
pub struct PriceTick {
    /// Price-map key: `nse_fo|47158`, or `idx|NIFTY 50` for an index.
    pub key: String,
    pub ltp: f64,
    /// Unix ms it was read from the bridge.
    pub at_ms: i64,
}

/// Room for a burst of ticks while a listener catches up.
const TICK_TAP_CAPACITY: usize = 4096;

static TICK_TAP: LazyLock<broadcast::Sender<PriceTick>> =
    LazyLock::new(|| broadcast::channel(TICK_TAP_CAPACITY).0);

/// Every price the bridge decodes from now on, for the v2 candle service.
/// The feed never waits on a listener: with none it skips the copy, and one
/// that falls behind misses ticks (`RecvError::Lagged`), never the feed.
pub fn subscribe_ticks() -> broadcast::Receiver<PriceTick> {
    TICK_TAP.subscribe()
}

/// Copy one decoded price to the tap — only while something listens, so with
/// no v2 client open the feed does exactly what it did before the tap.
fn publish_tick(key: &str, ltp: f64) {
    if TICK_TAP.receiver_count() > 0 {
        let at_ms = shared_domain::now_ist().timestamp_millis();
        let _ = TICK_TAP.send(PriceTick { key: key.to_string(), ltp, at_ms });
    }
}

fn resolve_bridge_dir() -> Option<PathBuf> {
    let mut candidates = Vec::new();

    if let Ok(current_dir) = std::env::current_dir() {
        candidates.push(current_dir.join("kotak-bridge"));
        candidates.push(current_dir.join("../kotak-bridge"));
    }

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    candidates.push(manifest_dir.join("../../kotak-bridge"));

    candidates.into_iter().find(|path| path.is_dir())
}

/// What the bridge has been asked to stream, so a restarted bridge replays it.
#[derive(Default)]
struct Subscribed {
    /// Positions' contracts — kept for as long as this stream runs.
    scrips: std::collections::HashSet<String>,
    /// The Execute screen's option ladders (`"ladder": true`) — dropped at
    /// each day's close, so they never pile up across days.
    ladder: std::collections::HashSet<String>,
    /// Index spot values by name (`nse_cm|Nifty 50`).
    indices: std::collections::HashSet<String>,
}

impl Subscribed {
    fn len(&self) -> usize {
        self.scrips.union(&self.ladder).count() + self.indices.len()
    }

    /// Every scrip to stream: positions' and the ladders'.
    fn all_scrips(&self) -> Vec<String> {
        self.scrips.union(&self.ladder).cloned().collect()
    }
}

/// Record what a control message for the bridge subscribes to — scrips
/// (`{"scrips": "nse_fo|1&nse_fo|2"}`, a ladder's with `"ladder": true`) or,
/// for `subscribe_indices`, index names (`{"indices": "nse_cm|Nifty 50"}`).
fn remember_subscriptions(msg: &str, subs: &mut Subscribed) {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(msg) else { return };
    let (field, set) = match parsed.get("action").and_then(|v| v.as_str()) {
        Some("subscribe_indices") => ("indices", &mut subs.indices),
        _ if parsed.get("ladder").and_then(|v| v.as_bool()) == Some(true) => ("scrips", &mut subs.ladder),
        _ => ("scrips", &mut subs.scrips),
    };
    if let Some(s) = parsed.get(field).and_then(|v| v.as_str()) {
        for item in s.split(|c| c == ',' || c == '&') {
            let item = item.trim();
            if !item.is_empty() {
                set.insert(item.to_string());
            }
        }
    }
}

/// Kotak's production market-feed endpoint, used when the login response
/// carried no `feedUrl` of its own.
pub const DEFAULT_FEED_URL: &str = "wss://sfeed.kotaksecurities.com/apifeed";

/// Streams live prices into `prices` until `rx` closes. The feed authenticates
/// with the client code (`ucc`) and the trade session's `sid`.
pub async fn start_market_data_stream(
    feed_url: Option<String>,
    ucc: String,
    sid: String,
    initial_scrips: String,
    prices: Arc<dashmap::DashMap<String, f64>>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<String>,
) {
    let feed_url = feed_url
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_FEED_URL.to_string());

    let mut subs = Subscribed::default();
    for s in initial_scrips.split(|c| c == ',' || c == '&') {
        let s = s.trim();
        if !s.is_empty() {
            subs.scrips.insert(s.to_string());
        }
    }

    let Some(bridge_dir) = resolve_bridge_dir() else {
        error!("Failed to locate kotak-bridge directory from the current runtime paths.");
        return;
    };

    let mut current_stdin: Option<tokio::process::ChildStdin> = None;
    let mut child_opt: Option<tokio::process::Child> = None;
    // Deadline for the hard 15:40:00 IST cutoff, set once a connection is live.
    let mut close_deadline: Option<tokio::time::Instant> = None;

    loop {
        telemetry::FEED_SUBSCRIPTIONS.set(subs.len() as i64);

        // If child is dead or not started, start it
        if current_stdin.is_none() {
            if !shared_domain::is_market_open() {
                let wait = shared_domain::duration_until_market_open();
                info!(
                    secs = wait.as_secs(),
                    "Market is closed. Waiting until 09:15:00 IST sharp before connecting..."
                );
                // Sleep precisely until the next market open, while still
                // processing rx to buffer subscription requests.
                let sleep = tokio::time::sleep(wait);
                tokio::pin!(sleep);
                tokio::select! {
                    _ = &mut sleep => {}
                    msg_opt = rx.recv() => {
                        if let Some(msg) = msg_opt {
                            remember_subscriptions(&msg, &mut subs);
                        } else {
                            return; // rx closed
                        }
                    }
                }
                continue;
            }

            let scrips_str = subs.all_scrips().join("&");
            let indices_str = subs.indices.iter().cloned().collect::<Vec<_>>().join(",");
            info!("Starting Node.js bridge for Kotak WebSocket ({}) with scrips: {}", feed_url, scrips_str);

            let mut child = match Command::new("bash")
                .arg("-lc")
                .arg("node index.js")
                .current_dir(&bridge_dir)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    telemetry::FEED_BRIDGE_RESTARTS.inc();
                    error!("Failed to start Node.js bridge. Retrying in 10s... {}", e);
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                    continue;
                }
            };

            let mut stdin = child.stdin.take().expect("Failed to open stdin");
            let stdout = child.stdout.take().expect("Failed to open stdout");

            // Send connection payload
            let connect_payload = serde_json::json!({
                "action": "connect",
                "url": feed_url,
                "user": ucc,
                "auth": sid,
                "scrips": scrips_str,
                "indices": indices_str
            });
            let mut connect_str = connect_payload.to_string();
            connect_str.push('\n');

            if let Err(e) = stdin.write_all(connect_str.as_bytes()).await {
                error!("Failed to write connect payload to Node bridge: {}", e);
                let _ = child.kill().await;
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                continue;
            }

            current_stdin = Some(stdin);

            // Arm the hard 15:40:00 IST cutoff for this connection.
            close_deadline = shared_domain::duration_until_market_close()
                .map(|d| tokio::time::Instant::now() + d);

            // Spawn reader for stdout
            let prices_clone = Arc::clone(&prices);
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    tracing::info!("KOTAK_WS_RAW: {}", line);
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&line) {
                        if parsed["event"] == "data" {
                            if let Some(arr) = parsed["data"].as_array() {
                                for item in arr {
                                    if let (Some(tk), Some(e)) = (item["tk"].as_str(), item["e"].as_str()) {
                                        if let Some(ltp) = item["ltp"].as_f64().or_else(|| item["ltp"].as_str().and_then(|s| s.parse::<f64>().ok())) {
                                            // Index ticks are keyed by the index's name.
                                            let key = if item["idx"].as_bool() == Some(true) {
                                                if let Some(close) = item["close"].as_f64().filter(|c| *c > 0.0) {
                                                    prices_clone.insert(shared_domain::index_close_key(tk), close);
                                                }
                                                shared_domain::index_price_key(tk)
                                            } else {
                                                format!("{}|{}", e.to_ascii_lowercase(), tk.trim())
                                            };
                                            telemetry::feed_tick(&key);
                                            publish_tick(&key, ltp);
                                            prices_clone.insert(key, ltp);
                                        }
                                    }
                                }
                            }
                        } else if parsed["event"] == "connected" {
                            telemetry::FEED_CONNECTED.set(1);
                        } else if parsed["event"] == "closed" {
                            telemetry::FEED_CONNECTED.set(0);
                            warn!("Node bridge reported WebSocket closed.");
                            break;
                        } else if parsed["event"] == "error" {
                            error!("Node bridge reported WebSocket error.");
                        }
                    }
                }
                telemetry::FEED_CONNECTED.set(0);
                warn!("Node bridge stdout closed.");
            });

            child_opt = Some(child);
        }

        // Wait for next message or child exit
        tokio::select! {
            msg_opt = rx.recv() => {
                match msg_opt {
                    Some(msg) => {
                        remember_subscriptions(&msg, &mut subs);
                        if let Some(stdin) = current_stdin.as_mut() {
                            let mut payload = msg.clone();
                            if !payload.ends_with('\n') { payload.push('\n'); }
                            if let Err(e) = stdin.write_all(payload.as_bytes()).await {
                                error!("Failed to write to Node bridge stdin: {}", e);
                                current_stdin = None; // Force restart
                                close_deadline = None;
                            }
                        }
                    }
                    None => {
                        info!("rx channel closed, exiting WebSocket loop.");
                        if let Some(mut child) = child_opt.take() { let _ = child.kill().await; }
                        return;
                    }
                }
            }
            res = async {
                if let Some(child) = child_opt.as_mut() {
                    child.wait().await
                } else {
                    std::future::pending().await
                }
            } => {
                telemetry::FEED_BRIDGE_RESTARTS.inc();
                telemetry::FEED_CONNECTED.set(0);
                warn!("Node bridge child process exited with {:?}", res);
                current_stdin = None;
                child_opt = None;
                close_deadline = None;
                tokio::time::sleep(std::time::Duration::from_secs(5)).await; // Backoff before restart
            }
            _ = async {
                if let Some(deadline) = close_deadline {
                    tokio::time::sleep_until(deadline).await
                } else {
                    std::future::pending().await
                }
            } => {
                telemetry::FEED_CONNECTED.set(0);
                warn!("Market close (15:40:00 IST) reached — closing WebSocket sharply and clearing LTPs.");
                if let Some(mut child) = child_opt.take() { let _ = child.kill().await; }
                current_stdin = None;
                close_deadline = None;
                for scrip in subs.all_scrips() {
                    prices.remove(&scrip);
                }
                // Ladders are re-requested by the Execute screen tomorrow,
                // around tomorrow's prices.
                subs.ladder.clear();
                for index in subs.indices.iter() {
                    let name = index.split_once('|').map_or(index.as_str(), |(_, n)| n);
                    prices.remove(&shared_domain::index_price_key(name));
                    prices.remove(&shared_domain::index_close_key(name));
                }
            }
        }
    }
}

#[cfg(test)]
mod subscription_tests {
    use super::*;

    #[test]
    fn ladder_positions_and_indices_are_remembered_apart() {
        let mut subs = Subscribed::default();
        remember_subscriptions(r#"{"action":"subscribe","scrips":"nse_fo|1&nse_fo|2"}"#, &mut subs);
        remember_subscriptions(r#"{"action":"subscribe","scrips":"nse_fo|2&nse_fo|3","ladder":true}"#, &mut subs);
        remember_subscriptions(r#"{"action":"subscribe_indices","indices":"nse_cm|Nifty 50,bse_cm|SENSEX"}"#, &mut subs);

        let mut all = subs.all_scrips();
        all.sort();
        assert_eq!(all, ["nse_fo|1", "nse_fo|2", "nse_fo|3"]);
        assert_eq!(subs.len(), 5);
        assert!(subs.indices.contains("nse_cm|Nifty 50"));

        // The day's close drops only the ladder: a position's contract stays,
        // even one the ladder had asked for too.
        subs.ladder.clear();
        let mut all = subs.all_scrips();
        all.sort();
        assert_eq!(all, ["nse_fo|1", "nse_fo|2"]);
    }

    #[test]
    fn a_tick_reaches_whoever_listens() {
        let mut rx = subscribe_ticks();
        publish_tick("nse_fo|51342", 159.55);
        let tick = rx.try_recv().expect("the listener gets the tick");
        assert_eq!((tick.key.as_str(), tick.ltp), ("nse_fo|51342", 159.55));
        assert!(tick.at_ms > 0);
    }
}
