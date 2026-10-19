use std::collections::{BTreeMap, HashMap};
use chrono::NaiveDate;
use shared_domain::{OrderRequest, TradeSignal};

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScripRecord {
    pub instrument_token: String,
    pub trading_symbol: String,
    pub symbol_name: String,
    pub exchange_segment_code: String,
    pub strike_price: f64,
    pub option_type: String, // CE or PE
    pub expiry_date: NaiveDate,
    pub lot_size: u32,
    pub tick_size: f64,
}

impl ScripRecord {
    /// The option's real strike. The scrip master stores it scaled (×100 for
    /// every index option seen so far: `2290000` for `NIFTY26O1922900PE`), so
    /// the scale is read off the trading symbol, which always ends in the
    /// strike followed by `CE`/`PE`. `None` for anything that isn't a CE/PE
    /// whose symbol agrees with one of the known scales.
    pub fn normalized_strike(&self) -> Option<f64> {
        let opt = self.option_type.as_str();
        if opt != "CE" && opt != "PE" {
            return None;
        }
        let stem = self.trading_symbol.trim().strip_suffix(opt)?;
        [100.0, 1.0, 1000.0].into_iter().find_map(|div| {
            let s = self.strike_price / div;
            let whole = s.round();
            (whole > 0.0 && (s - whole).abs() < 1e-6 && stem.ends_with(&format!("{}", whole as i64)))
                .then_some(whole)
        })
    }

    /// One-lot BUY template for this contract. `MonitoredPosition::resolved_order`
    /// is always this pristine template — its quantity *is* the lot size; the
    /// engine sets the real quantity, side and order type when it sends.
    pub fn order_template(&self) -> OrderRequest {
        use shared_domain::{AmoFlag, ExchangeSegment, OrderType, ProductCode, TransactionType, Validity};
        let exchange_segment = match self.exchange_segment_code.as_str() {
            "bse_fo" => ExchangeSegment::BseFo,
            "nse_cm" => ExchangeSegment::NseCm,
            _ => ExchangeSegment::NseFo,
        };
        OrderRequest {
            after_market_order: AmoFlag::No,
            disclosed_quantity: "0".to_string(),
            exchange_segment,
            market_protection: "0".to_string(),
            product_code: ProductCode::Nrml,
            portfolio_flag: "N".to_string(),
            price: "0".to_string(),
            order_type: OrderType::Limit,
            quantity: self.lot_size.to_string(),
            validity: Validity::Day,
            trigger_price: "0".to_string(),
            trading_symbol: self.trading_symbol.clone(),
            transaction_type: TransactionType::Buy,
        }
    }

    /// Price-map / feed key for this contract — `nse_fo|47158`.
    pub fn ws_key(&self) -> String {
        format!("{}|{}", self.exchange_segment_code, self.instrument_token)
    }
}

/// One strike of an [`OptionChain`]: the call and put listed at it.
#[derive(Debug, Clone)]
pub struct ChainStrike {
    pub strike: f64,
    pub ce: Option<ScripRecord>,
    pub pe: Option<ScripRecord>,
}

/// Every strike of one index's nearest expiry, sorted by strike.
#[derive(Debug, Clone)]
pub struct OptionChain {
    pub index: String,
    pub expiry: NaiveDate,
    pub lot_size: u32,
    pub tick_size: f64,
    pub strikes: Vec<ChainStrike>,
}

/// F&O segment an index's options trade in.
pub fn index_fo_segment(index: &str) -> &'static str {
    match index {
        "SENSEX" | "BANKEX" => "bse_fo",
        _ => "nse_fo",
    }
}

#[derive(Default, serde::Serialize)]
pub struct ScripStore {
    // Map of symbol_name (e.g. "BANKNIFTY") -> Vec of options
    pub records: HashMap<String, Vec<ScripRecord>>,
}

impl ScripStore {
    pub fn parse_csv(csv: &str, exchange_segment_code: &str) -> Self {
        let mut store = ScripStore::default();
        let mut lines = csv.lines();
        
        let header_line = match lines.next() {
            Some(line) => line,
            None => return store,
        };
        
        let mut headers = HashMap::new();
        for (i, h) in header_line.split(',').enumerate() {
            headers.insert(h.trim().to_lowercase(), i);
        }
        
        // Kotak Neo typical headers mapping
        let token_idx = headers.get("psymbol").copied();
        let trd_symbol_idx = headers.get("ptrdsymbol").copied();
        let sym_name_idx = headers.get("psymbolname").copied();
        
        // Use contains for strike_idx because the CSV header has a stray semicolon `dStrikePrice;`
        let strike_idx = headers.keys().find(|k| k.contains("strikeprice")).and_then(|k| headers.get(k)).copied();
        
        let opt_type_idx = headers.get("poptiontype").copied();
        let expiry_idx = headers.get("lexpirydate").or(headers.get("pexpirydate")).copied();
        let lot_size_idx = headers.get("llotsize").copied();
        let tick_size_idx = headers.get("pticksize").or(headers.get("ticksize")).copied();

        // If we can't find core headers, fallback to some defaults or return empty.
        // But for safety we just skip lines missing these.
        if token_idx.is_none() || trd_symbol_idx.is_none() || sym_name_idx.is_none() {
            tracing::error!("Failed to parse Scrip Master headers. Found: {:?}", headers.keys());
            return store;
        }

        let token_idx = token_idx.unwrap();
        let trd_symbol_idx = trd_symbol_idx.unwrap();
        let sym_name_idx = sym_name_idx.unwrap();
        let strike_idx = strike_idx.unwrap_or(usize::MAX);
        let opt_type_idx = opt_type_idx.unwrap_or(usize::MAX);
        let expiry_idx = expiry_idx.unwrap_or(usize::MAX);
        let lot_size_idx = lot_size_idx.unwrap_or(usize::MAX);
        let tick_size_idx = tick_size_idx.unwrap_or(usize::MAX);

        for line in lines {
            let cols: Vec<&str> = line.split(',').map(|s| s.trim().trim_matches('"')).collect();
            if cols.len() <= trd_symbol_idx { continue; }
            
            let sym_name = cols[sym_name_idx].to_uppercase();
            if sym_name.is_empty() { continue; }

            // Parse strike
            let strike_price = if strike_idx < cols.len() {
                cols[strike_idx].parse::<f64>().unwrap_or(0.0)
            } else { 0.0 };

            // Parse Option type
            let option_type = if opt_type_idx < cols.len() {
                cols[opt_type_idx].to_uppercase()
            } else { "".to_string() };

            // Parse expiry date (formats can vary: string or epoch)
            let expiry_date = if expiry_idx < cols.len() {
                let date_str = cols[expiry_idx];
                // Try string formats first
                if let Ok(d) = NaiveDate::parse_from_str(date_str, "%d-%b-%Y") {
                    Some(d)
                } else if let Ok(d) = NaiveDate::parse_from_str(date_str, "%d/%m/%Y") {
                    Some(d)
                } else if let Ok(d) = NaiveDate::parse_from_str(date_str, "%d-%m-%Y") {
                    Some(d)
                } else if let Ok(d) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
                    Some(d)
                } else if let Ok(epoch) = date_str.parse::<i64>() {
                    // It's an epoch. If nse_fo, we must add 315511200 (Kotak custom epoch offset)
                    // The epoch might already be correct if it's BSE, but we'll assume NSE F&O for now.
                    // 315511200 is Kotak's exact specified offset. Then we add 19800 for IST offset to ensure it falls on the correct day in UTC parsing.
                    let adjusted_epoch = if epoch < 1600000000 { epoch + 315511200 + 19800 } else { epoch + 19800 };
                    chrono::DateTime::from_timestamp(adjusted_epoch, 0)
                        .map(|dt| dt.date_naive())
                } else {
                    None
                }
            } else {
                None
            };

            // Parse lot size
            let lot_size = if lot_size_idx < cols.len() {
                cols[lot_size_idx].parse::<u32>().unwrap_or(1)
            } else { 1 };

            // Parse tick size
            let tick_size = if tick_size_idx < cols.len() {
                cols[tick_size_idx].parse::<f64>().unwrap_or(0.05)
            } else { 0.05 };

            let record = ScripRecord {
                instrument_token: cols[token_idx].to_string(),
                trading_symbol: cols[trd_symbol_idx].to_string(),
                symbol_name: sym_name.clone(),
                exchange_segment_code: exchange_segment_code.to_string(),
                strike_price,
                option_type,
                expiry_date: expiry_date.unwrap_or(NaiveDate::from_ymd_opt(2099, 12, 31).unwrap()),
                lot_size,
                tick_size,
            };

            store.records.entry(sym_name).or_default().push(record);
        }

        tracing::info!("Loaded {} distinct symbols into ScripStore", store.records.len());
        store
    }

    pub fn merge(&mut self, other: ScripStore) {
        for (symbol, mut records) in other.records {
            self.records.entry(symbol).or_default().append(&mut records);
        }
    }

    /// Exact lookup by broker trading symbol (e.g. `"NIFTY2872625000CE"`) —
    /// for resolving a raw broker position/order that has no parsed
    /// `TradeSignal` to search with, unlike `resolve_signal`'s strike/expiry/
    /// option-type search.
    pub fn find_by_trading_symbol(&self, trading_symbol: &str) -> Option<ScripRecord> {
        let needle = trading_symbol.trim();
        self.records
            .values()
            .flatten()
            .find(|r| r.trading_symbol.trim() == needle)
            .cloned()
    }

    /// `index`'s option chain for its nearest expiry on or after `today`, in
    /// the index's own F&O segment. `None` when the store holds no options
    /// for it.
    pub fn nearest_expiry_chain(&self, index: &str, today: NaiveDate) -> Option<OptionChain> {
        let segment = index_fo_segment(index);
        let options: Vec<(&ScripRecord, f64)> = self
            .records
            .get(index)?
            .iter()
            .filter(|r| r.exchange_segment_code == segment && r.expiry_date >= today)
            .filter_map(|r| r.normalized_strike().map(|s| (r, s)))
            .collect();
        let expiry = options.iter().map(|(r, _)| r.expiry_date).min()?;

        let mut by_strike: BTreeMap<i64, ChainStrike> = BTreeMap::new();
        for (r, strike) in options.iter().filter(|(r, _)| r.expiry_date == expiry) {
            let row = by_strike
                .entry((strike * 100.0).round() as i64)
                .or_insert_with(|| ChainStrike { strike: *strike, ce: None, pe: None });
            let slot = if r.option_type == "CE" { &mut row.ce } else { &mut row.pe };
            if slot.is_none() {
                *slot = Some((*r).clone());
            }
        }
        let first = by_strike.values().find_map(|s| s.ce.as_ref().or(s.pe.as_ref()))?;
        Some(OptionChain {
            index: index.to_string(),
            expiry,
            lot_size: first.lot_size,
            tick_size: first.tick_size,
            strikes: by_strike.into_values().collect(),
        })
    }

    pub fn resolve_signal(&self, signal: &TradeSignal) -> Option<ScripRecord> {
        let options = self.records.get(&signal.instrument_name)?;
        
        // Target strike
        let target_strike = signal.strike.unwrap_or(0.0);
        let target_opt_type = signal.option_type.as_deref().unwrap_or("");

        // Parse signal expiry (which the parser outputs as DD-MMM-YYYY, e.g. 30-JUL-2026)
        // chrono's %b expects Titlecase (Jul), so we need to fix the casing.
        let target_expiry_date = if let Some(ref exp_str) = signal.expiry {
            let exp_lower = exp_str.to_lowercase();
            let mut chars = exp_lower.chars().collect::<Vec<_>>();
            if chars.len() == 11 && chars[3].is_ascii_lowercase() {
                chars[3] = chars[3].to_ascii_uppercase();
            }
            let exp_title = chars.into_iter().collect::<String>();
            NaiveDate::parse_from_str(&exp_title, "%d-%b-%Y").ok()
        } else {
            None
        };

        let today = shared_domain::today_ist();
        
        if signal.instrument_name == "NIFTY" {
            let sample_strikes: Vec<f64> = options.iter().take(5).map(|o| o.strike_price).collect();
            let matching_opt: Vec<f64> = options.iter().filter(|o| o.option_type == target_opt_type).take(5).map(|o| o.strike_price).collect();
            tracing::info!("DEBUG NIFTY: target_strike={}, target_opt={}, sample_strikes={:?}, matching_opt={:?}", target_strike, target_opt_type, sample_strikes, matching_opt);
        }

        // Filter by strike (handle 1x, 100x, 1000x scaling from Kotak) and option type
        let mut candidates: Vec<&ScripRecord> = options.iter()
            .filter(|o| {
                let s = o.strike_price;
                let strike_match = (s - target_strike).abs() < 1e-4 ||
                (s / 100.0 - target_strike).abs() < 1e-4 ||
                (s / 1000.0 - target_strike).abs() < 1e-4;
                if strike_match {
                    tracing::info!("DEBUG STRIKE MATCH: s={}, target_strike={}, o.option_type={}, target_opt={}, o.expiry_date={}, today={}", s, target_strike, o.option_type, target_opt_type, o.expiry_date, today);
                }
                strike_match
            })
            .filter(|o| target_opt_type.is_empty() || o.option_type == target_opt_type)
            .filter(|o| o.exchange_segment_code == "nse_cm" || o.expiry_date >= today)
            .filter(|o| {
                // Ignore BSE options unless it's a known BSE index
                if o.exchange_segment_code == "bse_fo" {
                    o.symbol_name == "SENSEX" || o.symbol_name == "BANKEX"
                } else {
                    true
                }
            })
            .collect();

        if candidates.is_empty() {
            return None;
        }

        // Sort by expiry date, prioritizing nse_fo
        candidates.sort_by_key(|o| {
            let exch_score = if o.exchange_segment_code == "nse_fo" { 0 } else { 1 };
            (o.expiry_date, exch_score)
        });

        if let Some(target_date) = target_expiry_date {
            // Find the closest expiry to the requested target_date
            candidates.into_iter().min_by_key(|o| {
                let diff = (o.expiry_date - target_date).num_days().abs();
                let exch_score = if o.exchange_segment_code == "nse_fo" { 0 } else { 1 };
                (diff, exch_score)
            }).cloned()
        } else {
            // Pick the earliest upcoming expiry
            candidates.into_iter().next().cloned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(sym: &str, segment: &str, raw_strike: f64, opt: &str, expiry: (i32, u32, u32)) -> ScripRecord {
        ScripRecord {
            instrument_token: sym.len().to_string(),
            trading_symbol: sym.to_string(),
            symbol_name: sym.chars().take_while(|c| c.is_ascii_alphabetic()).collect(),
            exchange_segment_code: segment.to_string(),
            strike_price: raw_strike,
            option_type: opt.to_string(),
            expiry_date: NaiveDate::from_ymd_opt(expiry.0, expiry.1, expiry.2).unwrap(),
            lot_size: 65,
            tick_size: 0.05,
        }
    }

    #[test]
    fn normalized_strike_reads_the_scale_off_the_symbol() {
        // Real records from the 2026-10-19 scrip master.
        assert_eq!(rec("NIFTY26O1922900PE", "nse_fo", 2290000.0, "PE", (2026, 10, 19)).normalized_strike(), Some(22900.0));
        assert_eq!(rec("SENSEX26O2275100CE", "bse_fo", 7510000.0, "CE", (2026, 10, 22)).normalized_strike(), Some(75100.0));
        assert_eq!(rec("MIDCPNIFTY26OCT11875PE", "nse_fo", 1187500.0, "PE", (2026, 10, 27)).normalized_strike(), Some(11875.0));
        assert_eq!(rec("BANKNIFTY26OCT45300PE", "nse_fo", 45300.0, "PE", (2026, 10, 27)).normalized_strike(), Some(45300.0));
        // Futures and symbols that disagree with every scale are not options we can place.
        assert_eq!(rec("NIFTY26OCTFUT", "nse_fo", 0.0, "XX", (2026, 10, 27)).normalized_strike(), None);
        assert_eq!(rec("NIFTY26O1922900PE", "nse_fo", 2295000.0, "PE", (2026, 10, 19)).normalized_strike(), None);
    }

    #[test]
    fn nearest_expiry_chain_pairs_calls_and_puts_of_the_earliest_expiry() {
        let mut store = ScripStore::default();
        store.records.insert("NIFTY".to_string(), vec![
            rec("NIFTY26O1922950CE", "nse_fo", 2295000.0, "CE", (2026, 10, 19)),
            rec("NIFTY26O1922900CE", "nse_fo", 2290000.0, "CE", (2026, 10, 19)),
            rec("NIFTY26O1922900PE", "nse_fo", 2290000.0, "PE", (2026, 10, 19)),
            rec("NIFTY26O2722900CE", "nse_fo", 2290000.0, "CE", (2026, 10, 27)),
            // Already expired — never the "nearest".
            rec("NIFTY26O1322900CE", "nse_fo", 2290000.0, "CE", (2026, 10, 13)),
        ]);
        let today = NaiveDate::from_ymd_opt(2026, 10, 19).unwrap();
        let chain = store.nearest_expiry_chain("NIFTY", today).unwrap();
        assert_eq!(chain.expiry, today);
        assert_eq!(chain.strikes.len(), 2);
        assert_eq!(chain.strikes[0].strike, 22900.0);
        assert_eq!(chain.strikes[0].ce.as_ref().unwrap().trading_symbol, "NIFTY26O1922900CE");
        assert_eq!(chain.strikes[0].pe.as_ref().unwrap().trading_symbol, "NIFTY26O1922900PE");
        assert_eq!(chain.strikes[1].strike, 22950.0);
        assert!(chain.strikes[1].pe.is_none());

        // The day after expiry, the next week's contracts are the nearest.
        let chain = store.nearest_expiry_chain("NIFTY", NaiveDate::from_ymd_opt(2026, 10, 20).unwrap()).unwrap();
        assert_eq!(chain.expiry, NaiveDate::from_ymd_opt(2026, 10, 27).unwrap());
        assert!(store.nearest_expiry_chain("SENSEX", today).is_none());
    }
}
