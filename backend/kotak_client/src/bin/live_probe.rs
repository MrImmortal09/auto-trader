//! Standalone diagnostic: log in to Kotak directly (bypassing the whole
//! trading engine / server) and call get_positions / get_order_book /
//! get_limits, printing exactly what comes back.
//!
//! Run from the repo root: `cargo run -p kotak_client --bin live_probe`
//!
//! Reads API_ACCESS_TOKEN / NUMBER / CLIENT_UCC / PIN from .env at the repo
//! root; prompts for the TOTP (can't be read from a file — it's time-based).
//! Saves the resulting session to /tmp/kotak_session.json and reuses it on
//! the next run instead of logging in again, until you delete that file.

use std::collections::HashMap;
use std::io::{self, Write};

use kotak_client::{KotakClient, KotakCredentials};

const SESSION_FILE: &str = "/tmp/kotak_session.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedSession {
    access_token: String,
    auth_token: String,
    sid: String,
    base_url: String,
}

fn read_env_file() -> HashMap<String, String> {
    let candidates = [
        ".env",
        "../.env",
        "/Users/oms/Coding/auto-trader/.env",
    ];
    for path in candidates {
        if let Ok(content) = std::fs::read_to_string(path) {
            println!("Loaded env from {path}");
            let mut map = HashMap::new();
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = line.split_once('=') {
                    map.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
            return map;
        }
    }
    println!("No .env found in any candidate path — will prompt for everything.");
    HashMap::new()
}

fn prompt(label: &str) -> String {
    print!("{label}: ");
    io::stdout().flush().ok();
    let mut s = String::new();
    io::stdin().read_line(&mut s).expect("stdin read failed");
    s.trim().to_string()
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let env = read_env_file();
    let access_token = env
        .get("API_ACCESS_TOKEN")
        .cloned()
        .unwrap_or_else(|| prompt("API_ACCESS_TOKEN"));

    let mut client = KotakClient::new(&access_token).expect("failed to build KotakClient");

    let mut reused = false;
    if let Ok(raw) = std::fs::read_to_string(SESSION_FILE) {
        if let Ok(saved) = serde_json::from_str::<SavedSession>(&raw) {
            if saved.access_token == access_token {
                println!("Found saved session in {SESSION_FILE} — reusing it (no fresh login).");
                println!("Delete that file first if you want a fresh login instead.");
                client.restore_session(saved.auth_token, saved.sid, saved.base_url);
                reused = true;
            }
        }
    }

    if !reused {
        let mobile = env
            .get("NUMBER")
            .cloned()
            .unwrap_or_else(|| prompt("Mobile number (+91XXXXXXXXXX)"));
        let ucc = env.get("CLIENT_UCC").cloned().unwrap_or_else(|| prompt("UCC"));
        let mpin = env.get("PIN").cloned().unwrap_or_else(|| prompt("MPIN"));
        let totp = prompt("Current 6-digit TOTP from your authenticator app");

        let creds = KotakCredentials {
            access_token: access_token.clone(),
            mobile_number: mobile,
            ucc,
            totp,
            mpin,
        };

        println!("\nLogging in...");
        if let Err(e) = client.login(creds).await {
            eprintln!("LOGIN FAILED: {e}");
            std::process::exit(1);
        }
        println!("Login OK.");

        if let Some(session) = &client.session {
            let saved = SavedSession {
                access_token: access_token.clone(),
                auth_token: session.auth_token.clone(),
                sid: session.sid.clone(),
                base_url: session.base_url.clone(),
            };
            if let Ok(json) = serde_json::to_string_pretty(&saved) {
                if std::fs::write(SESSION_FILE, json).is_ok() {
                    println!("Session saved to {SESSION_FILE} for reuse on the next run.");
                }
            }
        }
    }

    if let Some(s) = &client.session {
        println!("\n--- Session ---");
        println!("base_url:   {}", s.base_url);
        println!("sid:        {}", s.sid);
        println!("auth_token: {}...", &s.auth_token[..s.auth_token.len().min(24)]);
    }

    println!("\n--- get_positions() ---");
    match client.get_positions().await {
        Ok(positions) => {
            println!("OK — {} row(s)", positions.len());
            for p in &positions {
                println!("  {p:?}");
            }
        }
        Err(e) => println!("ERROR: {e}"),
    }

    println!("\n--- get_order_book() ---");
    match client.get_order_book().await {
        Ok(orders) => {
            println!("OK — {} row(s)", orders.len());
            for o in &orders {
                println!("  {o:?}");
            }
        }
        Err(e) => println!("ERROR: {e}"),
    }

    println!("\n--- get_limits() ---");
    match client.get_limits().await {
        Ok(limits) => println!("OK — {limits:?}"),
        Err(e) => println!("ERROR: {e}"),
    }
}
