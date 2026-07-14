use std::collections::HashMap;
use chrono::NaiveDate;
use shared_domain::TradeSignal;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScripRecord {
    pub instrument_token: String,
    pub trading_symbol: String,
    pub symbol_name: String,
    pub strike_price: f64,
    pub option_type: String, // CE or PE
    pub expiry_date: NaiveDate,
    pub lot_size: u32,
}

#[derive(Default, serde::Serialize)]
pub struct ScripStore {
    // Map of symbol_name (e.g. "BANKNIFTY") -> Vec of options
    pub records: HashMap<String, Vec<ScripRecord>>,
}

impl ScripStore {
    pub fn parse_csv(csv: &str) -> Self {
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
                    d
                } else if let Ok(d) = NaiveDate::parse_from_str(date_str, "%d/%m/%Y") {
                    d
                } else if let Ok(d) = NaiveDate::parse_from_str(date_str, "%d-%m-%Y") {
                    d
                } else if let Ok(d) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
                    d
                } else if let Ok(epoch) = date_str.parse::<i64>() {
                    // It's an epoch. If nse_fo, we must add 315511200 (Kotak custom epoch offset)
                    // The epoch might already be correct if it's BSE, but we'll assume NSE F&O for now.
                    // 315511200 is Kotak's exact specified offset. Then we add 19800 for IST offset to ensure it falls on the correct day in UTC parsing.
                    let adjusted_epoch = if epoch < 1600000000 { epoch + 315511200 + 19800 } else { epoch + 19800 };
                    chrono::DateTime::from_timestamp(adjusted_epoch, 0)
                        .map(|dt| dt.date_naive())
                        .unwrap_or_default()
                } else {
                    NaiveDate::default()
                }
            } else {
                NaiveDate::default()
            };

            let lot_size = if lot_size_idx < cols.len() {
                cols[lot_size_idx].parse::<u32>().unwrap_or(1)
            } else { 1 };

            let record = ScripRecord {
                instrument_token: cols[token_idx].to_string(),
                trading_symbol: cols[trd_symbol_idx].to_string(),
                symbol_name: sym_name.clone(),
                strike_price,
                option_type,
                expiry_date,
                lot_size,
            };

            store.records.entry(sym_name).or_default().push(record);
        }

        tracing::info!("Loaded {} distinct symbols into ScripStore", store.records.len());
        store
    }

    pub fn resolve_signal(&self, signal: &TradeSignal) -> Option<ScripRecord> {
        let options = self.records.get(&signal.instrument_name)?;
        
        // Target strike
        let target_strike = signal.strike.unwrap_or(0.0);
        let target_opt_type = signal.option_type.as_deref().unwrap_or("");

        // Parse signal expiry (which the parser outputs as DD-MMM-YYYY)
        let target_expiry_date = if let Some(ref exp_str) = signal.expiry {
            NaiveDate::parse_from_str(exp_str, "%d-%b-%Y").ok()
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
            .filter(|o| o.expiry_date >= today)
            .collect();

        if candidates.is_empty() {
            return None;
        }

        // Sort by expiry date
        candidates.sort_by_key(|o| o.expiry_date);

        if let Some(target_date) = target_expiry_date {
            // Find the closest expiry to the requested target_date
            candidates.into_iter().min_by_key(|o| {
                let diff = (o.expiry_date - target_date).num_days().abs();
                diff
            }).cloned()
        } else {
            // Pick the earliest upcoming expiry
            candidates.into_iter().next().cloned()
        }
    }
}
