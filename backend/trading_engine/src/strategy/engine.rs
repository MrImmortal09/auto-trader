//! Strategy engine — orchestration, signal synthesis and the safety gate.
//!
//! Owns the per-instrument bar history, runs the agent pipeline on a fixed
//! cadence, and turns an actionable debate into a [`TradeSignal`].
//!
//! # The safety gate
//!
//! [`StrategyEngine::evaluate_once`] returns a [`Decision`]. A decision only
//! becomes a published signal through [`StrategyEngine::publish`], and that
//! method refuses to publish unless `TradingConfig::mode` is exactly `"PAPER"`.
//! In LIVE mode the pipeline still runs and the decision is still recorded — so
//! the dashboard shows what the engine *would* have done — but nothing reaches
//! the order path.
//!
//! Every synthesised signal also carries `paper_only: true`, which the live
//! entry gate refuses independently. Either guard alone is sufficient; both
//! exist so that weakening one does not put real money behind this code.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use shared_domain::{MonitoredPosition, TickStore, TradeSignal, TradingConfig};
use tokio::sync::broadcast;

use super::agents::{
    debate::{DebateCoordinator, DebateOutcome},
    orderflow::{self, OrderFlowAnalyst},
    risk::{RiskDecision, RiskManager, RiskState},
    technical::TechnicalAnalyst,
    AgentView, Stance,
};
use super::candles::{Candle, CandleSeries};
use super::config::{IndexSpec, StrategyConfig};
use super::regime::{self, MarketRegime, RegimeReading};
use super::strikes::{self, StrikeSelection};
use crate::scrip_master::{ScripRecord, ScripStore};

/// Weight of the regime agent in the debate. Regime is a gate more than a
/// voice, so it carries less pull than the technical read but still counts.
const REGIME_WEIGHT: f64 = 1.2;

/// What one evaluation of one index concluded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub underlying: String,
    pub regime: RegimeReading,
    pub debate: DebateOutcome,
    /// Present once the debate was actionable and a strike search ran.
    pub selection: Option<StrikeSelection>,
    /// Present once a strike was found and risk was consulted.
    pub risk: Option<RiskDecision>,
    /// The signal this decision produced, if it survived every gate.
    pub signal: Option<TradeSignal>,
    /// Why the pipeline stopped where it did.
    pub outcome: String,
    /// IST timestamp of the evaluation.
    pub evaluated_at: String,
}

/// Everything the dashboard needs to render engine state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategySnapshot {
    pub enabled: bool,
    /// True when signals are actually being published (PAPER mode only).
    pub publishing: bool,
    /// Why publishing is suppressed, when it is.
    pub publish_block_reason: Option<String>,
    pub risk: RiskState,
    /// Most recent decision per underlying.
    pub decisions: Vec<Decision>,
    /// Bars accumulated per instrument, for the warm-up display.
    pub warmup: Vec<(String, usize)>,
    pub config: StrategyConfig,
}

pub struct StrategyEngine {
    pub cfg: StrategyConfig,
    /// Bar history keyed by feed key.
    series: HashMap<String, CandleSeries>,
    /// Most recent decision per underlying.
    decisions: HashMap<String, Decision>,
    pub risk: RiskState,
    /// Option feed keys the engine has asked to subscribe, so it does not
    /// re-request them every tick.
    subscribed: std::collections::HashSet<String>,
}

impl StrategyEngine {
    pub fn new(cfg: StrategyConfig) -> Self {
        Self {
            cfg,
            series: HashMap::new(),
            decisions: HashMap::new(),
            risk: RiskState { session_date: shared_domain::today_ist().to_string(), ..Default::default() },
            subscribed: std::collections::HashSet::new(),
        }
    }

    /// Seed one instrument's history from the database.
    pub fn seed_series(&mut self, key: &str, bars: Vec<Candle>) {
        let cap = self.cfg.series_capacity;
        self.series
            .entry(key.to_string())
            .or_insert_with(|| CandleSeries::new(key, cap))
            .seed(bars);
    }

    /// Fold the current state of every subscribed tick into the bar series.
    ///
    /// Returns the bars that closed on this pass, paired with their feed key,
    /// so the caller can persist them.
    pub fn ingest_ticks(&mut self, ticks: &TickStore, now_ms: i64) -> Vec<(String, Candle)> {
        let mut closed = Vec::new();
        let cap = self.cfg.series_capacity;
        let max_age = self.cfg.max_tick_age_ms;

        for entry in ticks.iter() {
            let (key, tick) = (entry.key(), entry.value());
            // A stale tick would otherwise keep extending the current bar with
            // a price the exchange stopped confirming.
            if tick.is_stale(now_ms, max_age) {
                continue;
            }
            let Some(price) = tick.ltp.filter(|p| *p > 0.0) else { continue };

            let s = self
                .series
                .entry(key.to_string())
                .or_insert_with(|| CandleSeries::new(key, cap));
            if let Some(bar) = s.push_tick(price, tick.volume, now_ms) {
                closed.push((key.to_string(), bar));
            }
        }
        closed
    }

    /// Bars accumulated per instrument.
    pub fn warmup_status(&self) -> Vec<(String, usize)> {
        let mut v: Vec<(String, usize)> = self.series.iter().map(|(k, s)| (k.clone(), s.len())).collect();
        v.sort();
        v
    }

    /// Feed keys for the near strikes of one index, so the caller can subscribe
    /// them and give the chain agents something to read.
    pub fn chain_keys(&self, index: &IndexSpec, store: &ScripStore, spot: f64) -> Vec<(String, String)> {
        let Some(records) = store.records.get(&index.symbol) else { return Vec::new() };
        if spot <= 0.0 {
            return Vec::new();
        }
        let today = shared_domain::today_ist();
        let refs: Vec<&ScripRecord> = records.iter().collect();
        let Some(expiry) = strikes::select_expiry(&refs, today, current_hm(), &self.cfg) else {
            return Vec::new();
        };
        let step = if index.strike_step > 0.0 { index.strike_step } else { 50.0 };
        let atm = (spot / step).round() * step;
        let span = self.cfg.strike_search_steps as f64 * step;

        // Strike scaling is resolved the same way the selector does it, by
        // comparing the chain against spot rather than guessing per row.
        let scale = {
            let mut ks: Vec<f64> = refs.iter().map(|r| r.strike_price).filter(|s| *s > 0.0).collect();
            ks.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let median = ks.get(ks.len() / 2).copied().unwrap_or(0.0);
            [1.0_f64, 100.0, 1000.0]
                .into_iter()
                .min_by(|a, b| {
                    (median / a - spot).abs().partial_cmp(&(median / b - spot).abs()).unwrap_or(std::cmp::Ordering::Equal)
                })
                .unwrap_or(1.0)
        };

        refs.iter()
            .filter(|r| r.expiry_date == expiry)
            .filter(|r| ((r.strike_price / scale) - atm).abs() <= span)
            .map(|r| (format!("{}|{}", r.exchange_segment_code, r.instrument_token), r.option_type.clone()))
            .collect()
    }

    /// Mark feed keys as subscribed, returning only the ones that are new.
    pub fn newly_subscribed(&mut self, keys: &[String]) -> Vec<String> {
        keys.iter()
            .filter(|k| self.subscribed.insert((*k).clone()))
            .cloned()
            .collect()
    }

    /// Clear subscribed set so new WebSocket connections re-subscribe all needed feeds.
    pub fn reset_subscribed(&mut self) {
        self.subscribed.clear();
    }

    /// Run the full pipeline for one index.
    ///
    /// This is pure with respect to the order path: it never places an order and
    /// never publishes. It only computes a [`Decision`].
    #[allow(clippy::too_many_arguments)]
    pub fn evaluate_once(
        &mut self,
        index: &IndexSpec,
        ticks: &TickStore,
        store: &ScripStore,
        open_positions: &[MonitoredPosition],
        max_trade_amount_inr: f64,
        now_ms: i64,
    ) -> Decision {
        let now_hm = current_hm();
        let evaluated_at = shared_domain::current_ist_timestamp_string();

        let stop = |regime: RegimeReading, debate: DebateOutcome, outcome: String| Decision {
            underlying: index.symbol.clone(),
            regime, debate,
            selection: None, risk: None, signal: None,
            outcome,
            evaluated_at: evaluated_at.clone(),
        };

        // ── Data freshness. No spot, no opinion. ────────────────────────── //
        let spot_tick = ticks.get(&index.spot_key).map(|t| t.clone());
        let spot_fresh = spot_tick.as_ref().is_some_and(|t| !t.is_stale(now_ms, self.cfg.max_tick_age_ms));

        let bars: Vec<Candle> = self
            .series
            .get(&index.spot_key)
            .map(|s| s.closed().iter().cloned().collect())
            .unwrap_or_default();

        let reading = regime::classify(&bars, &self.cfg);

        if !spot_fresh {
            let empty = DebateCoordinator::deliberate(Vec::new(), &self.cfg);
            return stop(reading, empty, format!("{} spot feed is stale or absent — standing down", index.spot_key));
        }

        // ── Agents ──────────────────────────────────────────────────────── //
        let technical = TechnicalAnalyst::analyse(&bars, &reading, &self.cfg);

        let chain_keys = self.chain_keys(index, store, reading.spot);
        let chain = orderflow::chain_oi(&chain_keys, ticks, now_ms, self.cfg.max_tick_age_ms);
        let flow = OrderFlowAnalyst::analyse(&bars, spot_tick.as_ref(), chain, &self.cfg);

        let regime_view = regime_agent_view(&reading);

        let debate = DebateCoordinator::deliberate(vec![technical, flow, regime_view], &self.cfg);

        // ── Regime gate. Even a won debate does not trade into a bad regime. //
        let regime_allowed = reading.regime.allows_entry()
            || (self.cfg.allow_rangebound_entry && matches!(reading.regime, MarketRegime::Rangebound));
        if !regime_allowed {
            return stop(
                reading.clone(),
                debate,
                format!("regime {} does not permit a long-premium entry — {}", reading.regime.as_str(), reading.rationale),
            );
        }

        if !debate.actionable {
            let why = debate.summary.clone();
            return stop(reading, debate, why);
        }

        // ── Strike selection ────────────────────────────────────────────── //
        let direction = debate.stance.direction();
        let Some(records) = store.records.get(&index.symbol) else {
            return stop(reading, debate, format!("no scrip-master records for {}", index.symbol));
        };
        let refs: Vec<&ScripRecord> = records.iter().collect();
        let selection = strikes::select_strike(
            &refs, reading.spot, direction, index, ticks, &self.cfg,
            shared_domain::today_ist(), now_hm, now_ms,
        );

        let Some(contract) = selection.chosen.clone() else {
            let why = selection.reason.clone();
            return Decision {
                underlying: index.symbol.clone(),
                regime: reading, debate,
                selection: Some(selection), risk: None, signal: None,
                outcome: why,
                evaluated_at,
            };
        };

        // ── Risk, with final veto ───────────────────────────────────────── //
        let premium = contract.premium.unwrap_or(0.0);
        let risk = RiskManager::evaluate(
            &index.symbol, premium, contract.lot_size,
            open_positions, &self.risk, &self.cfg, max_trade_amount_inr, now_hm,
        );

        let RiskDecision::Approved { lots, .. } = &risk else {
            let why = risk.reason().to_string();
            return Decision {
                underlying: index.symbol.clone(),
                regime: reading, debate,
                selection: Some(selection), risk: Some(risk), signal: None,
                outcome: format!("risk manager declined: {why}"),
                evaluated_at,
            };
        };
        let lots = *lots;

        // ── Synthesise the exit plan, in option-premium terms ───────────── //
        let tick_size = contract.tick_size;
        let entry = round_tick(premium, tick_size);
        let stop_px = round_tick(premium * (1.0 - self.cfg.stop_loss_pct), tick_size);
        let t1 = round_tick(premium * (1.0 + self.cfg.target_1_pct), tick_size);
        let t2 = round_tick(premium * (1.0 + self.cfg.target_2_pct), tick_size);

        // A plan whose rounded levels no longer separate is not a plan. This can
        // happen on a low-premium contract where one tick swallows the risk
        // distance, and it would leave a position with a stop at or above entry.
        if !(stop_px < entry && t1 > entry && t2 > t1 && stop_px > 0.0) {
            return Decision {
                underlying: index.symbol.clone(),
                regime: reading, debate,
                selection: Some(selection), risk: Some(risk), signal: None,
                outcome: format!(
                    "exit levels collapsed after tick rounding (stop {stop_px}, entry {entry}, t1 {t1}, t2 {t2}) — no trade"
                ),
                evaluated_at,
            };
        }

        let reward_risk = (t1 - entry) / (entry - stop_px);
        if reward_risk < self.cfg.min_reward_risk {
            return Decision {
                underlying: index.symbol.clone(),
                regime: reading, debate,
                selection: Some(selection), risk: Some(risk), signal: None,
                outcome: format!("reward:risk {reward_risk:.2} is below the {:.2} minimum", self.cfg.min_reward_risk),
                evaluated_at,
            };
        }

        let signal = TradeSignal {
            instrument_name: index.symbol.clone(),
            strike: Some(contract.strike),
            option_type: Some(contract.option_type.clone()),
            expiry: Some(contract.expiry.format("%d-%b-%Y").to_string().to_uppercase()),
            // Options are only ever bought — there is no path here that sells.
            action: "BUY".into(),
            entry_condition: "ABOVE".into(),
            entry_price: entry,
            targets: vec![t1, t2],
            stop_loss: stop_px,
            source: "algo".into(),
            signal_id: Some(format!("algo-{}-{}", index.symbol, evaluated_at.replace([' ', ':', '-'], ""))),
            raw_message: Some(format!(
                "[{}] {} — {} | {} | {} lot(s)",
                debate.stance.as_str(), reading.regime.as_str(), debate.summary, selection.reason, lots
            )),
            // Second, independent guard against ever reaching the live path.
            paper_only: true,
        };

        Decision {
            underlying: index.symbol.clone(),
            regime: reading,
            debate,
            selection: Some(selection),
            risk: Some(risk),
            signal: Some(signal),
            outcome: format!("signal generated — {lots} lot(s), entry ₹{entry:.2}, stop ₹{stop_px:.2}, targets ₹{t1:.2}/₹{t2:.2}"),
            evaluated_at,
        }
    }

    /// Publish a decision's signal — **only** in PAPER mode.
    ///
    /// Returns `Ok(true)` when a signal was published, `Ok(false)` when the
    /// decision carried none, and `Err` with the reason when publishing was
    /// refused. Refusal is not a failure: in LIVE mode it is the expected and
    /// correct outcome.
    pub fn publish(
        &mut self,
        decision: &Decision,
        trading_cfg: &TradingConfig,
        signal_tx: &broadcast::Sender<TradeSignal>,
    ) -> Result<bool, String> {
        if !self.cfg.enabled {
            return Err("strategy engine is disabled".into());
        }
        // The gate. Anything other than an exact "PAPER" refuses.
        if trading_cfg.mode != "PAPER" {
            return Err(format!(
                "mode is {} — the algo publishes signals in PAPER mode only",
                trading_cfg.mode
            ));
        }
        let Some(signal) = decision.signal.clone() else { return Ok(false) };

        // Belt and braces: never publish a signal that lost its paper marking.
        if !signal.paper_only {
            return Err("refusing to publish an algo signal that is not marked paper_only".into());
        }

        signal_tx.send(signal).map_err(|e| format!("signal channel closed: {e}"))?;
        self.risk.entries_today += 1;
        Ok(true)
    }

    /// Record a decision for the dashboard.
    pub fn record(&mut self, decision: Decision) {
        self.decisions.insert(decision.underlying.clone(), decision);
    }

    /// Roll risk state if the session date changed.
    pub fn roll_session(&mut self) {
        self.risk.roll_session(&shared_domain::today_ist().to_string());
    }

    pub fn snapshot(&self, trading_cfg: &TradingConfig) -> StrategySnapshot {
        let publishing = self.cfg.enabled && trading_cfg.mode == "PAPER";
        let publish_block_reason = if !self.cfg.enabled {
            Some("strategy engine is disabled".into())
        } else if trading_cfg.mode != "PAPER" {
            Some(format!("mode is {} — algo signals publish in PAPER mode only", trading_cfg.mode))
        } else {
            None
        };

        let mut decisions: Vec<Decision> = self.decisions.values().cloned().collect();
        decisions.sort_by(|a, b| a.underlying.cmp(&b.underlying));

        StrategySnapshot {
            enabled: self.cfg.enabled,
            publishing,
            publish_block_reason,
            risk: self.risk.clone(),
            decisions,
            warmup: self.warmup_status(),
            config: self.cfg.clone(),
        }
    }
}

/// The regime analyst's vote, derived from the classification.
fn regime_agent_view(r: &RegimeReading) -> AgentView {
    match r.regime {
        MarketRegime::TrendingUp => AgentView {
            agent: "RegimeAnalyst".into(),
            stance: Stance::Bullish,
            // Confidence scales with how separated the ribbon is and how far
            // ATR sits above its own median.
            confidence: (55.0 + r.atr_percentile * 0.35).min(100.0),
            weight: REGIME_WEIGHT,
            evidence: vec![r.rationale.clone()],
        },
        MarketRegime::TrendingDown => AgentView {
            agent: "RegimeAnalyst".into(),
            stance: Stance::Bearish,
            confidence: (55.0 + r.atr_percentile * 0.35).min(100.0),
            weight: REGIME_WEIGHT,
            evidence: vec![r.rationale.clone()],
        },
        // Volatile-but-directionless, ranging, extended and unknown all decline
        // to take a side. The regime gate handles blocking; the vote abstains with weight 0.0.
        _ => AgentView::neutral("RegimeAnalyst", 0.0, r.rationale.clone()),
    }
}

/// Round a price down onto the contract's tick grid.
///
/// Deliberately identical to `monitor::round_down_tick`: rounding *down* is the
/// engine-wide agreed direction for every price — stops, targets and trailed
/// stops alike — and a strategy that rounded differently would place orders on
/// a grid the rest of the system does not expect.
///
/// Rounding down slightly widens the stop and slightly pulls in the target, so
/// it can only ever degrade the plan's reward:risk, never flatter it. That is
/// why the reward:risk check runs *after* rounding.
fn round_tick(price: f64, tick: f64) -> f64 {
    if !price.is_finite() || tick <= 0.0 {
        return price;
    }
    // The epsilon stops an exact multiple (0.15 / 0.05 = 2.9999999999999996 in
    // binary floating point) from being floored a whole tick too far.
    let steps = (price / tick + 1e-9).floor();
    (steps * tick * 100.0).round() / 100.0
}

fn current_hm() -> (u32, u32) {
    use chrono::Timelike;
    let now = shared_domain::now_ist();
    (now.hour(), now.minute())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dashmap::DashMap;
    use std::sync::Arc;

    fn trading_cfg(mode: &str) -> TradingConfig {
        TradingConfig {
            max_trade_amount_inr: 15_000.0,
            index_lots: 1, other_lots: 1,
            index_lots_by_symbol: Default::default(),
            mode: mode.into(),
            brokerage_per_order: 20.0,
            target_1_exit_pct: 50.0, target_2_exit_pct: 50.0,
            entry_market_protection: 5.0,
            dynamic_targeting: false,
        }
    }

    fn signal() -> TradeSignal {
        TradeSignal {
            instrument_name: "NIFTY".into(), strike: Some(24_050.0),
            option_type: Some("CE".into()), expiry: Some("27-AUG-2026".into()),
            action: "BUY".into(), entry_condition: "ABOVE".into(), entry_price: 120.0,
            targets: vec![162.0, 204.0], stop_loss: 84.0, source: "algo".into(),
            signal_id: Some("algo-test".into()), raw_message: None, paper_only: true,
        }
    }

    fn decision_with(sig: Option<TradeSignal>) -> Decision {
        let bars: Vec<Candle> = Vec::new();
        let cfg = StrategyConfig::default();
        Decision {
            underlying: "NIFTY".into(),
            regime: regime::classify(&bars, &cfg),
            debate: DebateCoordinator::deliberate(Vec::new(), &cfg),
            selection: None, risk: None,
            signal: sig,
            outcome: "test".into(),
            evaluated_at: "2026-08-25 11:00:00".into(),
        }
    }

    fn enabled_engine() -> StrategyEngine {
        StrategyEngine::new(StrategyConfig { enabled: true, ..Default::default() })
    }

    // ── The safety gate ─────────────────────────────────────────────────── //

    #[test]
    fn refuses_to_publish_in_live_mode() {
        let mut e = enabled_engine();
        let (tx, mut rx) = broadcast::channel(4);
        let err = e.publish(&decision_with(Some(signal())), &trading_cfg("LIVE"), &tx)
            .expect_err("LIVE mode must refuse to publish");
        assert!(err.contains("PAPER mode only"), "got: {err}");
        assert!(rx.try_recv().is_err(), "nothing may reach the order path in LIVE mode");
    }

    #[test]
    fn publishes_in_paper_mode() {
        let mut e = enabled_engine();
        let (tx, mut rx) = broadcast::channel(4);
        let sent = e.publish(&decision_with(Some(signal())), &trading_cfg("PAPER"), &tx).expect("should publish");
        assert!(sent);
        let got = rx.try_recv().expect("signal should be on the channel");
        assert!(got.paper_only, "every published algo signal must stay marked paper_only");
        assert_eq!(got.action, "BUY", "the engine must never emit a sell");
        assert_eq!(e.risk.entries_today, 1);
    }

    #[test]
    fn a_disabled_engine_publishes_nothing_even_in_paper_mode() {
        let mut e = StrategyEngine::new(StrategyConfig { enabled: false, ..Default::default() });
        let (tx, mut rx) = broadcast::channel(4);
        assert!(e.publish(&decision_with(Some(signal())), &trading_cfg("PAPER"), &tx).is_err());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn refuses_a_signal_that_lost_its_paper_marking() {
        let mut e = enabled_engine();
        let (tx, mut rx) = broadcast::channel(4);
        let mut s = signal();
        s.paper_only = false;
        let err = e.publish(&decision_with(Some(s)), &trading_cfg("PAPER"), &tx).expect_err("must refuse");
        assert!(err.contains("paper_only"), "got: {err}");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn unknown_modes_are_treated_as_not_paper() {
        let mut e = enabled_engine();
        let (tx, _rx) = broadcast::channel(4);
        for mode in ["live", "paper", "Paper", "", "BACKTEST"] {
            assert!(
                e.publish(&decision_with(Some(signal())), &trading_cfg(mode), &tx).is_err(),
                "mode {mode:?} must not be accepted as PAPER"
            );
        }
    }

    #[test]
    fn a_decision_with_no_signal_publishes_nothing_without_erroring() {
        let mut e = enabled_engine();
        let (tx, mut rx) = broadcast::channel(4);
        assert_eq!(e.publish(&decision_with(None), &trading_cfg("PAPER"), &tx), Ok(false));
        assert!(rx.try_recv().is_err());
        assert_eq!(e.risk.entries_today, 0, "a no-op must not consume the entry budget");
    }

    // ── Snapshot / state ────────────────────────────────────────────────── //

    #[test]
    fn snapshot_reports_why_publishing_is_blocked() {
        let e = enabled_engine();
        let s = e.snapshot(&trading_cfg("LIVE"));
        assert!(!s.publishing);
        assert!(s.publish_block_reason.unwrap().contains("PAPER mode only"));

        let s = e.snapshot(&trading_cfg("PAPER"));
        assert!(s.publishing);
        assert!(s.publish_block_reason.is_none());
    }

    #[test]
    fn stale_ticks_are_not_folded_into_bars() {
        let mut e = enabled_engine();
        let ticks: TickStore = Arc::new(DashMap::new());
        let mut t = shared_domain::MarketTick::new("nse_cm|Nifty 50");
        t.merge_from(&serde_json::json!({"ltp": 24000.0}), 0);
        ticks.insert("nse_cm|Nifty 50".into(), t);

        // Far beyond max_tick_age_ms — the price is no longer being confirmed.
        e.ingest_ticks(&ticks, 10_000_000);
        assert!(e.warmup_status().is_empty(), "a stale tick must not create bar history");
    }

    #[test]
    fn fresh_ticks_build_bars() {
        let mut e = enabled_engine();
        let ticks: TickStore = Arc::new(DashMap::new());
        let mut t = shared_domain::MarketTick::new("nse_cm|Nifty 50");
        t.merge_from(&serde_json::json!({"ltp": 24000.0}), 60_000);
        ticks.insert("nse_cm|Nifty 50".into(), t);

        e.ingest_ticks(&ticks, 60_000);
        assert_eq!(e.warmup_status(), vec![("nse_cm|Nifty 50".to_string(), 0)], "bar is open, none closed yet");
    }

    #[test]
    fn subscriptions_are_only_requested_once() {
        let mut e = enabled_engine();
        let keys = vec!["nse_fo|1".to_string(), "nse_fo|2".to_string()];
        assert_eq!(e.newly_subscribed(&keys).len(), 2);
        assert!(e.newly_subscribed(&keys).is_empty(), "already-subscribed keys must not be re-requested");
    }

    #[test]
    fn evaluation_without_a_spot_feed_stands_down() {
        let mut e = enabled_engine();
        let ticks: TickStore = Arc::new(DashMap::new());
        let store = ScripStore::default();
        let index = IndexSpec { symbol: "NIFTY".into(), spot_key: "nse_cm|Nifty 50".into(), strike_step: 50.0 };

        let d = e.evaluate_once(&index, &ticks, &store, &[], 15_000.0, 1_000);
        assert!(d.signal.is_none(), "no feed must never produce a signal");
        assert!(d.outcome.contains("stale or absent"), "got: {}", d.outcome);
    }

    #[test]
    fn tick_rounding_matches_the_engine_wide_round_down_convention() {
        // Exact multiples must survive binary-float error, not lose a tick.
        assert_eq!(round_tick(120.10, 0.05), 120.10);
        assert_eq!(round_tick(0.15, 0.05), 0.15);
        // Anything off the grid rounds down, as everywhere else in the engine.
        assert_eq!(round_tick(120.03, 0.05), 120.00);
        assert_eq!(round_tick(120.19, 0.05), 120.15);
        // Degenerate tick sizes pass through rather than dividing by zero.
        assert_eq!(round_tick(120.13, 0.0), 120.13);
    }

    #[test]
    fn allow_rangebound_entry_permits_rangebound_regime() {
        let regime = MarketRegime::Rangebound;
        assert!(!regime.allows_entry(), "Rangebound does not allow entry by default");

        let cfg_default = StrategyConfig { allow_rangebound_entry: false, ..Default::default() };
        let allowed_default = regime.allows_entry()
            || (cfg_default.allow_rangebound_entry && matches!(regime, MarketRegime::Rangebound));
        assert!(!allowed_default);

        let cfg_allowed = StrategyConfig { allow_rangebound_entry: true, ..Default::default() };
        let allowed_custom = regime.allows_entry()
            || (cfg_allowed.allow_rangebound_entry && matches!(regime, MarketRegime::Rangebound));
        assert!(allowed_custom, "allow_rangebound_entry must permit entry under Rangebound regime");
    }
}
