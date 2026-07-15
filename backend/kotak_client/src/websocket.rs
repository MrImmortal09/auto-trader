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
    for s in initial_scrips.split(',') {
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

    loop {
        // If child is dead or not started, start it
        if current_stdin.is_none() {
            if !shared_domain::is_market_open() {
                info!("Market is closed. Pausing WebSocket reconnection for 5 minutes...");
                // Sleep while still processing rx to buffer subscriptions
                let sleep = tokio::time::sleep(std::time::Duration::from_secs(300));
                tokio::pin!(sleep);
                tokio::select! {
                    _ = &mut sleep => {}
                    msg_opt = rx.recv() => {
                        if let Some(msg) = msg_opt {
                            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&msg) {
                                if let Some(s) = parsed.get("scrips").and_then(|v| v.as_str()) {
                                    active_scrips.insert(s.to_string());
                                }
                            }
                        } else {
                            return; // rx closed
                        }
                    }
                }
                continue;
            }

            let scrips_str = active_scrips.iter().cloned().collect::<Vec<_>>().join(",");
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
                                active_scrips.insert(s.to_string());
                            }
                        }
                        if let Some(stdin) = current_stdin.as_mut() {
                            let mut payload = msg.clone();
                            if !payload.ends_with('\n') { payload.push('\n'); }
                            if let Err(e) = stdin.write_all(payload.as_bytes()).await {
                                error!("Failed to write to Node bridge stdin: {}", e);
                                current_stdin = None; // Force restart
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
                tokio::time::sleep(std::time::Duration::from_secs(5)).await; // Backoff before restart
            }
        }
    }
}
