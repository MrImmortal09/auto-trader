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

/// Kotak Neo statutory fee calculator for paper trades.
///
/// # Fee schedule (NSE)
/// | Charge              | Rule                                   |
/// |---------------------|----------------------------------------|
/// | Brokerage           | Flat per leg from `TradingConfig`      |
/// | SEBI fee            | 0.0001 % of turnover                   |
/// | Exchange fee (NSE)  | 0.00297 % of turnover                  |
/// | STT (options)       | 0.05 % on SELL side only               |
/// | STT (equity intra.) | 0.025 % on SELL side only              |
/// | Stamp duty          | 0.003 % on BUY side only               |
/// | GST                 | 18 % × (brokerage + SEBI + exchange)   |
pub struct FeeCalculator;

impl FeeCalculator {
    pub fn calculate(
        qty: i32,
        price: f64,
        action: &str,
        is_options: bool,
        brokerage_flat: f64,
    ) -> ChargeBreakdown {
        let gross_value = qty as f64 * price;
        let is_buy = action.eq_ignore_ascii_case("BUY");

        let brokerage         = brokerage_flat;
        let sebi_fee          = gross_value * 0.000_001;
        let transaction_charge = gross_value * 0.000_029_7;

        let stt_charge = if is_buy {
            0.0
        } else if is_options {
            gross_value * 0.000_5
        } else {
            gross_value * 0.000_25
        };

        let stamp_duty = if is_buy { gross_value * 0.000_03 } else { 0.0 };
        let gst = (brokerage + sebi_fee + transaction_charge) * 0.18;
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
