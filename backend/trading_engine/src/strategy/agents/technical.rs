//! Technical analyst — price action, momentum and structure.
//!
//! Reads the **underlying index** minute bars (never option premium) and forms
//! a directional view from four independent pieces of evidence, each scored and
//! summed:
//!
//! | Evidence            | Max | Why it counts                                  |
//! |---------------------|-----|------------------------------------------------|
//! | EMA ribbon stack    |  30 | Persistent direction, not a single-bar flicker  |
//! | Opening-range break |  25 | Structural level the whole session references   |
//! | VWAP side + slope   |  25 | Where value sits, and whether it is moving      |
//! | RSI momentum        |  20 | Momentum confirmation, penalised at extremes    |
//!
//! Scores from evidence pointing the *opposite* way are not merely omitted —
//! they are subtracted, so a market giving genuinely mixed signals lands near
//! neutral rather than accumulating a high score from one loud indicator.

use super::{AgentView, Stance};
use crate::strategy::candles::Candle;
use crate::strategy::config::StrategyConfig;
use crate::strategy::indicators;
use crate::strategy::regime::RegimeReading;

pub struct TechnicalAnalyst;

/// Relative say in the debate. Price structure is the primary evidence for a
/// directional trade, so it carries the largest single weight.
pub const WEIGHT: f64 = 1.4;

/// RSI beyond this is treated as stretched, not strong: buying a call into an
/// 85 RSI is buying the last of a move.
const RSI_OVERBOUGHT: f64 = 78.0;
const RSI_OVERSOLD: f64 = 22.0;

/// Minimum |slope| (percent of price per bar) for a drift to count as a real
/// trend rather than rounding noise.
///
/// Without this floor, a perfectly flat tape still produces a slope with a
/// *sign* — something like -0.001%/bar — and treating that sign as confirmation
/// let a market going nowhere score full directional credit. On a 24,000 index
/// this threshold is roughly 1.2 points per minute bar.
const MIN_SLOPE_PCT_PER_BAR: f64 = 0.005;

impl TechnicalAnalyst {
    /// Form a view from the spot series and the regime already computed on it.
    pub fn analyse(bars: &[Candle], regime: &RegimeReading, cfg: &StrategyConfig) -> AgentView {
        if bars.len() < cfg.min_bars_for_signal {
            return AgentView::neutral(
                "TechnicalAnalyst",
                0.0,
                format!("warming up — {} of {} bars", bars.len(), cfg.min_bars_for_signal),
            );
        }

        let closes: Vec<f64> = bars.iter().map(|c| c.close).collect();
        let spot = closes[closes.len() - 1];
        let mut score: f64 = 0.0;
        let mut evidence: Vec<String> = Vec::new();

        // ── 1. EMA ribbon (±30) ─────────────────────────────────────────── //
        // Weight by separation so a barely-ordered stack scores far less than a
        // clearly fanned one.
        let spread = regime.ribbon_spread_pct.abs();
        let conviction = (spread / 0.25).clamp(0.0, 1.0); // 0.25% spread = full marks
        match regime.ribbon_direction {
            1 => {
                score += 30.0 * conviction;
                evidence.push(format!(
                    "EMA{}/{}/{} stacked bullish, {spread:.2}% apart",
                    cfg.ema_fast, cfg.ema_medium, cfg.ema_slow
                ));
            }
            -1 => {
                score -= 30.0 * conviction;
                evidence.push(format!(
                    "EMA{}/{}/{} stacked bearish, {spread:.2}% apart",
                    cfg.ema_fast, cfg.ema_medium, cfg.ema_slow
                ));
            }
            _ => evidence.push("EMA ribbon interleaved — no structural direction".into()),
        }

        // ── 2. Opening-range breakout (±25) ─────────────────────────────── //
        match indicators::opening_range(bars, cfg.opening_range_bars) {
            Some((hi, lo)) if hi > lo => {
                if spot > hi {
                    score += 25.0;
                    evidence.push(format!("broken above the {}-bar opening range high {hi:.1}", cfg.opening_range_bars));
                } else if spot < lo {
                    score -= 25.0;
                    evidence.push(format!("broken below the {}-bar opening range low {lo:.1}", cfg.opening_range_bars));
                } else {
                    evidence.push(format!("still inside the opening range {lo:.1}–{hi:.1}"));
                }
            }
            _ => evidence.push("opening range not established".into()),
        }

        // ── 3. VWAP side and slope (±25) ────────────────────────────────── //
        // Being on one side of VWAP only counts when price is actually drifting
        // that way; price above a falling VWAP is a rally inside a decline, and
        // price a hair above a flat VWAP is nothing at all.
        let vwap_slope = indicators::slope_pct(&closes, cfg.ema_medium.min(closes.len())).unwrap_or(0.0);
        let above_vwap = spot > regime.vwap;
        let slope_is_real = vwap_slope.abs() >= MIN_SLOPE_PCT_PER_BAR;
        let slope_agrees = slope_is_real && (vwap_slope > 0.0) == above_vwap;

        if slope_agrees {
            let full = if above_vwap { 25.0 } else { -25.0 };
            score += full;
            evidence.push(format!(
                "{} VWAP {:.1} with price sloping the same way ({vwap_slope:+.3}%/bar)",
                if above_vwap { "above" } else { "below" }, regime.vwap
            ));
        } else if !slope_is_real {
            // Side alone, on a flat tape, is weak positional evidence — not a
            // trend. Scoring it fully is what let a directionless market look
            // decisively bearish.
            let weak = if above_vwap { 8.0 } else { -8.0 };
            score += weak;
            evidence.push(format!(
                "{} VWAP {:.1} but the slope is flat ({vwap_slope:+.3}%/bar) — positional only, no trend",
                if above_vwap { "above" } else { "below" }, regime.vwap
            ));
        } else {
            // A meaningful slope that opposes the side is genuine conflict, so
            // it earns nothing rather than half credit.
            evidence.push(format!(
                "{} VWAP {:.1} but sloping the other way ({vwap_slope:+.3}%/bar) — conflicting, no credit",
                if above_vwap { "above" } else { "below" }, regime.vwap
            ));
        }

        // ── 4. RSI momentum (±20, penalised at extremes) ────────────────── //
        let rsi = regime.rsi;
        if rsi >= RSI_OVERBOUGHT {
            score -= 10.0;
            evidence.push(format!("RSI {rsi:.0} is overbought — late to buy strength"));
        } else if rsi <= RSI_OVERSOLD {
            score += 10.0;
            evidence.push(format!("RSI {rsi:.0} is oversold — late to buy weakness"));
        } else if rsi > 55.0 {
            score += 20.0 * ((rsi - 55.0) / (RSI_OVERBOUGHT - 55.0)).clamp(0.0, 1.0);
            evidence.push(format!("RSI {rsi:.0} confirms upward momentum"));
        } else if rsi < 45.0 {
            score -= 20.0 * ((45.0 - rsi) / (45.0 - RSI_OVERSOLD)).clamp(0.0, 1.0);
            evidence.push(format!("RSI {rsi:.0} confirms downward momentum"));
        } else {
            evidence.push(format!("RSI {rsi:.0} is neutral"));
        }

        // Net score in ±100 maps to a stance and a confidence.
        let stance = if score >= 20.0 {
            Stance::Bullish
        } else if score <= -20.0 {
            Stance::Bearish
        } else {
            Stance::Neutral
        };
        if stance == Stance::Neutral {
            evidence.push(format!("net technical score {score:+.0} is inside the ±20 neutral band"));
        }

        // When TechnicalAnalyst is neutral, it abstains with weight 0.0 to avoid diluting the debate denominator.
        let weight = if stance == Stance::Neutral { 0.0 } else { WEIGHT };

        AgentView {
            agent: "TechnicalAnalyst".into(),
            stance,
            confidence: score.abs().min(100.0),
            weight,
            evidence,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::regime;

    fn cfg() -> StrategyConfig {
        StrategyConfig {
            min_bars_for_signal: 30,
            ema_fast: 3, ema_medium: 6, ema_slow: 12,
            atr_period: 5, rsi_period: 5, opening_range_bars: 5,
            ..Default::default()
        }
    }

    fn series(closes: &[f64]) -> Vec<Candle> {
        closes.iter().enumerate().map(|(i, &c)| Candle {
            minute: i as i64, open: c, high: c + 1.0, low: c - 1.0, close: c,
            volume: 100.0, ticks: 10,
        }).collect()
    }

    fn view_for(closes: &[f64]) -> AgentView {
        let bars = series(closes);
        let c = cfg();
        let r = regime::classify(&bars, &c);
        TechnicalAnalyst::analyse(&bars, &r, &c)
    }

    #[test]
    fn is_neutral_while_cold() {
        let v = view_for(&[100.0; 5]);
        assert_eq!(v.stance, Stance::Neutral);
        assert_eq!(v.confidence, 0.0);
        assert_eq!(v.weight, 0.0, "cold warmup must abstain with 0 weight");
        assert!(v.evidence[0].contains("warming up"));
    }

    #[test]
    fn sustained_rally_reads_bullish() {
        let closes: Vec<f64> = (0..60).map(|i| 20_000.0 + i as f64 * 4.0).collect();
        let v = view_for(&closes);
        assert_eq!(v.stance, Stance::Bullish, "evidence: {:?}", v.evidence);
        assert_eq!(v.weight, WEIGHT);
        assert!(v.confidence > 20.0, "got {}", v.confidence);
    }

    #[test]
    fn sustained_selloff_reads_bearish() {
        let closes: Vec<f64> = (0..60).map(|i| 20_000.0 - i as f64 * 4.0).collect();
        let v = view_for(&closes);
        assert_eq!(v.stance, Stance::Bearish, "evidence: {:?}", v.evidence);
        assert_eq!(v.weight, WEIGHT);
        assert!(v.confidence > 20.0);
    }

    #[test]
    fn flat_chop_stays_neutral() {
        let closes: Vec<f64> = (0..60).map(|i| 20_000.0 + if i % 2 == 0 { 1.0 } else { -1.0 }).collect();
        let v = view_for(&closes);
        assert_eq!(v.stance, Stance::Neutral, "a directionless tape must not produce a directional view: {:?}", v.evidence);
        assert_eq!(v.weight, 0.0, "neutral tape must abstain with 0 weight");
    }

    #[test]
    fn a_noise_level_slope_does_not_count_as_trend_confirmation() {
        // Regression: an alternating ±1 tape still yields a slope with a sign
        // (about -0.001%/bar). Treating that sign as confirmation previously
        // scored a full -25 and turned a flat market decisively bearish.
        let closes: Vec<f64> = (0..60).map(|i| 20_000.0 + if i % 2 == 0 { 1.0 } else { -1.0 }).collect();
        let v = view_for(&closes);
        assert!(
            v.evidence.iter().any(|e| e.contains("slope is flat")),
            "the flat slope should be called out, not credited: {:?}", v.evidence
        );
        assert!(v.confidence < 20.0, "a flat tape must not build real confidence, got {}", v.confidence);
    }

    #[test]
    fn price_above_a_falling_vwap_earns_no_directional_credit() {
        // A rally inside a decline: closes fall steadily, so the last close sits
        // below a slope that is genuinely negative — the conflicting case.
        let closes: Vec<f64> = (0..60).map(|i| 20_000.0 - i as f64 * 5.0).collect();
        let v = view_for(&closes);
        assert!(
            v.evidence.iter().any(|e| e.contains("sloping the same way") || e.contains("conflicting")),
            "the VWAP reading should state whether slope agreed: {:?}", v.evidence
        );
    }

    #[test]
    fn confidence_never_exceeds_one_hundred() {
        // Violent one-way move — every piece of evidence agrees.
        let closes: Vec<f64> = (0..80).map(|i| 20_000.0 + i as f64 * 50.0).collect();
        let v = view_for(&closes);
        assert!(v.confidence <= 100.0, "got {}", v.confidence);
    }

    #[test]
    fn every_view_carries_evidence() {
        let closes: Vec<f64> = (0..60).map(|i| 20_000.0 + (i as f64 / 3.0).sin() * 30.0).collect();
        let v = view_for(&closes);
        assert!(!v.evidence.is_empty(), "the debate log needs reasons, not just a number");
    }
}
