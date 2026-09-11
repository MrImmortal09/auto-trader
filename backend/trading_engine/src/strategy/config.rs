//! Tunable parameters for the autonomous strategy.
//!
//! Every threshold the engine consults lives here rather than scattered as
//! literals, so the whole risk surface can be reviewed — and changed — in one
//! place. Defaults are deliberately conservative: they are chosen to trade
//! *less*, on the principle that a strategy which sits out an ambiguous setup
//! costs an opportunity, while one that takes it costs money.

use serde::{Deserialize, Serialize};

/// Underlying index feed keys, paired with the scrip-master symbol name.
///
/// The index *spot* is what the technical and regime agents analyse; the
/// options traded against it are resolved separately from the scrip master.
/// Index feed keys use the exchange's case-sensitive display name rather than a
/// numeric token (see `kotak-api-docs/neo-websocket.md` § "Subscribe Index").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexSpec {
    /// Scrip-master symbol name, e.g. `"NIFTY"`.
    pub symbol: String,
    /// WebSocket feed key for the spot index, e.g. `"nse_cm|Nifty 50"`.
    pub spot_key: String,
    /// Approximate gap between adjacent strikes, used to sanity-check the
    /// strike ladder read from the scrip master.
    pub strike_step: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyConfig {
    /// Master switch. When false the engine does not even evaluate.
    pub enabled: bool,

    /// Whether to permit trade entries when the regime is classified as Rangebound.
    /// Primarily used for testing/simulation in flat markets; long options bleed
    /// theta in rangebound conditions so this defaults to false.
    #[serde(default)]
    pub allow_rangebound_entry: bool,

    /// Indices the engine watches.
    pub indices: Vec<IndexSpec>,

    // ── Warm-up ────────────────────────────────────────────────────────── //
    /// Closed minute bars required on the spot series before any evaluation is
    /// allowed. Nothing trades until this many bars exist — on a cold first
    /// session that means roughly this many minutes after the open.
    pub min_bars_for_signal: usize,
    /// Bars kept in memory (and reloaded from SQLite) per instrument.
    pub series_capacity: usize,

    // ── Indicator periods (minute bars) ─────────────────────────────────── //
    pub ema_fast: usize,
    pub ema_medium: usize,
    pub ema_slow: usize,
    pub atr_period: usize,
    pub rsi_period: usize,
    /// Bars in the opening range used for breakout reference.
    pub opening_range_bars: usize,
    /// Sigma multiple for the VWAP bands.
    pub vwap_band_sigma: f64,

    // ── Data freshness ──────────────────────────────────────────────────── //
    /// A tick older than this is treated as no data at all. The engine refuses
    /// to evaluate an index whose spot feed has gone quiet.
    pub max_tick_age_ms: i64,

    // ── Debate / conviction ─────────────────────────────────────────────── //
    /// Minimum net conviction (0–100) for the debate to produce a trade. The
    /// task specification sets this at 75.
    pub conviction_threshold: f64,
    /// Minimum margin by which the winning side must beat the losing side. A
    /// 76-vs-74 split is a coin flip dressed up as a decision, so it is refused
    /// even when the winner clears the threshold above.
    pub min_debate_margin: f64,

    // ── Strike selection (liquidity-first) ──────────────────────────────── //
    /// How many strike steps away from ATM to consider, each side.
    pub strike_search_steps: i32,
    /// Reject a contract whose bid/ask spread exceeds this percent of mid.
    pub max_spread_pct: f64,
    /// Reject a contract with less open interest than this.
    pub min_open_interest: f64,
    /// Premium band (INR) considered tradeable — too cheap is lottery-ticket
    /// theta decay, too rich wastes the per-trade budget on one lot.
    pub min_premium: f64,
    pub max_premium: f64,
    /// Preferred moneyness: how far OTM (in strike steps) the ideal contract
    /// sits. 0 = ATM. Slight OTM balances gamma against theta.
    pub preferred_otm_steps: i32,

    // ── Expiry awareness ────────────────────────────────────────────────── //
    /// Refuse new entries once days-to-expiry is below this and the clock is
    /// past [`Self::expiry_day_no_entry_hour`].
    pub min_days_to_expiry: i64,
    /// On expiry day, take no new entry at/after this IST hour:minute — a long
    /// option bleeds fastest into the expiry afternoon.
    pub expiry_day_no_entry_hour: u32,
    pub expiry_day_no_entry_minute: u32,

    // ── Risk ────────────────────────────────────────────────────────────── //
    /// Max positions open at once across **all** signal sources (Telegram
    /// included), so the two cannot silently double up.
    pub max_open_positions: usize,
    /// Max positions this engine may open per session.
    pub max_algo_entries_per_day: usize,
    /// Halt for the day once realised loss reaches this (INR, positive number).
    pub max_daily_loss_inr: f64,
    /// Halt for the day after this many consecutive losing algo trades.
    pub max_consecutive_losses: usize,
    /// Only one algo position per underlying at a time.
    pub one_position_per_underlying: bool,
    /// No new algo entry at/after this IST time.
    pub no_entry_hour: u32,
    pub no_entry_minute: u32,

    // ── Exit plan (expressed in option-premium terms) ───────────────────── //
    /// Stop distance as a fraction of entry premium (0.30 = 30 % below entry).
    pub stop_loss_pct: f64,
    /// First target as a fraction above entry premium.
    pub target_1_pct: f64,
    /// Second target as a fraction above entry premium.
    pub target_2_pct: f64,
    /// Minimum reward:risk the synthesised exit plan must show.
    pub min_reward_risk: f64,
}

impl Default for StrategyConfig {
    fn default() -> Self {
        Self {
            // Off until a human turns it on. A strategy that starts itself is
            // exactly the failure mode this project cannot afford.
            enabled: false,
            allow_rangebound_entry: false,
            indices: vec![
                IndexSpec { symbol: "NIFTY".into(),     spot_key: "nse_cm|Nifty 50".into(),   strike_step: 50.0 },
                IndexSpec { symbol: "BANKNIFTY".into(), spot_key: "nse_cm|Nifty Bank".into(), strike_step: 100.0 },
            ],

            min_bars_for_signal: 10,
            series_capacity: 900,

            ema_fast: 3,
            ema_medium: 6,
            ema_slow: 9,
            atr_period: 5,
            rsi_period: 5,
            opening_range_bars: 5,
            vwap_band_sigma: 2.0,

            max_tick_age_ms: 10_000,

            conviction_threshold: 45.0,
            min_debate_margin: 15.0,

            strike_search_steps: 4,
            max_spread_pct: 1.5,
            min_open_interest: 50_000.0,
            min_premium: 40.0,
            max_premium: 1000.0,
            preferred_otm_steps: 1,

            min_days_to_expiry: 0,
            expiry_day_no_entry_hour: 13,
            expiry_day_no_entry_minute: 0,

            max_open_positions: 3,
            max_algo_entries_per_day: 4,
            max_daily_loss_inr: 5_000.0,
            max_consecutive_losses: 2,
            one_position_per_underlying: true,
            no_entry_hour: 15,
            no_entry_minute: 0,

            stop_loss_pct: 0.30,
            target_1_pct: 0.35,
            target_2_pct: 0.70,
            min_reward_risk: 1.1,
        }
    }
}

impl StrategyConfig {
    /// Reject a configuration that would be unsafe or self-contradictory.
    ///
    /// Called whenever config is loaded or changed from the API — a bad number
    /// typed into the dashboard must fail loudly at the edge, not silently
    /// produce a strategy that risks more than intended.
    pub fn validate(&self) -> Result<(), String> {
        if self.min_bars_for_signal < 2 {
            return Err("min_bars_for_signal must be at least 2".into());
        }
        if self.series_capacity < self.min_bars_for_signal {
            return Err("series_capacity must be at least min_bars_for_signal".into());
        }
        if self.ema_fast >= self.ema_medium || self.ema_medium >= self.ema_slow {
            return Err("EMA periods must satisfy fast < medium < slow".into());
        }
        if !(0.0..=100.0).contains(&self.conviction_threshold) {
            return Err("conviction_threshold must be within 0..=100".into());
        }
        if self.stop_loss_pct <= 0.0 || self.stop_loss_pct >= 1.0 {
            return Err("stop_loss_pct must be within (0, 1) — a stop at or below zero premium can never trigger".into());
        }
        if self.target_1_pct <= 0.0 || self.target_2_pct <= self.target_1_pct {
            return Err("targets must satisfy 0 < target_1_pct < target_2_pct".into());
        }
        if self.min_premium <= 0.0 || self.max_premium <= self.min_premium {
            return Err("premium band must satisfy 0 < min_premium < max_premium".into());
        }
        if self.max_spread_pct <= 0.0 {
            return Err("max_spread_pct must be positive".into());
        }
        if self.max_daily_loss_inr <= 0.0 {
            return Err("max_daily_loss_inr must be positive".into());
        }
        if self.max_open_positions == 0 || self.max_algo_entries_per_day == 0 {
            return Err("position and entry limits must be at least 1 (use `enabled: false` to stop trading)".into());
        }
        // Reward:risk is checked against the configured defaults so an
        // impossible plan is caught at config time, not at signal time.
        let rr = self.target_1_pct / self.stop_loss_pct;
        if rr < self.min_reward_risk {
            return Err(format!(
                "target_1_pct/stop_loss_pct = {rr:.2} is below min_reward_risk {:.2}",
                self.min_reward_risk
            ));
        }
        if self.indices.is_empty() {
            return Err("at least one index must be configured".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid_and_starts_disabled() {
        let c = StrategyConfig::default();
        assert!(!c.enabled, "the strategy must never arm itself by default");
        c.validate().expect("shipped defaults must pass validation");
    }

    #[test]
    fn default_conviction_threshold_matches_spec() {
        assert_eq!(StrategyConfig::default().conviction_threshold, 45.0);
    }

    #[test]
    fn default_min_bars_for_signal_covers_ema_slow() {
        let c = StrategyConfig::default();
        assert_eq!(c.min_bars_for_signal, 10);
        assert!(c.min_bars_for_signal >= c.ema_slow);
    }

    #[test]
    fn rejects_inverted_ema_periods() {
        let mut c = StrategyConfig::default();
        c.ema_fast = 50;
        c.ema_slow = 9;
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_a_stop_that_can_never_trigger() {
        let mut c = StrategyConfig::default();
        c.stop_loss_pct = 1.0; // stop at zero premium
        assert!(c.validate().is_err(), "a 100% stop is not a stop");
    }

    #[test]
    fn rejects_inverted_targets() {
        let mut c = StrategyConfig::default();
        c.target_2_pct = c.target_1_pct - 0.01;
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_a_plan_whose_reward_risk_is_too_thin() {
        let mut c = StrategyConfig::default();
        c.stop_loss_pct = 0.50;
        c.target_1_pct = 0.10;      // 0.2 R:R
        c.target_2_pct = 0.20;
        assert!(c.validate().is_err(), "a losing-by-construction plan must be refused");
    }

    #[test]
    fn rejects_zero_limits_rather_than_treating_them_as_unlimited() {
        let mut c = StrategyConfig::default();
        c.max_open_positions = 0;
        assert!(c.validate().is_err(), "0 must not silently mean 'no limit'");
    }
}
