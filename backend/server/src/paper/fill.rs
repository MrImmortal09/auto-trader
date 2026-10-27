//! A paper fill's money: Kotak's statutory charges (`trading_engine::fees`)
//! with the users' own flat brokerage, and the average price after an add.
//! Pure — no database, no clock.

use trading_engine::{ChargeBreakdown, Exchange, FeeCalculator};

/// Brokerage on every user leg. Users don't depend on the owner's config.
pub const PAPER_BROKERAGE_PER_ORDER: f64 = 20.0;

/// The charges on buying `qty` of an `index` option at `price`: `net_value`
/// is what the wallet pays.
pub fn buy_fill(index: &str, qty: i32, price: f64) -> ChargeBreakdown {
    FeeCalculator::calculate_on(Exchange::for_underlying(index), qty, price, "BUY", true, PAPER_BROKERAGE_PER_ORDER)
}

/// The charges on selling `qty` of an `index` option at `price`: `net_value`
/// is what the wallet gets.
pub fn sell_fill(index: &str, qty: i32, price: f64) -> ChargeBreakdown {
    FeeCalculator::calculate_on(Exchange::for_underlying(index), qty, price, "SELL", true, PAPER_BROKERAGE_PER_ORDER)
}

/// The average price of `held` at `avg` plus `added` at `price`.
pub fn merge_avg(held: i32, avg: f64, added: i32, price: f64) -> f64 {
    let total = held + added;
    if total <= 0 {
        return 0.0;
    }
    (f64::from(held) * avg + f64::from(added) * price) / f64::from(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_add_moves_the_average_by_quantity() {
        assert_eq!(merge_avg(0, 0.0, 75, 120.0), 120.0);
        assert_eq!(merge_avg(75, 100.0, 75, 120.0), 110.0);
        assert!((merge_avg(65, 100.0, 130, 130.0) - 120.0).abs() < 1e-9);
    }

    #[test]
    fn a_buy_costs_more_than_its_premium_and_a_sell_brings_less() {
        let buy = buy_fill("NIFTY", 75, 100.0);
        assert_eq!(buy.gross_value, 7_500.0);
        assert_eq!(buy.brokerage, PAPER_BROKERAGE_PER_ORDER);
        assert_eq!(buy.stt_charge, 0.0);
        assert!(buy.net_value > buy.gross_value);
        let sell = sell_fill("NIFTY", 75, 100.0);
        assert!(sell.stt_charge > 0.0);
        assert!(sell.net_value < sell.gross_value);
        // SENSEX trades on BSE, at BSE's exchange charge.
        assert!(sell_fill("SENSEX", 75, 100.0).transaction_charge < sell.transaction_charge);
    }
}
