//! `PATCH /api/v2/positions/:id` — a scalp's chart stop and targets
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
use shared_domain::{ChartHit, ChartLevel, ChartLevels, LevelRefusal, LevelTrigger, MonitoredPosition, TargetEdit, TradeState};

use crate::routes::auth_kotak::send_log;
use crate::routes::positions::persist_positions_snapshot;
use crate::AppState;

/// `PATCH /api/v2/positions/:id` body; anything left out stays as it is.
/// `stop`: `null` clears it, `{price, trigger}` sets it. `set_target`: one
/// target added (no `id`) or moved and resized (its `id`), with `lots` —
/// `null` for "all left" (see [`TargetEdit`]). `remove_target`: a target's id.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LevelsUpdate {
    #[serde(default, deserialize_with = "present")]
    pub(crate) stop: Option<Option<ChartLevel>>,
    #[serde(default)]
    pub(crate) set_target: Option<TargetEdit>,
    #[serde(default)]
    pub(crate) remove_target: Option<String>,
}

/// Tells a field that is there, even as `null`, from one left out.
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<ChartLevel>>, D::Error> {
    Option::<ChartLevel>::deserialize(d).map(Some)
}

impl LevelsUpdate {
    fn sets_a_level(&self) -> bool {
        matches!(self.stop, Some(Some(_))) || self.set_target.is_some()
    }

    pub(crate) fn new_levels(&self) -> impl Iterator<Item = ChartLevel> + '_ {
        let target = self.set_target.iter().map(|t| ChartLevel { price: t.price, trigger: t.trigger });
        self.stop.iter().flatten().copied().chain(target)
    }

    /// Apply it to `chart`, on a position with `free_lots` held and not
    /// already being sold — all or nothing. The id of the target it set, if
    /// it set one.
    pub(crate) fn apply(&self, chart: &mut ChartLevels, free_lots: i32) -> Result<Option<String>, LevelRefusal> {
        if self.stop.iter().flatten().any(|l| !(l.price.is_finite() && l.price > 0.0)) {
            return Err(LevelRefusal::BadRequest("a level's price must be a positive number".to_string()));
        }
        let mut next = chart.clone();
        if let Some(stop) = self.stop {
            next.stop = stop;
        }
        if let Some(id) = &self.remove_target {
            next.remove_target(id);
        }
        let set = match &self.set_target {
            Some(edit) => Some(next.set_target(edit, free_lots)?),
            None => None,
        };
        *chart = next;
        Ok(set)
    }
}

/// A [`LevelRefusal`] as the API answers it.
pub(crate) fn level_refusal(r: LevelRefusal) -> (StatusCode, String) {
    match r {
        LevelRefusal::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
        LevelRefusal::Conflict(msg) => (StatusCode::CONFLICT, msg),
    }
}

/// What the answer says a just-set level sells at once: `(sells_now,
/// sells_lots)`, the lots `null` for the whole holding.
pub(crate) fn sells_now(hit: Option<&ChartHit>) -> (bool, Option<i32>) {
    match hit {
        Some(ChartHit::All { .. }) => (true, None),
        Some(ChartHit::Part { lots, .. }) => (true, Some(*lots)),
        None => (false, None),
    }
}

fn lot_of(pos: &MonitoredPosition) -> i32 {
    pos.resolved_order.as_ref().and_then(|o| o.quantity.parse::<i32>().ok()).filter(|v| *v > 0).unwrap_or(1)
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
    pos.chart.levels().chain(pos.chart_targets_selling.iter().map(|t| ChartLevel { price: t.price, trigger: t.trigger })).any(|l| l.trigger == LevelTrigger::Index)
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

/// `PATCH /api/v2/positions/:id` `{stop?, set_target?, remove_target?}` —
/// set, clear or keep a scalp's chart stop, and add, move, resize or remove
/// one of its targets. A level is `{price, trigger}` with `trigger`
/// `"premium"` (the option's price) or `"index"` (its index, which fires in
/// the direction the option loses or gains). A target's lots may only use
/// what the other targets leave of the holding. One already crossed is taken
/// as asked, and the engine sells on its next tick — the whole holding for
/// the stop, a target's lots for a target: `sells_now` and `sells_lots`
/// (`null` = everything) say so. `target_id` names a target just set.
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

    let before = pos.chart.clone();
    let lot = lot_of(pos);
    // Lots a sell already at the broker is taking are not for a new target.
    let free_lots = (pos.executed_qty - pos.pending_exit_qty).max(0) / lot;
    let target_id = match update.apply(&mut pos.chart, free_lots) {
        Ok(set) => set,
        Err(r) => {
            let (status, msg) = level_refusal(r);
            return (status, Json(json!({ "error": msg }))).into_response();
        }
    };
    let chart = pos.chart.clone();
    let instrument = pos.signal.instrument_name.clone();
    let premium = pos.ws_scrip_key.as_deref().and_then(|k| live_price(&state, k));
    let index = shared_domain::kotak_index_feed(&instrument)
        .and_then(|(_, name)| live_price(&state, &shared_domain::index_price_key(name)));
    let hit = pos.chart_hit(premium, index, lot);
    let subscription = watches_index(pos).then(|| index_subscription(&instrument)).flatten();
    let snapshot = positions.clone();
    drop(positions);

    if let Some(msg) = subscription {
        if let Some(tx) = state.ws_tx.lock().await.as_ref() {
            let _ = tx.send(msg);
        }
    }
    let (sells, sells_lots) = sells_now(hit.as_ref());
    if chart != before {
        persist_positions_snapshot(&state, &snapshot).await;
        let message = match &hit {
            Some(ChartHit::All { reason }) => format!("Chart levels set — already reached ({reason}): selling the whole holding"),
            Some(ChartHit::Part { lots, .. }) => format!("Chart levels set — a target already reached: selling {lots} lot(s)"),
            None => "Chart levels set".to_string(),
        };
        send_log(&state.db_tx, &state.log_tx, if sells { "WARN" } else { "INFO" }, json!({
            "event": "CHART_LEVELS_SET",
            "level": if sells { "WARN" } else { "INFO" },
            "message": message,
            "instrument": instrument,
            "position_id": id,
            "chart": chart,
        }))
        .await;
    }

    Json(json!({ "id": id, "chart": chart, "sells_now": sells, "sells_lots": sells_lots, "target_id": target_id })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared_domain::{ChartTarget, TradeSignal};

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
        assert_eq!(update.set_target, None);
        let update = parse(r#"{"stop": null, "remove_target": "a1"}"#);
        assert_eq!(update.stop, Some(None));
        assert!(!update.sets_a_level());
        assert!(serde_json::from_str::<LevelsUpdate>(r#"{"sl": 1}"#).is_err());
        assert!(serde_json::from_str::<LevelsUpdate>(r#"{"stop": {"price": 1, "trigger": "spot"}}"#).is_err());
        // The one-target body from before targets had lots is refused, never read as a new target.
        assert!(serde_json::from_str::<LevelsUpdate>(r#"{"target": {"price": 140, "trigger": "premium"}}"#).is_err());
    }

    #[test]
    fn a_target_body_says_which_target_and_how_many_lots() {
        let add = parse(r#"{"set_target": {"price": 140, "trigger": "premium", "lots": 2}}"#);
        assert_eq!(add.set_target, Some(TargetEdit { id: None, price: 140.0, trigger: LevelTrigger::Premium, lots: Some(Some(2)) }));
        let all_left = parse(r#"{"set_target": {"price": 140, "trigger": "premium", "lots": null}}"#);
        assert_eq!(all_left.set_target.unwrap().lots, Some(None));
        let moved = parse(r#"{"set_target": {"id": "a1", "price": 141, "trigger": "premium"}}"#);
        assert_eq!(moved.set_target.unwrap().lots, None);
        assert!(serde_json::from_str::<LevelsUpdate>(r#"{"set_target": {"price": 140, "trigger": "premium", "qty": 2}}"#).is_err());
    }

    #[test]
    fn an_update_applies_whole_or_not_at_all() {
        let mut chart = ChartLevels {
            stop: None,
            targets: vec![ChartTarget { id: "a1".into(), price: 130.0, trigger: LevelTrigger::Premium, lots: Some(2) }],
        };
        // 3 lots free: 2 more would make 4 — refused, and the stop with it.
        let over = parse(r#"{"stop": {"price": 90, "trigger": "premium"}, "set_target": {"price": 140, "trigger": "premium", "lots": 2}}"#);
        assert_eq!(over.apply(&mut chart, 3).map_err(level_refusal).unwrap_err().0, StatusCode::CONFLICT);
        assert_eq!(chart.stop, None);
        assert_eq!(chart.targets.len(), 1);
        // Removing one and adding another in the same update frees its lots first.
        let swap = parse(r#"{"remove_target": "a1", "set_target": {"price": 140, "trigger": "premium", "lots": 3}}"#);
        let id = swap.apply(&mut chart, 3).unwrap().unwrap();
        assert_eq!(chart.targets.iter().map(|t| (t.id.as_str(), t.lots)).collect::<Vec<_>>(), [(id.as_str(), Some(3))]);
    }

    #[test]
    fn only_open_scalps_take_levels_and_only_while_the_switch_is_on() {
        let set = parse(r#"{"stop": {"price": 92.5, "trigger": "premium"}}"#);
        let clear = parse(r#"{"stop": null}"#);
        let add = parse(r#"{"set_target": {"price": 140, "trigger": "premium", "lots": 1}}"#);
        let remove = parse(r#"{"remove_target": "a1"}"#);
        assert_eq!(refusal(&scalp(), &set, true), None);
        assert_eq!(refusal(&scalp(), &add, true), None);
        // Switched off: nothing new can be set, but clearing still works.
        assert_eq!(refusal(&scalp(), &set, false).map(|r| r.0), Some(StatusCode::CONFLICT));
        assert_eq!(refusal(&scalp(), &add, false).map(|r| r.0), Some(StatusCode::CONFLICT));
        assert_eq!(refusal(&scalp(), &clear, false), None);
        assert_eq!(refusal(&scalp(), &remove, false), None);

        let mut telegram = scalp();
        telegram.manual = false;
        let mut waiting = scalp();
        waiting.state = TradeState::WaitingForEntry;
        let mut halted = scalp();
        halted.live_halt = Some("exit failed 3 times".into());
        for pos in [telegram, waiting, halted] {
            assert_eq!(refusal(&pos, &set, true).map(|r| r.0), Some(StatusCode::CONFLICT));
        }
        let bad_price = parse(r#"{"set_target": {"price": -1, "trigger": "premium"}}"#);
        assert_eq!(refusal(&scalp(), &bad_price, true).map(|r| r.0), Some(StatusCode::BAD_REQUEST));
    }

    #[test]
    fn index_levels_subscribe_their_index_once() {
        let mut a = scalp();
        a.chart = ChartLevels { stop: Some(ChartLevel { price: 25_060.0, trigger: LevelTrigger::Index }), targets: vec![] };
        let b = a.clone();
        let mut premium_only = scalp();
        premium_only.chart.stop = Some(ChartLevel { price: 92.5, trigger: LevelTrigger::Premium });
        let messages = chart_index_subscriptions(&[a, b, premium_only, scalp()]);
        assert_eq!(messages, [r#"{"action":"subscribe_indices","indices":"nse_cm|Nifty 50"}"#]);
        // A target on the index subscribes it too.
        let mut target = scalp();
        target.chart.targets.push(ChartTarget { id: "a1".into(), price: 25_180.0, trigger: LevelTrigger::Index, lots: None });
        assert_eq!(chart_index_subscriptions(&[target]).len(), 1);
    }
}
