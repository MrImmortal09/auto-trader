//! `PATCH /api/v2/positions/:id` — a scalp's chart stop and target
//! (`shared_domain::ChartLevels`), behind the `chart_stops` switch.

use std::collections::BTreeSet;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Deserializer};
use serde_json::json;
use shared_domain::{ChartLevel, LevelTrigger, MonitoredPosition, TradeState};

use crate::routes::auth_kotak::send_log;
use crate::routes::positions::persist_positions_snapshot;
use crate::AppState;

/// `PATCH /api/v2/positions/:id` body. A level left out stays as it is;
/// `null` clears it; `{price, trigger}` sets it.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LevelsUpdate {
    #[serde(default, deserialize_with = "present")]
    pub(crate) stop: Option<Option<ChartLevel>>,
    #[serde(default, deserialize_with = "present")]
    pub(crate) target: Option<Option<ChartLevel>>,
}

/// Tells a field that is there, even as `null`, from one left out.
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<ChartLevel>>, D::Error> {
    Option::<ChartLevel>::deserialize(d).map(Some)
}

impl LevelsUpdate {
    fn sets_a_level(&self) -> bool {
        matches!(self.stop, Some(Some(_))) || matches!(self.target, Some(Some(_)))
    }

    fn new_levels(&self) -> impl Iterator<Item = &ChartLevel> {
        self.stop.iter().chain(self.target.iter()).flatten()
    }
}

/// Why `update` can't be applied to `pos`, as `(status, message)`, if it can't.
fn refusal(pos: &MonitoredPosition, update: &LevelsUpdate, chart_stops_on: bool) -> Option<(StatusCode, String)> {
    let conflict = |msg: &str| Some((StatusCode::CONFLICT, msg.to_string()));
    if update.sets_a_level() && !chart_stops_on {
        return conflict(
            "chart stops are switched off (/api/v2/flags) — levels already set stay until their position closes, and can still be cleared",
        );
    }
    if !pos.manual {
        return conflict("chart stops and targets are for scalps (Execute-screen positions) only");
    }
    if !matches!(pos.state, TradeState::Active) || pos.executed_qty <= 0 {
        return conflict("only an open scalp can have a chart stop or target");
    }
    if pos.live_halt.is_some() {
        return conflict("the engine has stopped acting on this position — square it off by hand");
    }
    if !matches!(pos.signal.option_type.as_deref(), Some("CE" | "PE")) {
        return conflict("only a CE or a PE can have chart levels");
    }
    for level in update.new_levels() {
        if !(level.price.is_finite() && level.price > 0.0) {
            return Some((StatusCode::BAD_REQUEST, "a level's price must be a positive number".to_string()));
        }
        if level.trigger == LevelTrigger::Index && shared_domain::kotak_index_feed(&pos.signal.instrument_name).is_none() {
            return Some((
                StatusCode::BAD_REQUEST,
                format!("{} has no index feed to trigger on", pos.signal.instrument_name),
            ));
        }
    }
    None
}

/// The bridge message that streams `instrument`'s index, if it is one of the six.
fn index_subscription(instrument: &str) -> Option<String> {
    let (segment, feed_name) = shared_domain::kotak_index_feed(instrument)?;
    Some(json!({ "action": "subscribe_indices", "indices": format!("{segment}|{feed_name}") }).to_string())
}

fn watches_index(pos: &MonitoredPosition) -> bool {
    pos.chart.stop.iter().chain(pos.chart.target.iter()).any(|l| l.trigger == LevelTrigger::Index)
}

/// Bridge messages that stream the index of every position whose chart levels
/// watch it — sent when the feed starts, so a restart or a re-login keeps
/// those levels live.
pub fn chart_index_subscriptions(positions: &[MonitoredPosition]) -> Vec<String> {
    let indices: BTreeSet<&str> = positions
        .iter()
        .filter(|p| watches_index(p))
        .map(|p| p.signal.instrument_name.as_str())
        .collect();
    indices.into_iter().filter_map(index_subscription).collect()
}

fn live_price(state: &AppState, key: &str) -> Option<f64> {
    state.prices.get(key).map(|p| *p).filter(|p| *p > 0.0)
}

/// `PATCH /api/v2/positions/:id` `{stop?, target?}` — set, clear or keep a
/// scalp's chart stop and target. A level is `{price, trigger}` with
/// `trigger` `"premium"` (the option's price) or `"index"` (its index, which
/// fires in the direction the option loses or gains). One already crossed is
/// taken as asked, and the engine sells the whole holding on its next tick:
/// `sells_now` says so.
pub async fn patch_position_levels_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(update): Json<LevelsUpdate>,
) -> Response {
    let chart_stops_on = state.flags.read().await.chart_stops;
    let mut positions = state.positions.write().await;
    let Some(pos) = positions.iter_mut().find(|p| p.id == id) else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response();
    };
    if let Some((status, msg)) = refusal(pos, &update, chart_stops_on) {
        return (status, Json(json!({ "error": msg }))).into_response();
    }

    let before = pos.chart;
    if let Some(stop) = update.stop {
        pos.chart.stop = stop;
    }
    if let Some(target) = update.target {
        pos.chart.target = target;
    }
    let chart = pos.chart;
    let instrument = pos.signal.instrument_name.clone();
    let premium = pos.ws_scrip_key.as_deref().and_then(|k| live_price(&state, k));
    let index = shared_domain::kotak_index_feed(&instrument)
        .and_then(|(_, name)| live_price(&state, &shared_domain::index_price_key(name)));
    let sells_now = pos.chart_level_hit(premium, index);
    let subscription = watches_index(pos).then(|| index_subscription(&instrument)).flatten();
    let snapshot = positions.clone();
    drop(positions);

    if let Some(msg) = subscription {
        if let Some(tx) = state.ws_tx.lock().await.as_ref() {
            let _ = tx.send(msg);
        }
    }
    if chart != before {
        persist_positions_snapshot(&state, &snapshot).await;
        let message = match sells_now {
            Some(reason) => format!("Chart levels set — already reached ({reason}): selling the whole holding"),
            None => "Chart levels set".to_string(),
        };
        send_log(&state.db_tx, &state.log_tx, if sells_now.is_some() { "WARN" } else { "INFO" }, json!({
            "event": "CHART_LEVELS_SET",
            "level": if sells_now.is_some() { "WARN" } else { "INFO" },
            "message": message,
            "instrument": instrument,
            "position_id": id,
            "chart": chart,
        }))
        .await;
    }

    Json(json!({ "id": id, "chart": chart, "sells_now": sells_now.is_some() })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared_domain::{ChartLevels, TradeSignal};

    fn scalp() -> MonitoredPosition {
        MonitoredPosition {
            id: "p1".into(),
            signal: TradeSignal {
                instrument_name: "NIFTY".into(),
                strike: Some(25_100.0),
                option_type: Some("CE".into()),
                action: "BUY".into(),
                ..Default::default()
            },
            state: TradeState::Active,
            executed_qty: 65,
            manual: true,
            ..Default::default()
        }
    }

    fn parse(body: &str) -> LevelsUpdate {
        serde_json::from_str(body).unwrap()
    }

    #[test]
    fn a_level_left_out_is_kept_and_null_clears_it() {
        let update = parse(r#"{"stop": {"price": 25060, "trigger": "index"}}"#);
        assert_eq!(update.stop, Some(Some(ChartLevel { price: 25_060.0, trigger: LevelTrigger::Index })));
        assert_eq!(update.target, None);
        let update = parse(r#"{"target": null}"#);
        assert_eq!(update.target, Some(None));
        assert!(!update.sets_a_level());
        assert!(serde_json::from_str::<LevelsUpdate>(r#"{"sl": 1}"#).is_err());
        assert!(serde_json::from_str::<LevelsUpdate>(r#"{"stop": {"price": 1, "trigger": "spot"}}"#).is_err());
    }

    #[test]
    fn only_open_scalps_take_levels_and_only_while_the_switch_is_on() {
        let set = parse(r#"{"stop": {"price": 92.5, "trigger": "premium"}}"#);
        let clear = parse(r#"{"stop": null}"#);
        assert_eq!(refusal(&scalp(), &set, true), None);
        // Switched off: nothing new can be set, but clearing still works.
        assert_eq!(refusal(&scalp(), &set, false).map(|r| r.0), Some(StatusCode::CONFLICT));
        assert_eq!(refusal(&scalp(), &clear, false), None);

        let mut telegram = scalp();
        telegram.manual = false;
        let mut waiting = scalp();
        waiting.state = TradeState::WaitingForEntry;
        let mut halted = scalp();
        halted.live_halt = Some("exit failed 3 times".into());
        for pos in [telegram, waiting, halted] {
            assert_eq!(refusal(&pos, &set, true).map(|r| r.0), Some(StatusCode::CONFLICT));
        }
        let bad_price = parse(r#"{"target": {"price": -1, "trigger": "premium"}}"#);
        assert_eq!(refusal(&scalp(), &bad_price, true).map(|r| r.0), Some(StatusCode::BAD_REQUEST));
    }

    #[test]
    fn index_levels_subscribe_their_index_once() {
        let mut a = scalp();
        a.chart = ChartLevels { stop: Some(ChartLevel { price: 25_060.0, trigger: LevelTrigger::Index }), target: None };
        let b = a.clone();
        let mut premium_only = scalp();
        premium_only.chart.stop = Some(ChartLevel { price: 92.5, trigger: LevelTrigger::Premium });
        let messages = chart_index_subscriptions(&[a, b, premium_only, scalp()]);
        assert_eq!(messages, [r#"{"action":"subscribe_indices","indices":"nse_cm|Nifty 50"}"#]);
    }
}
