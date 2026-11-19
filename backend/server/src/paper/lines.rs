//! Line orders on a user's paper book: a buy line on the index with its stop
//! and targets, armed until the index crosses it (see `crate::line_orders`
//! for the rules both desks share). An armed order is not a position — it
//! holds nothing — so it lives in its own list (`user_line_orders`), and the
//! watch loop (`engine::step_lines`) fills it at the option's live price the
//! moment the index reaches the line, as a new position carrying its levels.
//!
//! Locks are always taken book first, then lines, wherever both are held.

use std::collections::HashSet;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use shared_domain::{ChartLevels, EntryLine};

use super::{fill, ContractRef, Filled, LegOut, PaperDesk, PaperPosition, Refusal, MAX_LOTS_PER_TAP, MAX_OPEN_POSITIONS_PER_USER, MAX_USER_POSITION_SCRIPS};
use crate::line_orders;
use crate::routes::v2::positions::LevelsUpdate;

/// One armed line order on a user's paper book.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaperLineOrder {
    pub id: String,
    pub index: String,
    pub trading_symbol: String,
    pub ws_key: String,
    pub expiry: NaiveDate,
    pub strike: f64,
    pub option_type: String,
    pub lot_size: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub segment: String,
    /// Lots it buys.
    pub lots: i32,
    pub line: EntryLine,
    /// The stop and targets the position opens with.
    pub chart: ChartLevels,
    /// IST `YYYY-MM-DD HH:MM:SS` it was armed.
    pub armed_at: String,
}

impl PaperLineOrder {
    /// "NIFTY 25000 CE".
    pub fn label(&self) -> String {
        crate::desk::contract_label(&self.index, Some(self.strike), Some(&self.option_type))
    }

    /// The price-map key of its index's value.
    pub fn index_key(&self) -> Option<String> {
        line_orders::index_key(&self.index)
    }

    fn contract(&self) -> ContractRef {
        ContractRef {
            index: self.index.clone(),
            trading_symbol: self.trading_symbol.clone(),
            ws_key: self.ws_key.clone(),
            expiry: self.expiry,
            strike: self.strike,
            option_type: self.option_type.clone(),
            lot_size: self.lot_size,
            underlying_key: None,
            segment: self.segment.clone(),
        }
    }
}

impl PaperDesk {
    /// `user_id`'s armed line orders, oldest first.
    pub async fn lines(&self, user_id: i64) -> Vec<PaperLineOrder> {
        self.lines.lock().await.get(&user_id).cloned().unwrap_or_default()
    }

    /// Every user's armed line orders — what the watch loop checks.
    pub async fn all_lines(&self) -> Vec<(i64, PaperLineOrder)> {
        let lines = self.lines.lock().await;
        lines.iter().flat_map(|(user, list)| list.iter().map(|o| (*user, o.clone()))).collect()
    }

    /// Whether `id` is one of `user_id`'s line orders.
    pub async fn is_line(&self, user_id: i64, id: &str) -> bool {
        self.lines.lock().await.get(&user_id).is_some_and(|l| l.iter().any(|o| o.id == id))
    }

    /// Arm a line order for `user_id`: `lots` of `contract`, bought when its
    /// index crosses `line`, opening with `chart`. The route checked the
    /// market hours, the prices and the levels. Refused on a contract held
    /// or already waiting on a line, and inside the same caps as a buy.
    pub async fn arm_line(&self, user_id: i64, contract: &ContractRef, lots: i32, line: EntryLine, chart: ChartLevels) -> Result<PaperLineOrder, Refusal> {
        if !(1..=MAX_LOTS_PER_TAP).contains(&lots) {
            return Err(Refusal::bad_request(format!("Lots must be between 1 and {MAX_LOTS_PER_TAP}")));
        }
        let book = self.book.lock().await;
        let held = book.get(&user_id).map(Vec::as_slice).unwrap_or_default();
        let label = crate::desk::contract_label(&contract.index, Some(contract.strike), Some(&contract.option_type));
        if held.iter().any(|p| p.trading_symbol == contract.trading_symbol) {
            return Err(Refusal::conflict(format!("You hold {label} already — one trade per contract")));
        }
        let mut lines = self.lines.lock().await;
        let mine = lines.get(&user_id).cloned().unwrap_or_default();
        if mine.iter().any(|o| o.trading_symbol == contract.trading_symbol) {
            return Err(Refusal::conflict(format!("A buy line is armed on {label} already")));
        }
        if held.len() + mine.len() >= MAX_OPEN_POSITIONS_PER_USER {
            return Err(Refusal::conflict(format!(
                "You hold or wait on {MAX_OPEN_POSITIONS_PER_USER} contracts already — close or cancel one first"
            )));
        }
        let subscribed: HashSet<&str> = book
            .values()
            .flatten()
            .flat_map(|p| [Some(p.ws_key.as_str()), p.underlying_key.as_deref()])
            .flatten()
            .chain(lines.values().flatten().map(|o| o.ws_key.as_str()))
            .collect();
        if !subscribed.contains(contract.ws_key.as_str()) && subscribed.len() >= MAX_USER_POSITION_SCRIPS {
            return Err(Refusal::conflict(
                "Paper trading is full right now — too many contracts are held across all accounts. Try again later",
            ));
        }
        let order = PaperLineOrder {
            id: uuid::Uuid::new_v4().to_string(),
            index: contract.index.clone(),
            trading_symbol: contract.trading_symbol.clone(),
            ws_key: contract.ws_key.clone(),
            expiry: contract.expiry,
            strike: contract.strike,
            option_type: contract.option_type.clone(),
            lot_size: contract.lot_size,
            segment: contract.segment.clone(),
            lots,
            line,
            chart,
            armed_at: shared_domain::current_ist_timestamp_string(),
        };
        let mut next = mine;
        next.push(order.clone());
        self.write_lines(user_id, &next).await?;
        lines.insert(user_id, next);
        drop(lines);
        drop(book);
        // Its contract onto the feed now, so its price is live at the crossing.
        self.feed_wake.notify_one();
        self.note(
            user_id,
            "line",
            Some(&order.id),
            format!("Buy line armed: {lots} lot(s) {} when {}", order.label(), line_orders::describe(&order.index, &order.line)),
        );
        Ok(order)
    }

    /// Cancel `user_id`'s line order `id` while its line is still armed.
    pub async fn cancel_line(&self, user_id: i64, id: &str) -> Result<PaperLineOrder, Refusal> {
        let order = self.take_line(user_id, id, true).await?;
        self.note(user_id, "line", Some(id), format!("Buy line cancelled: {}", order.label()));
        Ok(order)
    }

    /// Cancel every buy line of `user_id`'s — armed, or crossed and not
    /// bought yet (paper has nothing in flight) — so "exit all" leaves
    /// nothing to buy later. How many.
    pub async fn cancel_all_lines(&self, user_id: i64) -> usize {
        let ids: Vec<String> = self.lines(user_id).await.into_iter().map(|o| o.id).collect();
        let mut cancelled = 0;
        for id in ids {
            if let Ok(order) = self.take_line(user_id, &id, false).await {
                self.note(user_id, "line", Some(&id), format!("Buy line cancelled (exit all): {}", order.label()));
                cancelled += 1;
            }
        }
        cancelled
    }

    /// Drop `user_id`'s line order `id` unbought, saying `why`. Gone
    /// already (cancelled meanwhile) is fine.
    pub async fn drop_line(&self, user_id: i64, id: &str, why: &str) {
        if let Ok(order) = self.take_line(user_id, id, false).await {
            self.note(user_id, "line", Some(id), format!("Buy line on {} dropped, nothing bought: {why}", order.label()));
        }
    }

    /// Mark the line of `user_id`'s order `id` crossed at `at`: from then it
    /// is bought at the first live price, or dropped (`engine::decide_line`).
    pub async fn line_crossed(&self, user_id: i64, id: &str, at: &str) {
        let mut lines = self.lines.lock().await;
        let Some(list) = lines.get(&user_id) else { return };
        let mut next = list.clone();
        let Some(order) = next.iter_mut().find(|o| o.id == id && o.line.armed()) else { return };
        order.line.triggered_at = Some(at.to_string());
        if self.write_lines(user_id, &next).await.is_ok() {
            lines.insert(user_id, next);
        }
    }

    /// Take `id` off `user_id`'s line orders (only while armed, if
    /// `armed_only`), saving the list without it.
    async fn take_line(&self, user_id: i64, id: &str, armed_only: bool) -> Result<PaperLineOrder, Refusal> {
        let mut lines = self.lines.lock().await;
        let mut next = lines.get(&user_id).cloned().unwrap_or_default();
        let Some(i) = next.iter().position(|o| o.id == id) else {
            return Err(Refusal::not_found());
        };
        if armed_only && !next[i].line.armed() {
            return Err(Refusal::conflict("The buy line was already crossed — it is being bought"));
        }
        let order = next.remove(i);
        self.write_lines(user_id, &next).await?;
        if next.is_empty() {
            lines.remove(&user_id);
        } else {
            lines.insert(user_id, next);
        }
        Ok(order)
    }

    /// Move the line, the stop or a target of `user_id`'s armed order `id`
    /// (`LevelsUpdate::apply_to_line`); `index` is the index's value now.
    pub async fn set_line_levels(
        &self,
        user_id: i64,
        id: &str,
        update: &LevelsUpdate,
        index: Option<f64>,
    ) -> Result<(PaperLineOrder, Option<String>), line_orders::Refused> {
        use axum::http::StatusCode;
        let mut lines = self.lines.lock().await;
        let mut next = lines.get(&user_id).cloned().unwrap_or_default();
        let Some(order) = next.iter_mut().find(|o| o.id == id) else {
            return Err((StatusCode::NOT_FOUND, "not found".to_string(), None));
        };
        if !order.line.armed() {
            return Err((StatusCode::CONFLICT, "The buy line was crossed — it is being bought; set levels once it is".to_string(), None));
        }
        let index_name = order.index.clone();
        let option_type = order.option_type.clone();
        let lots = order.lots;
        let target_id = update.apply_to_line(&option_type, &index_name, &mut order.line, &mut order.chart, lots, index)?;
        let order = order.clone();
        self.write_lines(user_id, &next).await.map_err(|r| (r.status, r.message, None))?;
        lines.insert(user_id, next);
        drop(lines);
        self.note(
            user_id,
            "line",
            Some(id),
            format!("Buy line on {} moved: buys when {}", order.label(), line_orders::describe(&order.index, &order.line)),
        );
        Ok((order, target_id))
    }

    /// Fill `user_id`'s line order `id` at `price`: a new position on its
    /// contract with the order's stop and targets. The order is taken off
    /// first, so a fill refused here (funds, caps) never buys later — it is
    /// dropped, and the activity says why.
    pub async fn fill_line(&self, user_id: i64, id: &str, price: f64) -> Result<Filled, Refusal> {
        let mut book = self.book.lock().await;
        let order = {
            let mut lines = self.lines.lock().await;
            let mut next = lines.get(&user_id).cloned().unwrap_or_default();
            let Some(i) = next.iter().position(|o| o.id == id) else {
                return Err(Refusal::not_found());
            };
            let order = next.remove(i);
            self.write_lines(user_id, &next).await?;
            if next.is_empty() {
                lines.remove(&user_id);
            } else {
                lines.insert(user_id, next);
            }
            order
        };
        let filled = self.open_from_line(&mut book, user_id, &order, price).await;
        drop(book);
        match &filled {
            Ok(f) => self.note(
                user_id,
                "buy",
                Some(&f.position_id),
                format!("Bought {} {} at ₹{price:.2} — its buy line at {:.2} was crossed", f.qty, order.label(), order.line.price),
            ),
            Err(r) => self.note(user_id, "line", Some(id), format!("Buy line on {} crossed but not bought: {}", order.label(), r.message)),
        }
        filled
    }

    async fn open_from_line(&self, book: &mut super::Book, user_id: i64, order: &PaperLineOrder, price: f64) -> Result<Filled, Refusal> {
        if !(price.is_finite() && price > 0.0) || order.lot_size <= 0 {
            return Err(Refusal::conflict(format!("No live price for {} — not buying blind", order.trading_symbol)));
        }
        let mut next = book.get(&user_id).cloned().unwrap_or_default();
        if next.iter().any(|p| p.trading_symbol == order.trading_symbol) {
            return Err(Refusal::conflict(format!("{} is held already", order.label())));
        }
        if next.len() >= MAX_OPEN_POSITIONS_PER_USER {
            return Err(Refusal::conflict(format!("You hold {MAX_OPEN_POSITIONS_PER_USER} contracts already")));
        }
        let contract = order.contract();
        let qty = order.lots * contract.lot_size;
        next.push(PaperPosition {
            id: uuid::Uuid::new_v4().to_string(),
            index: contract.index.clone(),
            trading_symbol: contract.trading_symbol.clone(),
            ws_key: contract.ws_key.clone(),
            expiry: contract.expiry,
            strike: contract.strike,
            option_type: contract.option_type.clone(),
            lot_size: contract.lot_size,
            qty,
            avg_price: price,
            chart: order.chart.clone(),
            opened_at: shared_domain::current_ist_timestamp_string(),
            last_ltp: Some(price),
            underlying_key: None,
            segment: contract.segment.clone(),
        });
        let pos = next.last().cloned().expect("just pushed");
        let leg = LegOut {
            pos: &pos,
            action: "BUY",
            qty,
            price,
            charges: fill::buy_fill(&pos.index, qty, price),
            reason: None,
            label: format!("BUY {} lot(s) {} @ MKT {price:.2} (buy line {:.2})", order.lots, pos.label(), order.line.price),
        };
        let net_value = leg.charges.net_value;
        let (trade_id, balance) = self.record(user_id, leg, &next).await?;
        book.insert(user_id, next);
        Ok(Filled {
            trade_id,
            position_id: pos.id.clone(),
            action: "BUY",
            contract: pos.label(),
            index: pos.index.clone(),
            qty,
            price,
            net_value,
            held: pos.qty,
            balance,
            reason: None,
        })
    }

    async fn write_lines(&self, user_id: i64, orders: &[PaperLineOrder]) -> Result<(), Refusal> {
        let json = serde_json::to_string(orders).map_err(|e| Refusal::server("save the buy lines", e))?;
        sqlx::query(
            "INSERT INTO user_line_orders (user_id, json, updated_at) VALUES (?, ?, ?)
             ON CONFLICT(user_id) DO UPDATE SET json = excluded.json, updated_at = excluded.updated_at",
        )
        .bind(user_id)
        .bind(json)
        .bind(shared_domain::current_ist_timestamp_string())
        .execute(&self.pool)
        .await
        .map_err(|e| Refusal::server("save the buy lines", e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paper::tests::{desk, nifty};
    use crate::routes::v2::positions::LevelsUpdate;
    use shared_domain::{ChartLevel, ChartTarget, LevelTrigger};

    fn line(price: f64, rising: bool) -> EntryLine {
        EntryLine { price, rising, valid_until: None, triggered_at: None }
    }

    /// A call's stop below 25,980 and two 1-lot targets above it.
    fn levels() -> ChartLevels {
        let t = |id: &str, price| ChartTarget { id: id.into(), price, trigger: LevelTrigger::Index, lots: Some(1) };
        ChartLevels { stop: Some(ChartLevel { price: 25_950.0, trigger: LevelTrigger::Index }), targets: vec![t("a", 26_050.0), t("b", 26_100.0)] }
    }

    #[tokio::test]
    async fn an_armed_line_fills_as_a_position_with_its_levels_once() {
        let (_db, desk) = desk("line-fill").await;
        let order = desk.arm_line(1, &nifty(25_000.0, "CE"), 2, line(25_980.0, false), levels()).await.unwrap();
        assert_eq!(desk.lines(1).await.len(), 1);
        assert!(desk.is_line(1, &order.id).await);
        // One trade per contract: no second line, and no tap buy, on it.
        assert!(desk.arm_line(1, &nifty(25_000.0, "CE"), 1, line(25_970.0, false), levels()).await.is_err());
        assert_eq!(desk.buy(1, &nifty(25_000.0, "CE"), 1, 100.0).await.unwrap_err().status, axum::http::StatusCode::CONFLICT);

        let filled = desk.fill_line(1, &order.id, 120.0).await.unwrap();
        assert_eq!((filled.qty, filled.price), (2 * nifty(25_000.0, "CE").lot_size, 120.0));
        let open = desk.open(1).await;
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].chart, levels());
        assert!(desk.lines(1).await.is_empty());
        // Filled once only.
        assert_eq!(desk.fill_line(1, &order.id, 120.0).await.unwrap_err().status, axum::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_line_is_cancelled_or_moved_only_while_armed() {
        let (_db, desk) = desk("line-edit").await;
        let order = desk.arm_line(1, &nifty(25_000.0, "CE"), 2, line(25_980.0, false), levels()).await.unwrap();
        // Moved below the stop: refused, nothing changes.
        let below_stop: LevelsUpdate = serde_json::from_str(r#"{"entry":{"price":25940,"rising":false}}"#).unwrap();
        assert!(desk.set_line_levels(1, &order.id, &below_stop, Some(26_000.0)).await.is_err());
        // Moved past the index: refused as crossed.
        let crossed: LevelsUpdate = serde_json::from_str(r#"{"entry":{"price":26010,"rising":false}}"#).unwrap();
        let (_, _, code) = desk.set_line_levels(1, &order.id, &crossed, Some(26_000.0)).await.unwrap_err();
        assert_eq!(code, Some(line_orders::CROSSED));
        // The stop stays.
        let no_stop: LevelsUpdate = serde_json::from_str(r#"{"stop":null}"#).unwrap();
        assert!(desk.set_line_levels(1, &order.id, &no_stop, None).await.is_err());
        let moved: LevelsUpdate = serde_json::from_str(r#"{"entry":{"price":25970,"rising":false}}"#).unwrap();
        let (after, _) = desk.set_line_levels(1, &order.id, &moved, Some(26_000.0)).await.unwrap();
        assert_eq!(after.line.price, 25_970.0);

        desk.line_crossed(1, &order.id, "2026-11-18 11:00:00").await;
        assert!(desk.cancel_line(1, &order.id).await.is_err());
        assert!(desk.set_line_levels(1, &order.id, &moved, Some(26_000.0)).await.is_err());
        desk.drop_line(1, &order.id, "no live price").await;
        assert!(desk.lines(1).await.is_empty());
    }

    #[tokio::test]
    async fn exit_all_cancels_crossed_lines_too_and_taps_count_armed_ones() {
        let (_db, desk) = desk("line-caps").await;
        let a = desk.arm_line(1, &nifty(25_000.0, "CE"), 1, line(25_980.0, false), levels()).await.unwrap();
        desk.arm_line(1, &nifty(25_050.0, "CE"), 1, line(25_980.0, false), levels()).await.unwrap();
        // Crossed, waiting for a live price: exit all still takes it.
        desk.line_crossed(1, &a.id, "2026-11-18 11:00:00").await;
        assert_eq!(desk.cancel_all_lines(1).await, 2);
        assert!(desk.lines(1).await.is_empty());

        // Armed lines count against the contracts one user may hold or wait on.
        desk.arm_line(1, &nifty(24_000.0, "CE"), 1, line(25_980.0, false), levels()).await.unwrap();
        for i in 0..MAX_OPEN_POSITIONS_PER_USER - 1 {
            desk.buy(1, &nifty(25_100.0 + 50.0 * i as f64, "PE"), 1, 10.0).await.unwrap();
        }
        let full = desk.buy(1, &nifty(26_000.0, "PE"), 1, 10.0).await.unwrap_err();
        assert_eq!(full.status, axum::http::StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn armed_lines_survive_a_restart_and_keep_their_contract_on_the_feed() {
        let (db, desk) = desk("line-reload").await;
        desk.arm_line(1, &nifty(25_000.0, "CE"), 1, line(25_980.0, false), levels()).await.unwrap();
        let (scrips, indices) = desk.feed_keys().await;
        assert!(scrips.contains(&nifty(25_000.0, "CE").ws_key));
        assert!(indices.contains("NIFTY"));
        // A user waiting on a line isn't flat, and can't reset.
        assert!(desk.hold_flat(1).await.is_none());
        assert!(desk.reset(1).await.is_err());
        let again = PaperDesk::load(crate::db::init_db(&db.url()).await, Default::default()).await.unwrap();
        assert_eq!(again.lines(1).await.len(), 1);
    }
}
