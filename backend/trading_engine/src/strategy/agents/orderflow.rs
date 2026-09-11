//! Order-flow analyst — volume, book pressure and option-chain positioning.
//!
//! This is the agent that the feed widening exists for. It reads fields the
//! original engine discarded: cumulative volume, total resting buy/sell
//! quantity, and per-contract open interest across the chain.
//!
//! Three independent readings:
//!
//! 1. **Volume expansion** — is the current bar's volume meaningfully above the
//!    session's own recent average? Direction comes from the bar's own body, so
//!    a volume spike only counts as bullish if the bar it belongs to closed up.
//! 2. **Book imbalance** — resting buy quantity against resting sell quantity on
//!    the underlying's feed.
//! 3. **Put/Call ratio** — total put OI against total call OI across the near
//!    strikes. Read *contrarian at the extremes* and directional in between,
//!    which is the standard interpretation: a very high PCR means puts are
//!    crowded and the downside is already hedged.
//!
//! Every reading is skipped rather than guessed when its data is absent. Index
//! feeds carry no volume at all, so this agent frequently — and correctly —
//! returns a low-confidence view for index spot.

use shared_domain::{MarketTick, TickStore};

use super::{AgentView, Stance};
use crate::strategy::candles::Candle;
use crate::strategy::config::StrategyConfig;

pub struct OrderFlowAnalyst;

/// Relative say in the debate. Confirmatory rather than primary: order flow is
/// excellent at invalidating a technical setup and weaker at originating one.
pub const WEIGHT: f64 = 1.0;

/// Bar volume above this multiple of the recent average counts as expansion.
const VOLUME_SPIKE_MULT: f64 = 1.6;

/// Book imbalance beyond this magnitude counts as real pressure.
const IMBALANCE_THRESHOLD: f64 = 0.15;

/// PCR above this means puts are crowded — read as contrarian bullish.
const PCR_HIGH: f64 = 1.3;
/// PCR below this means calls are crowded — contrarian bearish.
const PCR_LOW: f64 = 0.7;

/// Total call and put open interest across a set of contracts.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ChainOi {
    pub call_oi: f64,
    pub put_oi: f64,
}

impl ChainOi {
    /// Put/call ratio. `None` when call OI is zero — an undefined ratio, not
    /// an infinitely bullish one.
    pub fn pcr(&self) -> Option<f64> {
        (self.call_oi > 0.0).then(|| self.put_oi / self.call_oi)
    }
}

impl OrderFlowAnalyst {
    /// Form a view from the underlying's bars/tick and the option chain's OI.
    ///
    /// `chain` is the accumulated call/put OI for the near strikes; pass
    /// `ChainOi::default()` when the chain is not subscribed, and the PCR
    /// reading is skipped rather than fabricated.
    pub fn analyse(
        bars: &[Candle],
        spot_tick: Option<&MarketTick>,
        chain: ChainOi,
        cfg: &StrategyConfig,
    ) -> AgentView {
        if bars.len() < cfg.min_bars_for_signal {
            return AgentView::neutral(
                "OrderFlowAnalyst",
                0.0,
                format!("warming up — {} of {} bars", bars.len(), cfg.min_bars_for_signal),
            );
        }

        let mut score: f64 = 0.0;
        let mut evidence: Vec<String> = Vec::new();
        // Tracks whether any reading actually had data. If none did, the agent
        // reports neutral with zero confidence instead of a confident "0".
        let mut readings = 0u32;

        // ── 1. Volume expansion (±35) ───────────────────────────────────── //
        let recent = &bars[bars.len().saturating_sub(21)..];
        let (last, prior) = recent.split_last().expect("non-empty after warm-up check");
        let avg_vol = if prior.is_empty() { 0.0 } else { prior.iter().map(|c| c.volume).sum::<f64>() / prior.len() as f64 };

        if avg_vol > 0.0 && last.volume > 0.0 {
            readings += 1;
            let mult = last.volume / avg_vol;
            if mult >= VOLUME_SPIKE_MULT {
                // Direction from the bar's own body — volume alone is not signed.
                let body = last.close - last.open;
                let magnitude = 35.0 * ((mult - VOLUME_SPIKE_MULT) / 2.0).clamp(0.2, 1.0);
                if body > 0.0 {
                    score += magnitude;
                    evidence.push(format!("volume {mult:.1}× the 20-bar average on an up bar"));
                } else if body < 0.0 {
                    score -= magnitude;
                    evidence.push(format!("volume {mult:.1}× the 20-bar average on a down bar"));
                } else {
                    evidence.push(format!("volume {mult:.1}× average but the bar closed flat — unsigned"));
                }
            } else {
                evidence.push(format!("volume {mult:.1}× average — no expansion"));
            }
        } else {
            evidence.push("no volume reported on this feed — volume reading skipped".into());
        }

        // ── 2. Book imbalance (±30) ─────────────────────────────────────── //
        match spot_tick.and_then(|t| t.book_imbalance()) {
            Some(imb) if imb.abs() >= IMBALANCE_THRESHOLD => {
                readings += 1;
                let magnitude = 30.0 * (imb.abs() / 0.5).clamp(0.0, 1.0);
                score += magnitude * imb.signum();
                evidence.push(format!(
                    "resting book {:.0}% skewed to the {} side",
                    imb.abs() * 100.0,
                    if imb > 0.0 { "buy" } else { "sell" }
                ));
            }
            Some(imb) => {
                readings += 1;
                evidence.push(format!("book roughly balanced ({imb:+.2})"));
            }
            None => evidence.push("no book quantities on this feed — imbalance reading skipped".into()),
        }

        // ── 3. Put/call ratio (±35) ─────────────────────────────────────── //
        match chain.pcr() {
            Some(pcr) => {
                readings += 1;
                if pcr >= PCR_HIGH {
                    score += 35.0 * ((pcr - PCR_HIGH) / 0.7).clamp(0.3, 1.0);
                    evidence.push(format!("PCR {pcr:.2} — puts crowded, downside already hedged (contrarian bullish)"));
                } else if pcr <= PCR_LOW {
                    score -= 35.0 * ((PCR_LOW - pcr) / 0.4).clamp(0.3, 1.0);
                    evidence.push(format!("PCR {pcr:.2} — calls crowded, upside already bought (contrarian bearish)"));
                } else {
                    evidence.push(format!("PCR {pcr:.2} is balanced"));
                }
            }
            None => evidence.push("option-chain OI unavailable — PCR reading skipped".into()),
        }

        if readings == 0 {
            // Feeds without volume, depth, or chain data must not dilute the debate
            // denominator with phantom weight. Weight is set to 0.0 so active agents
            // with real data determine consensus.
            return AgentView::neutral(
                "OrderFlowAnalyst",
                0.0,
                "no order-flow data available on this feed — abstaining (weight 0.0)",
            );
        }

        let stance = if score >= 20.0 {
            Stance::Bullish
        } else if score <= -20.0 {
            Stance::Bearish
        } else {
            Stance::Neutral
        };

        // When OrderFlowAnalyst is neutral (e.g. balanced PCR, or no volume/depth edge),
        // it must abstain with weight 0.0 rather than diluting the debate denominator.
        // It only participates with active weight when a real order-flow imbalance is detected.
        let weight = if stance == Stance::Neutral { 0.0 } else { WEIGHT };

        AgentView {
            agent: "OrderFlowAnalyst".into(),
            stance,
            confidence: score.abs().min(100.0),
            weight,
            evidence,
        }
    }
}

/// Accumulate call/put open interest across the near strikes of a chain.
///
/// Contracts with no live tick, or a stale one, contribute nothing — a
/// half-subscribed chain must not produce a confidently wrong PCR.
pub fn chain_oi(keys_by_type: &[(String, String)], ticks: &TickStore, now_ms: i64, max_age_ms: i64) -> ChainOi {
    let mut out = ChainOi::default();
    for (key, opt_type) in keys_by_type {
        let Some(t) = ticks.get(key.as_str()) else { continue };
        if t.is_stale(now_ms, max_age_ms) {
            continue;
        }
        let Some(oi) = t.open_interest else { continue };
        match opt_type.as_str() {
            "CE" => out.call_oi += oi,
            "PE" => out.put_oi += oi,
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use dashmap::DashMap;
    use serde_json::json;
    use std::sync::Arc;

    fn cfg() -> StrategyConfig {
        StrategyConfig { min_bars_for_signal: 30, ..Default::default() }
    }

    /// Flat-volume bars, so a test can spike only the final one.
    fn bars_with(last_vol: f64, last_body: f64) -> Vec<Candle> {
        let mut v: Vec<Candle> = (0..40).map(|i| Candle {
            minute: i, open: 100.0, high: 101.0, low: 99.0, close: 100.0,
            volume: 1_000.0, ticks: 10,
        }).collect();
        let l = v.last_mut().unwrap();
        l.volume = last_vol;
        l.open = 100.0;
        l.close = 100.0 + last_body;
        v
    }

    fn tick_with(tbq: f64, tsq: f64) -> MarketTick {
        let mut t = MarketTick::new("nse_cm|Nifty 50");
        t.merge_from(&json!({"ltp": 24000.0, "tbq": tbq, "tsq": tsq}), 1_000);
        t
    }

    #[test]
    fn is_neutral_with_zero_weight_while_warming_up() {
        let bars: Vec<Candle> = (0..5).map(|i| Candle {
            minute: i, open: 100.0, high: 101.0, low: 99.0, close: 100.0, volume: 1000.0, ticks: 5,
        }).collect();
        let v = OrderFlowAnalyst::analyse(&bars, None, ChainOi::default(), &cfg());
        assert_eq!(v.stance, Stance::Neutral);
        assert_eq!(v.confidence, 0.0);
        assert_eq!(v.weight, 0.0);
        assert!(v.evidence[0].contains("warming up"));
    }

    #[test]
    fn abstains_when_no_data_is_available_at_all() {
        // Index feed: no volume, no book, no chain.
        let bars: Vec<Candle> = (0..40).map(|i| Candle {
            minute: i, open: 100.0, high: 101.0, low: 99.0, close: 100.0, volume: 0.0, ticks: 5,
        }).collect();
        let v = OrderFlowAnalyst::analyse(&bars, None, ChainOi::default(), &cfg());
        assert_eq!(v.stance, Stance::Neutral);
        assert_eq!(v.confidence, 0.0, "no data must mean no confidence, not a confident zero");
        assert_eq!(v.weight, 0.0, "absent feed data must have 0 weight to not dilute consensus");
        assert!(v.evidence[0].contains("abstaining"), "got {:?}", v.evidence);
    }

    #[test]
    fn volume_spike_on_an_up_bar_is_bullish() {
        let v = OrderFlowAnalyst::analyse(&bars_with(5_000.0, 2.0), None, ChainOi::default(), &cfg());
        assert_eq!(v.stance, Stance::Bullish, "{:?}", v.evidence);
    }

    #[test]
    fn the_same_volume_spike_on_a_down_bar_is_bearish() {
        let v = OrderFlowAnalyst::analyse(&bars_with(5_000.0, -2.0), None, ChainOi::default(), &cfg());
        assert_eq!(v.stance, Stance::Bearish, "volume is unsigned — the bar body signs it: {:?}", v.evidence);
    }

    #[test]
    fn volume_spike_on_a_flat_bar_is_not_directional() {
        let v = OrderFlowAnalyst::analyse(&bars_with(5_000.0, 0.0), None, ChainOi::default(), &cfg());
        assert_eq!(v.stance, Stance::Neutral);
    }

    #[test]
    fn book_imbalance_moves_the_view() {
        let heavy_buy = tick_with(900_000.0, 100_000.0);
        let v = OrderFlowAnalyst::analyse(&bars_with(1_000.0, 0.0), Some(&heavy_buy), ChainOi::default(), &cfg());
        assert_eq!(v.stance, Stance::Bullish, "{:?}", v.evidence);

        let heavy_sell = tick_with(100_000.0, 900_000.0);
        let v = OrderFlowAnalyst::analyse(&bars_with(1_000.0, 0.0), Some(&heavy_sell), ChainOi::default(), &cfg());
        assert_eq!(v.stance, Stance::Bearish);
    }

    #[test]
    fn pcr_is_read_contrarian_at_the_extremes() {
        let bars = bars_with(1_000.0, 0.0);
        // Puts crowded → contrarian bullish.
        let v = OrderFlowAnalyst::analyse(&bars, None, ChainOi { call_oi: 100_000.0, put_oi: 220_000.0 }, &cfg());
        assert_eq!(v.stance, Stance::Bullish, "{:?}", v.evidence);

        // Calls crowded → contrarian bearish.
        let v = OrderFlowAnalyst::analyse(&bars, None, ChainOi { call_oi: 250_000.0, put_oi: 100_000.0 }, &cfg());
        assert_eq!(v.stance, Stance::Bearish, "{:?}", v.evidence);
    }

    #[test]
    fn balanced_pcr_is_not_directional() {
        let v = OrderFlowAnalyst::analyse(&bars_with(1_000.0, 0.0), None, ChainOi { call_oi: 100_000.0, put_oi: 100_000.0 }, &cfg());
        assert_eq!(v.stance, Stance::Neutral, "{:?}", v.evidence);
        assert_eq!(v.weight, 0.0, "neutral order flow must abstain with 0 weight");
    }

    #[test]
    fn pcr_is_undefined_rather_than_infinite_without_call_oi() {
        assert_eq!(ChainOi { call_oi: 0.0, put_oi: 500.0 }.pcr(), None);
    }

    #[test]
    fn chain_oi_ignores_stale_and_missing_quotes() {
        let ticks: TickStore = Arc::new(DashMap::new());
        let mut fresh = MarketTick::new("nse_fo|1");
        fresh.merge_from(&json!({"oi": 100_000.0}), 10_000);
        ticks.insert("nse_fo|1".into(), fresh);

        let mut stale = MarketTick::new("nse_fo|2");
        stale.merge_from(&json!({"oi": 900_000.0}), 0);
        ticks.insert("nse_fo|2".into(), stale);

        let keys = vec![
            ("nse_fo|1".to_string(), "CE".to_string()),
            ("nse_fo|2".to_string(), "CE".to_string()),  // stale
            ("nse_fo|3".to_string(), "PE".to_string()),  // absent
        ];
        let oi = chain_oi(&keys, &ticks, 12_000, 5_000);
        assert_eq!(oi.call_oi, 100_000.0, "stale OI must not be counted");
        assert_eq!(oi.put_oi, 0.0, "absent contracts contribute nothing");
    }
}
