//! Statutory charges on one order leg, used to price paper trades and to
//! record the charges on live fills.
//!
//! | Charge                | Rule                                                   |
//! |-----------------------|--------------------------------------------------------|
//! | Brokerage             | Flat per leg from `TradingConfig`                      |
//! | STT (options)         | 0.15 % of premium, SELL side only (from 1 April 2026)  |
//! | STT (equity intraday) | 0.025 % of turnover, SELL side only                    |
//! | Exchange charge       | Options: 0.03503 % of premium (NSE), 0.0325 % (BSE)    |
//! |                       | Equity: 0.00297 % (NSE), 0.00375 % (BSE)               |
//! | SEBI fee              | 0.0001 % of turnover (₹10 per crore)                   |
//! | Stamp duty            | 0.003 % of turnover, BUY side only                     |
//! | GST                   | 18 % × (brokerage + SEBI fee + exchange charge)        |
//!
//! Not modelled: STT on an option held to expiry and exercised (0.15 % of
//! intrinsic value; the engine always exits by selling), NSE's IPFT levy, and
//! the rounding to whole rupees that contract notes apply to STT.

/// STT on the sale of an option, charged on premium (Finance Act 2026, from 1 April 2026).
pub const STT_OPTIONS_SELL: f64 = 0.0015;
/// STT on an equity intraday sale.
pub const STT_EQUITY_INTRADAY_SELL: f64 = 0.00025;
/// Exchange transaction charge on option premium, both sides (since 1 October 2024).
pub const TXN_OPTIONS_NSE: f64 = 0.0003503;
/// BSE transaction charge on SENSEX / BANKEX option premium, both sides.
pub const TXN_OPTIONS_BSE: f64 = 0.000325;
/// Exchange transaction charge on equity turnover, both sides.
pub const TXN_EQUITY_NSE: f64 = 0.0000297;
/// BSE transaction charge on equity turnover (group A/B), both sides.
pub const TXN_EQUITY_BSE: f64 = 0.0000375;
/// SEBI turnover fee: ₹10 per crore.
pub const SEBI_FEE: f64 = 0.000001;
/// Stamp duty on the buy side.
pub const STAMP_DUTY_BUY: f64 = 0.00003;
/// GST on brokerage, SEBI fee and exchange charge.
pub const GST_RATE: f64 = 0.18;

/// Breakdown of all statutory charges for one order leg.
#[derive(Debug, Clone)]
pub struct ChargeBreakdown {
    pub gross_value: f64,
    pub brokerage: f64,
    pub stt_charge: f64,
    pub sebi_fee: f64,
    pub stamp_duty: f64,
    pub transaction_charge: f64,
    pub gst: f64,
    /// Total outflow (BUY) or net inflow (SELL) after all charges.
    pub net_value: f64,
}

/// Exchange an order leg trades on. SENSEX and BANKEX options trade on BSE;
/// NIFTY, BANKNIFTY, FINNIFTY and MIDCPNIFTY on NSE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exchange {
    Nse,
    Bse,
}

impl Exchange {
    /// Exchange for an underlying index name such as `"SENSEX"` or `"NIFTY"`
    /// (case-insensitive, surrounding spaces ignored).
    pub fn for_underlying(underlying: &str) -> Self {
        match underlying.trim().to_ascii_uppercase().as_str() {
            "SENSEX" | "BANKEX" => Exchange::Bse,
            _ => Exchange::Nse,
        }
    }
}

/// Kotak Neo statutory fee calculator.
pub struct FeeCalculator;

impl FeeCalculator {
    /// Charges for one leg at NSE rates. Kept so existing callers compile
    /// unchanged; SENSEX and BANKEX legs should move to
    /// [`FeeCalculator::calculate_on`] with [`Exchange::for_underlying`].
    pub fn calculate(
        qty: i32,
        price: f64,
        action: &str,
        is_options: bool,
        brokerage_flat: f64,
    ) -> ChargeBreakdown {
        Self::calculate_on(Exchange::Nse, qty, price, action, is_options, brokerage_flat)
    }

    /// Charges for one leg on `exchange`. For options `price` is the premium,
    /// so every percentage applies to premium turnover.
    pub fn calculate_on(
        exchange: Exchange,
        qty: i32,
        price: f64,
        action: &str,
        is_options: bool,
        brokerage_flat: f64,
    ) -> ChargeBreakdown {
        let gross_value = qty as f64 * price;
        let is_buy = action.eq_ignore_ascii_case("BUY");

        let brokerage = brokerage_flat;
        let sebi_fee = gross_value * SEBI_FEE;
        let txn_rate = match (is_options, exchange) {
            (true, Exchange::Nse) => TXN_OPTIONS_NSE,
            (true, Exchange::Bse) => TXN_OPTIONS_BSE,
            (false, Exchange::Nse) => TXN_EQUITY_NSE,
            (false, Exchange::Bse) => TXN_EQUITY_BSE,
        };
        let transaction_charge = gross_value * txn_rate;

        let stt_charge = if is_buy {
            0.0
        } else if is_options {
            gross_value * STT_OPTIONS_SELL
        } else {
            gross_value * STT_EQUITY_INTRADAY_SELL
        };

        let stamp_duty = if is_buy { gross_value * STAMP_DUTY_BUY } else { 0.0 };
        let gst = (brokerage + sebi_fee + transaction_charge) * GST_RATE;
        let total = brokerage + stt_charge + sebi_fee + stamp_duty + transaction_charge + gst;

        let net_value = if is_buy {
            gross_value + total
        } else {
            gross_value - total
        };

        ChargeBreakdown {
            gross_value, brokerage, stt_charge, sebi_fee,
            stamp_duty, transaction_charge, gst, net_value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    // One NIFTY lot (65) at ₹150 premium: ₹9,750 premium turnover.
    const QTY: i32 = 65;
    const PREMIUM: f64 = 150.0;

    #[test]
    fn option_sell_pays_stt_on_premium_at_0_15_percent() {
        let c = FeeCalculator::calculate(QTY, PREMIUM, "SELL", true, 20.0);
        assert!(close(c.gross_value, 9_750.0));
        assert!(close(c.stt_charge, 14.625));
        assert!(close(c.stamp_duty, 0.0));
    }

    #[test]
    fn option_buy_pays_stamp_duty_but_no_stt() {
        let c = FeeCalculator::calculate(QTY, PREMIUM, "buy", true, 20.0);
        assert!(close(c.stt_charge, 0.0));
        assert!(close(c.stamp_duty, 0.2925));
    }

    #[test]
    fn option_exchange_charge_depends_on_exchange() {
        let nse = FeeCalculator::calculate_on(Exchange::Nse, QTY, PREMIUM, "BUY", true, 20.0);
        let bse = FeeCalculator::calculate_on(Exchange::Bse, QTY, PREMIUM, "BUY", true, 20.0);
        assert!(close(nse.transaction_charge, 3.415425));
        assert!(close(bse.transaction_charge, 3.16875));
    }

    #[test]
    fn gst_covers_brokerage_sebi_fee_and_exchange_charge() {
        let c = FeeCalculator::calculate(QTY, PREMIUM, "SELL", true, 20.0);
        assert!(close(c.gst, (20.0 + c.sebi_fee + c.transaction_charge) * 0.18));
    }

    #[test]
    fn sell_net_value_is_turnover_minus_every_charge() {
        let c = FeeCalculator::calculate(QTY, PREMIUM, "SELL", true, 20.0);
        // 9,750 − (20 + 14.625 + 0.00975 + 3.415425 + 4.2165315)
        assert!(close(c.net_value, 9_707.733_293_5));
    }

    #[test]
    fn buy_net_value_is_turnover_plus_every_charge() {
        let c = FeeCalculator::calculate(QTY, PREMIUM, "BUY", true, 20.0);
        let total = c.brokerage + c.stt_charge + c.sebi_fee + c.stamp_duty + c.transaction_charge + c.gst;
        assert!(close(c.net_value, c.gross_value + total));
    }

    #[test]
    fn calculate_uses_nse_rates() {
        let a = FeeCalculator::calculate(QTY, PREMIUM, "SELL", true, 20.0);
        let b = FeeCalculator::calculate_on(Exchange::Nse, QTY, PREMIUM, "SELL", true, 20.0);
        assert!(close(a.net_value, b.net_value) && close(a.transaction_charge, b.transaction_charge));
    }

    #[test]
    fn bse_indices_map_to_bse() {
        assert_eq!(Exchange::for_underlying("SENSEX"), Exchange::Bse);
        assert_eq!(Exchange::for_underlying(" bankex "), Exchange::Bse);
        for nse in ["NIFTY", "BANKNIFTY", "FINNIFTY", "MIDCPNIFTY"] {
            assert_eq!(Exchange::for_underlying(nse), Exchange::Nse);
        }
    }

    #[test]
    fn equity_intraday_rates_are_unchanged() {
        let c = FeeCalculator::calculate(100, 2_500.0, "SELL", false, 20.0);
        assert!(close(c.stt_charge, 62.5));
        assert!(close(c.transaction_charge, 7.425));
    }
}
