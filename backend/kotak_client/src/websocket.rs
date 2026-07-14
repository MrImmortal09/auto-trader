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
    scrips: String,
    _channel_num: u32,
    prices: Arc<dashmap::DashMap<String, f64>>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<String>,
) {
    info!("Starting Node.js bridge for Kotak WebSocket with scrips: {}", scrips);

    let Some(bridge_dir) = resolve_bridge_dir() else {
        error!("Failed to locate kotak-bridge directory from the current runtime paths.");
        return;
    };

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
            error!("Failed to start Node.js bridge. Is Node.js installed? {}", e);
            return;
        }
    };

    let mut stdin = child.stdin.take().expect("Failed to open stdin");
    let stdout = child.stdout.take().expect("Failed to open stdout");

    // Send connection payload
    let connect_payload = serde_json::json!({
        "action": "connect",
        "auth": auth_token,
        "sid": sid,
        "scrips": scrips
    });
    let mut connect_str = connect_payload.to_string();
    connect_str.push('\n');

    if let Err(e) = stdin.write_all(connect_str.as_bytes()).await {
        error!("Failed to write connect payload to Node bridge: {}", e);
        return;
    }

    // Spawn a task to forward dynamic subscription messages to the Node bridge
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let mut payload = msg.clone();
            if !payload.ends_with('\n') {
                payload.push('\n');
            }
            if let Err(e) = stdin.write_all(payload.as_bytes()).await {
                error!("Failed to write dynamic message to Node bridge stdin: {}", e);
                break;
            }
        }
    });

    let mut reader = BufReader::new(stdout).lines();

    loop {
        match reader.next_line().await {
            Ok(Some(line)) => {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&line) {
                    if parsed["event"] == "data" {
                        if let Some(arr) = parsed["data"].as_array() {
                            for item in arr {
                                if let (Some(tk), Some(e)) = (
                                    item["tk"].as_str(),
                                    item["e"].as_str(),
                                ) {
                                    if let Some(ltp) = item["ltp"].as_f64().or_else(|| item["ltp"].as_str().and_then(|s| s.parse::<f64>().ok())) {
                                        let key = format!("{}|{}", e.to_ascii_lowercase(), tk.trim());
                                        prices.insert(key, ltp);
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
            Ok(None) => {
                warn!("Node bridge stdout closed unexpectedly.");
                break;
            }
            Err(e) => {
                error!("Error reading from Node bridge stdout: {}", e);
                break;
            }
        }
    }

    let _ = child.kill().await;
    info!("Node.js bridge terminated.");
}
