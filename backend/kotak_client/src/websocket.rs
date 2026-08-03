use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tracing::{error, info, warn};

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

pub async fn start_market_data_stream(
    auth_token: String,
    sid: String,
    initial_scrips: String,
    _channel_num: u32,
    prices: Arc<dashmap::DashMap<String, f64>>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<String>,
) {
    let mut active_scrips = std::collections::HashSet::new();
    for s in initial_scrips.split(|c| c == ',' || c == '&') {
        let s = s.trim();
        if !s.is_empty() {
            active_scrips.insert(s.to_string());
        }
    }

    let Some(bridge_dir) = resolve_bridge_dir() else {
        error!("Failed to locate kotak-bridge directory from the current runtime paths.");
        return;
    };

    let mut current_stdin: Option<tokio::process::ChildStdin> = None;
    let mut child_opt: Option<tokio::process::Child> = None;
    // Deadline for the hard 15:30:00 IST cutoff, set once a connection is live.
    let mut close_deadline: Option<tokio::time::Instant> = None;

    loop {
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
                            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&msg) {
                                if let Some(s) = parsed.get("scrips").and_then(|v| v.as_str()) {
                                    for item in s.split(|c| c == ',' || c == '&') {
                                        let item = item.trim();
                                        if !item.is_empty() {
                                            active_scrips.insert(item.to_string());
                                        }
                                    }
                                }
                            }
                        } else {
                            return; // rx closed
                        }
                    }
                }
                continue;
            }

            let scrips_str = active_scrips.iter().cloned().collect::<Vec<_>>().join("&");
            info!("Starting Node.js bridge for Kotak WebSocket with scrips: {}", scrips_str);

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
                "auth": auth_token,
                "sid": sid,
                "scrips": scrips_str
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

            // Arm the hard 15:30:00 IST cutoff for this connection.
            close_deadline = shared_domain::duration_until_market_close()
                .map(|d| tokio::time::Instant::now() + d);

            // Spawn reader for stdout
            let prices_clone = Arc::clone(&prices);
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&line) {
                        if parsed["event"] == "data" {
                            if let Some(arr) = parsed["data"].as_array() {
                                for item in arr {
                                    if let (Some(tk), Some(e)) = (item["tk"].as_str(), item["e"].as_str()) {
                                        if let Some(ltp) = item["ltp"].as_f64().or_else(|| item["ltp"].as_str().and_then(|s| s.parse::<f64>().ok())) {
                                            prices_clone.insert(format!("{}|{}", e.to_ascii_lowercase(), tk.trim()), ltp);
                                        }
                                    }
                                }
                            }
                        } else if parsed["event"] == "closed" {
                            warn!("Node bridge reported WebSocket closed.");
                            break;
                        } else if parsed["event"] == "error" {
                            error!("Node bridge reported WebSocket error.");
                        }
                    }
                }
                warn!("Node bridge stdout closed.");
            });

            child_opt = Some(child);
        }

        // Wait for next message or child exit
        tokio::select! {
            msg_opt = rx.recv() => {
                match msg_opt {
                    Some(msg) => {
                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&msg) {
                            if let Some(s) = parsed.get("scrips").and_then(|v| v.as_str()) {
                                for item in s.split(|c| c == ',' || c == '&') {
                                    let item = item.trim();
                                    if !item.is_empty() {
                                        active_scrips.insert(item.to_string());
                                    }
                                }
                            }
                        }
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
                warn!("Market close (15:30:00 IST) reached — closing WebSocket sharply and clearing LTPs.");
                if let Some(mut child) = child_opt.take() { let _ = child.kill().await; }
                current_stdin = None;
                close_deadline = None;
                for scrip in active_scrips.iter() {
                    prices.remove(scrip);
                }
            }
        }
    }
}
