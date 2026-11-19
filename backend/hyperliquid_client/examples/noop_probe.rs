//! Signs a `noop` with a throwaway key and posts it to Hyperliquid's
//! testnet: the answer names the address Hyperliquid recovered from the
//! signature, which must be the key's own. Trades nothing — the key belongs
//! to no account. `cargo run -p hyperliquid_client --example noop_probe`.

use hyperliquid_client::exchange::{ExchangeClient, Nonces};
use hyperliquid_client::signing::Wallet;
use hyperliquid_client::Network;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let wallet = Wallet::from_hex("0x7f3a1c0e9b2d4f6a8c1e3b5d7f9a2c4e6b8d0f1a3c5e7b9d2f4a6c8e0b1d3f5a").expect("a key");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    let nonce = Nonces::default().next(now);
    let answer = ExchangeClient::new(Network::Testnet).noop(&wallet, nonce).await;
    println!("signed as {}", wallet.address());
    println!("testnet answered {answer:?}");
}
