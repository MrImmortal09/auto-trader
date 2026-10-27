//! Sends the sign-in code over WhatsApp through the owner's self-hosted
//! server (Whatsmeow or OpenWA — the same REST shape): `POST
//! {WA_SERVER_URL}/api/sendText` with the `key` header and
//! `{chatId, text}`. Both `WA_SERVER_URL` and `WA_API_KEY` come from the
//! environment; without them nobody can sign in, and the request route says so.

use std::time::Duration;

use serde_json::json;

/// How an OTP leaves the server.
pub enum Delivery {
    Sender { http: reqwest::Client, base: String, key: String },
    /// Debug builds with `OTP_DEV_LOG=1` and no WhatsApp server: the code is
    /// written to the server log so the flow can be walked locally.
    DevLog,
    Unconfigured,
}

impl Delivery {
    pub fn from_env() -> Self {
        let env = |name: &str| std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        match (env("WA_SERVER_URL"), env("WA_API_KEY")) {
            (Some(base), Some(key)) => {
                let http = reqwest::Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()
                    .expect("reqwest client with a timeout");
                Delivery::Sender { http, base: base.trim_end_matches('/').to_string(), key }
            }
            _ if cfg!(debug_assertions) && env("OTP_DEV_LOG").as_deref() == Some("1") => Delivery::DevLog,
            _ => Delivery::Unconfigured,
        }
    }

    pub fn configured(&self) -> bool {
        !matches!(self, Delivery::Unconfigured)
    }

    /// For the startup log.
    pub fn describe(&self) -> &'static str {
        match self {
            Delivery::Sender { .. } => "WhatsApp server (WA_SERVER_URL)",
            Delivery::DevLog => "server log only (OTP_DEV_LOG, debug build)",
            Delivery::Unconfigured => "not configured — phone sign-in is off",
        }
    }

    /// Deliver `code` to `phone` (10 digits, Indian). The error is for the
    /// log, not the caller: the route answers with its own wording.
    pub async fn send_otp(&self, phone: &str, code: &str) -> Result<(), String> {
        match self {
            Delivery::Unconfigured => Err("WA_SERVER_URL / WA_API_KEY not set".to_string()),
            Delivery::DevLog => {
                tracing::warn!(phone, code, "OTP_DEV_LOG: no WhatsApp server configured — code logged for local testing");
                Ok(())
            }
            Delivery::Sender { http, base, key } => {
                let response = http
                    .post(format!("{base}/api/sendText"))
                    .header("Content-Type", "application/json")
                    .header("key", key)
                    .json(&json!({ "chatId": chat_id(phone), "text": message(code) }))
                    .send()
                    .await
                    .map_err(|e| format!("could not reach the WhatsApp server: {e}"))?;
                let status = response.status();
                if !status.is_success() {
                    let body = response.text().await.unwrap_or_default();
                    let body: String = body.chars().take(300).collect();
                    return Err(format!("WhatsApp server answered {status}: {body}"));
                }
                Ok(())
            }
        }
    }
}

/// Whatsmeow's chat id for an Indian mobile.
pub fn chat_id(phone: &str) -> String {
    format!("91{phone}@s.whatsapp.net")
}

/// The owner's message text.
pub fn message(code: &str) -> String {
    format!("Your OTP is: *{code}*\nValid for 10 minutes. Do not share with anyone.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_and_chat_id_match_the_owners_snippet() {
        assert_eq!(chat_id("7000000001"), "917000000001@s.whatsapp.net");
        assert_eq!(message("123456"), "Your OTP is: *123456*\nValid for 10 minutes. Do not share with anyone.");
    }
}
