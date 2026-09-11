//! Bull-vs-bear debate and consensus scoring.
//!
//! Each analyst's view is assigned to the bull or the bear side and scored as
//! `weight × confidence`. Both sides are then normalised against the *total*
//! weight in the room — including agents that abstained — so an abstention
//! genuinely dilutes conviction rather than being silently excluded from the
//! denominator. Two agents agreeing while three abstain is not a 100% consensus.
//!
//! A trade requires **both**:
//!
//! - the winning side's score to clear [`StrategyConfig::conviction_threshold`], and
//! - the winner to beat the loser by at least [`StrategyConfig::min_debate_margin`].
//!
//! The margin requirement guards against a split decision — a market the agents
//! genuinely disagree about, where taking a directional long-premium position is
//! how a strategy pays theta to be wrong slowly.
//!
//! # Why the margin check looks redundant at the default threshold
//!
//! Because every agent contributes to at most one side and both sides share the
//! same denominator, `bull_score + bear_score ≤ 100`. So a winner scoring 75
//! forces the loser to 25 or below, and the margin is already ≥ 50 — the margin
//! test cannot fail while `conviction_threshold` is 75.
//!
//! It is kept deliberately. The threshold is operator-tunable, and the margin
//! check is what stops a lowered threshold (say 40) from letting a 45-vs-40
//! near-tie trade. It is a guard against a future configuration, not dead code
//! under the current one.

use serde::{Deserialize, Serialize};

use super::{AgentView, Stance};
use crate::strategy::config::StrategyConfig;

pub struct DebateCoordinator;

/// The result of one debate round, retained in full for the dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebateOutcome {
    /// Every view that participated, abstentions included.
    pub views: Vec<AgentView>,
    /// Normalised bull score, 0–100.
    pub bull_score: f64,
    /// Normalised bear score, 0–100.
    pub bear_score: f64,
    /// The winning side's score.
    pub conviction: f64,
    /// How decisively the winner beat the loser.
    pub margin: f64,
    /// Direction the debate settled on, or `Neutral` when it did not settle.
    pub stance: Stance,
    /// Whether both the threshold and margin tests passed.
    pub actionable: bool,
    /// Plain-language summary of the outcome.
    pub summary: String,
}

impl DebateCoordinator {
    /// Run one debate round over the supplied views.
    pub fn deliberate(views: Vec<AgentView>, cfg: &StrategyConfig) -> DebateOutcome {
        // Total weight includes abstainers, so silence dilutes conviction.
        let total_weight: f64 = views.iter().map(|v| v.weight).sum();

        if total_weight <= 0.0 || views.is_empty() {
            return DebateOutcome {
                views,
                bull_score: 0.0, bear_score: 0.0, conviction: 0.0, margin: 0.0,
                stance: Stance::Neutral,
                actionable: false,
                summary: "no agents participated — standing down".into(),
            };
        }

        let side_score = |want: Stance| -> f64 {
            views
                .iter()
                .filter(|v| v.stance == want)
                .map(|v| v.weight * v.clamped_confidence())
                .sum::<f64>()
                / total_weight
        };

        let bull_score = side_score(Stance::Bullish);
        let bear_score = side_score(Stance::Bearish);

        let (stance, conviction, loser) = if bull_score > bear_score {
            (Stance::Bullish, bull_score, bear_score)
        } else if bear_score > bull_score {
            (Stance::Bearish, bear_score, bull_score)
        } else {
            (Stance::Neutral, 0.0, 0.0)
        };
        let margin = conviction - loser;

        let clears_threshold = conviction >= cfg.conviction_threshold;
        let clears_margin = margin >= cfg.min_debate_margin;
        let actionable = stance != Stance::Neutral && clears_threshold && clears_margin;

        let abstained: Vec<&str> = views
            .iter()
            .filter(|v| v.stance == Stance::Neutral)
            .map(|v| v.agent.as_str())
            .collect();

        let summary = if actionable {
            format!(
                "{} wins {conviction:.0} vs {loser:.0} (margin {margin:.0}) — clears the {:.0} threshold",
                stance.as_str(), cfg.conviction_threshold
            )
        } else if stance == Stance::Neutral {
            format!("bull {bull_score:.0} vs bear {bear_score:.0} — no directional consensus")
        } else if !clears_threshold {
            format!(
                "{} leads {conviction:.0} vs {loser:.0} but is short of the {:.0} conviction threshold{}",
                stance.as_str(), cfg.conviction_threshold,
                if abstained.is_empty() { String::new() } else { format!(" ({} abstained)", abstained.join(", ")) }
            )
        } else {
            format!(
                "{} leads {conviction:.0} vs {loser:.0} but the {margin:.0} margin is under the {:.0} minimum — too close to call",
                stance.as_str(), cfg.min_debate_margin
            )
        };

        DebateOutcome { views, bull_score, bear_score, conviction, margin, stance, actionable, summary }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> StrategyConfig {
        StrategyConfig { conviction_threshold: 75.0, min_debate_margin: 15.0, ..Default::default() }
    }

    fn view(agent: &str, stance: Stance, confidence: f64, weight: f64) -> AgentView {
        AgentView { agent: agent.into(), stance, confidence, weight, evidence: vec![] }
    }

    #[test]
    fn unanimous_high_confidence_is_actionable() {
        let o = DebateCoordinator::deliberate(
            vec![
                view("Tech", Stance::Bullish, 90.0, 1.4),
                view("Flow", Stance::Bullish, 85.0, 1.0),
                view("Regime", Stance::Bullish, 88.0, 1.2),
            ],
            &cfg(),
        );
        assert!(o.actionable, "{}", o.summary);
        assert_eq!(o.stance, Stance::Bullish);
        assert!(o.conviction >= 75.0);
    }

    #[test]
    fn a_near_tie_is_refused_at_the_default_threshold() {
        // Two agents disagreeing at similar confidence. Neither side can reach
        // 75 once they split the same denominator, so this is refused on the
        // conviction threshold.
        let o = DebateCoordinator::deliberate(
            vec![
                view("Tech", Stance::Bullish, 78.0, 1.0),
                view("Flow", Stance::Bearish, 76.0, 1.0),
            ],
            &cfg(),
        );
        assert!(!o.actionable, "a coin flip must not trade: {}", o.summary);
        assert!((o.bull_score - 39.0).abs() < 1e-9, "got {}", o.bull_score);
        assert!((o.bear_score - 38.0).abs() < 1e-9, "got {}", o.bear_score);
    }

    #[test]
    fn the_margin_check_bites_when_the_threshold_is_lowered() {
        // The margin test is unreachable at threshold 75 (see the module note),
        // so it is exercised where it actually applies: an operator who lowers
        // the conviction bar must still not get a near-tie trade.
        let lenient = StrategyConfig { conviction_threshold: 35.0, min_debate_margin: 15.0, ..Default::default() };
        let o = DebateCoordinator::deliberate(
            vec![
                view("Tech", Stance::Bullish, 78.0, 1.0),
                view("Flow", Stance::Bearish, 76.0, 1.0),
            ],
            &lenient,
        );
        assert!(o.conviction >= lenient.conviction_threshold, "threshold is cleared at {}", o.conviction);
        assert!(!o.actionable, "but a 1-point margin must still refuse: {}", o.summary);
        assert!(o.summary.contains("too close to call"), "got: {}", o.summary);
    }

    #[test]
    fn scores_never_sum_above_one_hundred() {
        // The property the margin analysis rests on.
        let o = DebateCoordinator::deliberate(
            vec![
                view("A", Stance::Bullish, 100.0, 1.4),
                view("B", Stance::Bearish, 100.0, 1.0),
                view("C", Stance::Bullish, 100.0, 1.2),
            ],
            &cfg(),
        );
        assert!(o.bull_score + o.bear_score <= 100.0 + 1e-9, "got {} + {}", o.bull_score, o.bear_score);
    }

    #[test]
    fn abstentions_dilute_conviction() {
        // One loud bull, two abstainers. Without dilution this would read 90.
        let o = DebateCoordinator::deliberate(
            vec![
                view("Tech", Stance::Bullish, 90.0, 1.0),
                view("Flow", Stance::Neutral, 0.0, 1.0),
                view("Regime", Stance::Neutral, 0.0, 1.0),
            ],
            &cfg(),
        );
        assert!((o.conviction - 30.0).abs() < 1e-9, "expected 90/3 = 30, got {}", o.conviction);
        assert!(!o.actionable, "one agent out of three is not a consensus");
    }

    #[test]
    fn a_dead_tie_produces_no_direction() {
        let o = DebateCoordinator::deliberate(
            vec![
                view("Tech", Stance::Bullish, 80.0, 1.0),
                view("Flow", Stance::Bearish, 80.0, 1.0),
            ],
            &cfg(),
        );
        assert_eq!(o.stance, Stance::Neutral);
        assert!(!o.actionable);
        assert!(o.summary.contains("no directional consensus"));
    }

    #[test]
    fn all_abstaining_is_not_actionable() {
        let o = DebateCoordinator::deliberate(
            vec![
                view("Tech", Stance::Neutral, 0.0, 1.0),
                view("Flow", Stance::Neutral, 0.0, 1.0),
            ],
            &cfg(),
        );
        assert!(!o.actionable);
        assert_eq!(o.conviction, 0.0);
    }

    #[test]
    fn empty_room_stands_down() {
        let o = DebateCoordinator::deliberate(vec![], &cfg());
        assert!(!o.actionable);
        assert!(o.summary.contains("standing down"));
    }

    #[test]
    fn weight_gives_the_technical_agent_more_pull() {
        // Equal confidence, unequal weight — the heavier agent should win.
        let o = DebateCoordinator::deliberate(
            vec![
                view("Tech", Stance::Bullish, 80.0, 1.4),
                view("Flow", Stance::Bearish, 80.0, 1.0),
            ],
            &cfg(),
        );
        assert_eq!(o.stance, Stance::Bullish);
        assert!(o.bull_score > o.bear_score);
    }

    #[test]
    fn runaway_confidence_cannot_manufacture_consensus() {
        // A miscomputed 10_000 confidence must be clamped to 100 before scoring.
        let o = DebateCoordinator::deliberate(
            vec![
                view("Rogue", Stance::Bullish, 10_000.0, 1.0),
                view("Flow", Stance::Neutral, 0.0, 1.0),
                view("Regime", Stance::Neutral, 0.0, 1.0),
            ],
            &cfg(),
        );
        assert!(o.conviction <= 100.0);
        assert!(!o.actionable, "clamping must keep one rogue agent from forcing a trade");
    }

    #[test]
    fn threshold_shortfall_names_the_abstainers() {
        let o = DebateCoordinator::deliberate(
            vec![
                view("Tech", Stance::Bullish, 100.0, 1.0),
                view("Flow", Stance::Neutral, 0.0, 1.0),
            ],
            &cfg(),
        );
        assert!(!o.actionable);
        assert!(o.summary.contains("Flow"), "the log should say who stayed silent: {}", o.summary);
    }

    #[test]
    fn two_active_agents_agreeing_without_orderflow_is_actionable() {
        // When OrderFlowAnalyst has weight 0.0 due to lack of feed data,
        // Tech + Regime agreeing on a trend clears consensus without phantom dilution.
        let o = DebateCoordinator::deliberate(
            vec![
                view("Tech", Stance::Bullish, 85.0, 1.4),
                view("Flow", Stance::Neutral, 0.0, 0.0),
                view("Regime", Stance::Bullish, 75.0, 1.2),
            ],
            &cfg(),
        );
        assert!(o.actionable, "{}", o.summary);
        assert_eq!(o.stance, Stance::Bullish);
        assert!(o.conviction >= 75.0, "got {}", o.conviction);
    }

    #[test]
    fn single_confident_agent_can_trade_when_others_abstain_with_zero_weight() {
        // When other agents abstain with weight 0.0, a confident technical view
        // is actionable without denominator dilution.
        let default_cfg = StrategyConfig::default(); // conviction_threshold: 45.0
        let o = DebateCoordinator::deliberate(
            vec![
                view("Tech", Stance::Bullish, 80.0, 1.4),
                view("Flow", Stance::Neutral, 0.0, 0.0),
                view("Regime", Stance::Neutral, 0.0, 0.0),
            ],
            &default_cfg,
        );
        assert!(o.actionable, "{}", o.summary);
        assert_eq!(o.conviction, 80.0);
    }
}
