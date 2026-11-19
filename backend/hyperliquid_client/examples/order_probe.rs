//! Signs the order shapes LIVE sends — an IOC, a line order (a market
//! trigger with its stop and target as `normalTpsl` children), a reduce-only
//! stop, a cancel and a leverage change — with a throwaway key, and posts
//! them to Hyperliquid's testnet. The key belongs to no account, so each is
//! refused for its wallet after Hyperliquid has read the action: an answer
//! naming the wallet (not a parse error) means the shape is right. Trades
//! nothing. `cargo run -p hyperliquid_client --example order_probe`.

use hyperliquid_client::exchange::{ExchangeClient, Nonces};
use hyperliquid_client::signing::{CancelAction, CancelWire, Grouping, LeverageAction, OrderAction, OrderTypeWire, OrderWire, Tif, Wallet};
use hyperliquid_client::Network;

fn order(a: u32, b: bool, p: &str, s: &str, r: bool, t: OrderTypeWire) -> OrderWire {
    OrderWire { a, b, p: p.into(), s: s.into(), r, t, c: None }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let wallet = Wallet::from_hex("0x7f3a1c0e9b2d4f6a8c1e3b5d7f9a2c4e6b8d0f1a3c5e7b9d2f4a6c8e0b1d3f5a").expect("a key");
    let ex = ExchangeClient::new(Network::Testnet);
    let nonces = Nonces::default();
    let now = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    // Testnet BTC is asset 3.
    let ioc = OrderAction::new(vec![order(3, true, "62000", "0.001", false, OrderTypeWire::limit(Tif::Ioc))], Grouping::Na);
    let line = OrderAction::new(
        vec![
            order(3, true, "69300", "0.001", false, OrderTypeWire::trigger(true, "63000".into(), "sl")),
            order(3, false, "54000", "0.001", true, OrderTypeWire::trigger(true, "60000".into(), "sl")),
            order(3, false, "58500", "0.001", true, OrderTypeWire::trigger(true, "65000".into(), "tp")),
        ],
        Grouping::NormalTpsl,
    );
    let stop = OrderAction::new(vec![order(3, false, "54000", "0.001", true, OrderTypeWire::trigger(true, "60000".into(), "sl"))], Grouping::Na);
    let cancel = CancelAction::new(vec![CancelWire { a: 3, o: 1 }]);
    let leverage = LeverageAction::isolated(3, 5);
    println!("signed as {}", wallet.address());
    println!("ioc:      {:?}", ex.post(&wallet, &ioc, nonces.next(now())).await);
    println!("line:     {:?}", ex.post(&wallet, &line, nonces.next(now())).await);
    println!("stop:     {:?}", ex.post(&wallet, &stop, nonces.next(now())).await);
    println!("cancel:   {:?}", ex.post(&wallet, &cancel, nonces.next(now())).await);
    println!("leverage: {:?}", ex.post(&wallet, &leverage, nonces.next(now())).await);
}
