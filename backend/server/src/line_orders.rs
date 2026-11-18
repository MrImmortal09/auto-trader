//! Line orders: a scalp bought when its index crosses a buy line, with its
//! stop and targets set at the same time — for the owner's engine
//! (`POST /api/execute/line-buy`) and a user's paper book
//! (`POST /api/v2/me/line-buy`) alike.
//!
//! The owner's rules (2026-11-18): the order is sent only once the buy line,
//! the stop and every target are placed, as one request; every level is on
//! the index, a call's stop below the line and its targets above (a put the
//! other way round); and a line the index has already crossed by then is
//! refused — placed again, never bought at once. Once armed, it buys at
//! market the moment the index reaches the line from the side it was armed
//! on, unless its validity ran out first.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use shared_domain::{ChartLevel, ChartLevels, ChartTarget, EntryLine, LevelRefusal, LevelTrigger};

use crate::execute;

/// Where the buy line goes and which way the index crosses it.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LinePick {
    pub price: f64,
    /// `true`: the index is below the line and buys as it rises to it;
    /// `false`: above it, and buys as it falls to it — as the chart saw it.
    pub rising: bool,
}

/// One target of a line order: its level on the index and the lots it sells.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineTarget {
    pub price: f64,
    pub lots: i32,
}

/// `POST …/line-buy` body: the contract and lots, the buy line, how many
/// minutes it stays armed (`null`: till close), the stop and the targets,
/// every level on the contract's index.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineBuyReq {
    pub trading_symbol: String,
    pub lots: i32,
    pub line: LinePick,
    #[serde(default)]
    pub valid_minutes: Option<i32>,
    pub stop: f64,
    pub targets: Vec<LineTarget>,
}

impl LineBuyReq {
    /// The stop and targets as chart levels on the index, each target with
    /// an id of its own.
    pub fn levels(&self) -> ChartLevels {
        ChartLevels {
            stop: Some(ChartLevel { price: self.stop, trigger: LevelTrigger::Index }),
            targets: self
                .targets
                .iter()
                .map(|t| ChartTarget { id: ChartTarget::new_id(), price: t.price, trigger: LevelTrigger::Index, lots: Some(t.lots) })
                .collect(),
        }
    }
}

/// A refusal as `(status, message, code)`; `code` lets the chart tell a
/// crossed line (placed again) from any other refusal.
pub type Refused = (StatusCode, String, Option<&'static str>);

/// The code a refusal for a line the index has already crossed carries.
pub const CROSSED: &str = "line_crossed";

/// A refusal as the API answers it: `{error}`, with `code` when it has one.
pub fn refused_response((status, msg, code): Refused) -> Response {
    let mut body = serde_json::json!({ "error": msg });
    if let Some(code) = code {
        body["code"] = serde_json::json!(code);
    }
    (status, Json(body)).into_response()
}

pub fn refused(r: LevelRefusal) -> Refused {
    match r {
        LevelRefusal::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg, None),
        LevelRefusal::Conflict(msg) => (StatusCode::CONFLICT, msg, None),
    }
}

/// The price-map key `underlying`'s value ticks under — one of the six
/// indices only: a buy line is on an index.
pub fn index_key(underlying: &str) -> Option<String> {
    shared_domain::kotak_index_feed(underlying).map(|(_, name)| shared_domain::index_price_key(name))
}

/// `underlying`'s value now, fresh enough to arm a line against.
pub fn index_now(prices: &DashMap<String, f64>, underlying: &str) -> Result<f64, Refused> {
    let Some(key) = index_key(underlying) else {
        return Err((StatusCode::BAD_REQUEST, format!("{underlying} can't be bought on a line — line orders are for the six indices"), None));
    };
    let Some(value) = prices.get(&key).map(|v| *v).filter(|v| *v > 0.0) else {
        return Err((StatusCode::CONFLICT, format!("No live {underlying} value yet — a buy line can't be armed blind"), None));
    };
    if execute::contract_age_secs(&key).is_none_or(|age| age > execute::MAX_CONTRACT_AGE_SECS) {
        return Err((
            StatusCode::CONFLICT,
            format!("{underlying}'s value hasn't updated in over {:.0} s — a buy line can't be armed blind", execute::MAX_CONTRACT_AGE_SECS),
            None,
        ));
    }
    Ok(value)
}

/// Refuse a line the index at `index` has already crossed from the side
/// the chart saw it on: the owner places it again, nothing is bought at once.
pub fn check_not_crossed(underlying: &str, pick: LinePick, index: f64) -> Result<(), Refused> {
    let crossed = if pick.rising { index >= pick.price } else { index <= pick.price };
    if crossed {
        return Err((
            StatusCode::CONFLICT,
            format!("{underlying} is already at {index:.2}, past the buy line at {:.2} — place it again", pick.price),
            Some(CROSSED),
        ));
    }
    Ok(())
}

/// The buy line as armed now, for `valid_minutes` (`None`: till close).
pub fn arm(pick: LinePick, valid_minutes: Option<i32>) -> Result<EntryLine, Refused> {
    let now = shared_domain::now_ist().naive_local();
    let valid_until = shared_domain::line_valid_until(now, valid_minutes).map_err(refused)?;
    Ok(EntryLine { price: pick.price, rising: pick.rising, valid_until, triggered_at: None })
}

/// "NIFTY ≤ 25,980.00 · till 11:45" — how an armed line reads in the log.
pub fn describe(underlying: &str, line: &EntryLine) -> String {
    let till = line.valid_until.as_deref().and_then(|t| t.get(11..16)).map_or_else(|| "till close".to_string(), |t| format!("till {t}"));
    format!("{underlying} {} {:.2} · {till}", if line.rising { '≥' } else { '≤' }, line.price)
}

/// The levels of a line order checked against its line and lots — every
/// shape check of the request, before anything is armed.
pub fn check_request(req: &LineBuyReq, option_type: &str) -> Result<ChartLevels, Refused> {
    let levels = req.levels();
    shared_domain::check_line_levels(Some(option_type), req.line.price, &levels, req.lots).map_err(refused)?;
    Ok(levels)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(body: &str) -> LineBuyReq {
        serde_json::from_str(body).unwrap()
    }

    #[test]
    fn a_request_reads_its_line_levels_and_validity() {
        let r = req(r#"{"trading_symbol":"NIFTY25D0226000CE","lots":4,"line":{"price":25980,"rising":false},"valid_minutes":30,"stop":25950,"targets":[{"price":26050,"lots":1},{"price":26100,"lots":3}]}"#);
        assert_eq!(r.valid_minutes, Some(30));
        let levels = check_request(&r, "CE").unwrap();
        assert_eq!(levels.stop, Some(ChartLevel { price: 25_950.0, trigger: LevelTrigger::Index }));
        assert_eq!(levels.targets.iter().map(|t| (t.price, t.lots)).collect::<Vec<_>>(), [(26_050.0, Some(1)), (26_100.0, Some(3))]);
        assert_ne!(levels.targets[0].id, levels.targets[1].id);
        // Till close, and nothing unknown.
        let till_close = req(r#"{"trading_symbol":"X","lots":1,"line":{"price":1,"rising":true},"stop":0.5,"targets":[{"price":2,"lots":1}]}"#);
        assert_eq!(till_close.valid_minutes, None);
        assert!(serde_json::from_str::<LineBuyReq>(r#"{"trading_symbol":"X","lots":1,"line":{"price":1,"rising":true},"stop":0.5,"targets":[],"sl":1}"#).is_err());
        // A target is required, and a put's stop goes above.
        let no_target = req(r#"{"trading_symbol":"X","lots":1,"line":{"price":100,"rising":true},"stop":99,"targets":[]}"#);
        assert_eq!(check_request(&no_target, "CE").unwrap_err().0, StatusCode::CONFLICT);
        assert_eq!(check_request(&r, "PE").unwrap_err().0, StatusCode::CONFLICT);
    }

    #[test]
    fn a_line_already_crossed_is_refused_with_its_code() {
        let falling = LinePick { price: 25_980.0, rising: false };
        assert!(check_not_crossed("NIFTY", falling, 26_000.0).is_ok());
        let (status, _, code) = check_not_crossed("NIFTY", falling, 25_980.0).unwrap_err();
        assert_eq!((status, code), (StatusCode::CONFLICT, Some(CROSSED)));
        let rising = LinePick { price: 26_020.0, rising: true };
        assert!(check_not_crossed("NIFTY", rising, 26_000.0).is_ok());
        assert!(check_not_crossed("NIFTY", rising, 26_020.0).is_err());
    }

    #[test]
    fn only_the_six_indices_take_a_line() {
        let prices = DashMap::new();
        assert_eq!(index_now(&prices, "RELIANCE").unwrap_err().0, StatusCode::BAD_REQUEST);
        // No value yet.
        assert_eq!(index_now(&prices, "NIFTY").unwrap_err().0, StatusCode::CONFLICT);
        assert_eq!(index_key("SENSEX").as_deref(), Some("idx|SENSEX"));
    }
}
