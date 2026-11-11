//! Which contract a paper buy names: `POST /api/v2/me/buy`'s `trading_symbol`
//! resolved against the scrip master. An index option on one of the six
//! indices always (exactly what the owner's one-tap buy resolves, through
//! the same `execute::index_option` test); behind the `stock_paper` switch,
//! any other F&O underlying's CE or PE as well — a stock's, or an index the
//! app only charts — in NSE's F&O segment, with a readable strike, expiring
//! today or later. Never a future, a stock itself, an expired contract or a
//! BSE stock option. Nothing here places an order or reads a price: the
//! route still demands the contract's own fresh tick before it fills.
//!
//! The owner's live buy (`POST /api/execute/buy`) does not use this: it
//! keeps `ExecuteBoard::find_contract`, index-only, whatever the switch says.

use chrono::NaiveDate;
use shared_domain::INDEX_NAMES;
use trading_engine::{index_fo_segment, ScripRecord, ScripStore};

use super::{ContractRef, Refusal};
use crate::execute::{index_option, listed_option};
use crate::AppState;

/// The contract `trading_symbol` names, if a paper buy may take it. The
/// scrip master must be loaded (it is, after the Kotak login); a stock's
/// cash-row feed key comes from the instrument catalog, which is built only
/// while `search` is on — without it the position has no underlying price
/// to set a level on, and says so when one is tried.
pub async fn find_paper_contract(state: &AppState, trading_symbol: &str) -> Result<ContractRef, Refusal> {
    let stock_paper = state.flags.read().await.stock_paper;
    let today = shared_domain::today_ist();
    let store = state.scrip_store.read().await;
    let Some(store) = store.as_ref() else {
        return Err(Refusal::conflict("Scrip master isn't loaded yet — try again after the Kotak login"));
    };
    resolve(store, trading_symbol, today, stock_paper, &|underlying| state.instruments.spot_key(underlying))
}

/// [`find_paper_contract`] on `store` as of `today`. `spot_key` gives an
/// underlying's cash-row feed key (`nse_cm|2885`), or `None` when the
/// catalog has none for it.
pub fn resolve(
    store: &ScripStore,
    trading_symbol: &str,
    today: NaiveDate,
    stock_paper: bool,
    spot_key: &dyn Fn(&str) -> Option<String>,
) -> Result<ContractRef, Refusal> {
    let needle = trading_symbol.trim();
    if needle.is_empty() {
        return Err(Refusal::bad_request("trading_symbol is required"));
    }
    // The six indices first, exactly as the owner's buy resolves them.
    if let Some(record) = store.find_index_option(needle) {
        return match index_option(&record, today) {
            Some(index) => Ok(contract(index, &record, None)),
            None => Err(why_not(needle, &record, today)),
        };
    }
    if !stock_paper {
        return Err(Refusal::bad_request(format!("{needle} is not a listed index option")));
    }
    // Any other underlying the scrip master keys records under whose name
    // starts the trading symbol (`RELIANCE26NOV1400CE` → `RELIANCE`; the
    // longest name first, so `M&MFIN…` is never read as `M&M`'s). The exact
    // trading symbol is then looked for among that underlying's own rows.
    let mut candidates: Vec<&String> = store
        .records
        .keys()
        .filter(|name| !INDEX_NAMES.contains(&name.as_str()) && needle.starts_with(name.as_str()))
        .collect();
    candidates.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    let mut seen: Option<&ScripRecord> = None;
    for underlying in candidates {
        let Some(rows) = store.records.get(underlying) else { continue };
        for record in rows.iter().filter(|r| r.trading_symbol.trim() == needle) {
            if listed_option(record, underlying, today) {
                return Ok(contract(underlying, record, spot_key(underlying)));
            }
            seen.get_or_insert(record);
        }
    }
    Err(match seen {
        Some(record) => why_not(needle, record, today),
        None => Refusal::bad_request(format!("{needle} is not a listed option")),
    })
}

fn contract(underlying: &str, record: &ScripRecord, underlying_key: Option<String>) -> ContractRef {
    ContractRef {
        index: underlying.to_string(),
        trading_symbol: record.trading_symbol.clone(),
        ws_key: record.ws_key(),
        expiry: record.expiry_date,
        // `listed_option` and `index_option` have checked it reads.
        strike: record.normalized_strike().unwrap_or(record.strike_price),
        option_type: record.option_type.clone(),
        lot_size: record.lot_size as i32,
        underlying_key,
        segment: record.exchange_segment_code.clone(),
    }
}

/// Why `record`, which `needle` names, can't be bought on paper.
fn why_not(needle: &str, record: &ScripRecord, today: NaiveDate) -> Refusal {
    let segment = record.exchange_segment_code.as_str();
    let message = if !segment.ends_with("_fo") {
        format!("{needle} is a stock, not an option — only options are bought on paper")
    } else if record.option_type != "CE" && record.option_type != "PE" {
        format!("{needle} is a future, not an option — only options are bought on paper")
    } else if record.expiry_date < today {
        format!("{needle} expired on {}", record.expiry_date.format("%d %b %Y"))
    } else if segment != index_fo_segment(&record.symbol_name) {
        format!("{needle} trades on BSE — only NSE stock options are bought on paper")
    } else if record.normalized_strike().is_none() {
        format!("Can't read the strike of {needle}")
    } else {
        format!("{needle} is not a listed option")
    };
    Refusal::bad_request(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    /// Kotak's shape, cut down: NIFTY and SENSEX index options, RELIANCE and
    /// M&M / M&MFIN stock options, futures, an expired contract, a cash row,
    /// a BSE stock option and an index the app has no feed name for.
    const NSE_FO: &str = "pSymbol,pExchSeg,pInstType,pSymbolName,pTrdSymbol,pOptionType,lLotSize,lExpiryDate,dStrikePrice;,dTickSize ,pISIN,pDesc\n\
51386,nse_fo,OPTIDX,NIFTY,NIFTY26N0323000CE,CE,65,1478152800,2300000.00,5,,NIFTY 03NOV26 23000 CE\n\
52001,nse_fo,OPTIDX,NIFTY,NIFTY26NOV23000CE,CE,65,1479967200,2300000.00,5,,NIFTY 24NOV26 23000 CE\n\
35013,nse_fo,FUTIDX,NIFTY,NIFTY26NOVFUT,XX,65,1479967200,0.00,5,,NIFTY 24NOV26 FUT\n\
41222,nse_fo,OPTSTK,RELIANCE,RELIANCE26NOV1400CE,CE,500,1479967200,140000.00,5,,RELIANCE 24NOV26 1400 CE\n\
41223,nse_fo,OPTSTK,RELIANCE,RELIANCE26NOV1400PE,PE,500,1479967200,140000.00,5,,RELIANCE 24NOV26 1400 PE\n\
41101,nse_fo,OPTSTK,RELIANCE,RELIANCE26OCT1400CE,CE,500,1477548000,140000.00,5,,RELIANCE 27OCT26 1400 CE\n\
43001,nse_fo,FUTSTK,RELIANCE,RELIANCE26NOVFUT,XX,500,1479967200,0.00,5,,RELIANCE 24NOV26 FUT\n\
46001,nse_fo,OPTSTK,M&M,M&M26NOV2800CE,CE,350,1479967200,280000.00,5,,M&M 24NOV26 2800 CE\n\
46101,nse_fo,OPTSTK,M&MFIN,M&MFIN26NOV300CE,CE,2000,1479967200,30000.00,5,,M&MFIN 24NOV26 300 CE\n\
49001,nse_fo,OPTIDX,NIFTYNXT50,NIFTYNXT5026NOV70000CE,CE,25,1479967200,7000000.00,5,,NIFTYNXT50 24NOV26 70000 CE\n\
50001,nse_fo,OPTSTK,WEIRD,WEIRD26NOV500CE,CE,100,1479967200,12345.00,5,,WEIRD 24NOV26 500 CE\n";
    const BSE_FO: &str = "pSymbol,pExchSeg,pInstType,pSymbolName,pTrdSymbol,pOptionType,lLotSize,lExpiryDate,dStrikePrice;,dTickSize ,pISIN,pDesc\n\
874002,bse_fo,OPTIDX,SENSEX,SENSEX26N2675100PE,PE,20,1795651200,7510000.00,5,,SENSEX 26NOV26 75100 PE\n\
876001,bse_fo,OPTSTK,TCS,TCS26NOV3000CE,CE,175,1795478400,300000.00,5,,TCS 24NOV26 3000 CE\n";
    const NSE_CM: &str = "pSymbol,pExchSeg,pInstType,pSymbolName,pTrdSymbol,pOptionType,lLotSize,lExpiryDate,dStrikePrice;,dTickSize ,pISIN,pDesc\n\
2885,nse_cm,EQ,RELIANCE,RELIANCE-EQ,,1,,0.00,5,INE002A01018,Reliance Industries\n";

    fn store() -> ScripStore {
        let mut store = ScripStore::parse_csv(NSE_FO, "nse_fo");
        store.merge(ScripStore::parse_csv(BSE_FO, "bse_fo"));
        store.merge(ScripStore::parse_csv(NSE_CM, "nse_cm"));
        store
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn spot(underlying: &str) -> Option<String> {
        (underlying == "RELIANCE").then(|| "nse_cm|2885".to_string())
    }

    fn err(result: Result<ContractRef, Refusal>) -> (StatusCode, String) {
        let r = result.unwrap_err();
        (r.status, r.message)
    }

    #[test]
    fn index_options_resolve_as_before_whatever_the_switch_says() {
        let store = store();
        let today = d(2026, 11, 10);
        for stock_paper in [false, true] {
            let c = resolve(&store, " NIFTY26NOV23000CE ", today, stock_paper, &spot).unwrap();
            assert_eq!((c.index.as_str(), c.strike, c.option_type.as_str(), c.lot_size), ("NIFTY", 23_000.0, "CE", 65));
            assert_eq!((c.ws_key.as_str(), c.segment.as_str(), c.underlying_key, c.expiry), ("nse_fo|52001", "nse_fo", None, d(2026, 11, 24)));
            let s = resolve(&store, "SENSEX26N2675100PE", today, stock_paper, &spot).unwrap();
            assert_eq!((s.index.as_str(), s.segment.as_str(), s.underlying_key, s.expiry), ("SENSEX", "bse_fo", None, d(2026, 11, 26)));
            // An index future, and an index option that has expired, never.
            let (status, msg) = err(resolve(&store, "NIFTY26NOVFUT", today, stock_paper, &spot));
            assert_eq!((status, msg.as_str()), (StatusCode::BAD_REQUEST, "NIFTY26NOVFUT is a future, not an option — only options are bought on paper"));
            assert_eq!(err(resolve(&store, "NIFTY26N0323000CE", today, stock_paper, &spot)).1, "NIFTY26N0323000CE expired on 03 Nov 2026");
        }
    }

    #[test]
    fn a_stock_option_resolves_only_with_the_switch_on() {
        let store = store();
        let today = d(2026, 11, 10);
        let (status, msg) = err(resolve(&store, "RELIANCE26NOV1400CE", today, false, &spot));
        assert_eq!((status, msg.as_str()), (StatusCode::BAD_REQUEST, "RELIANCE26NOV1400CE is not a listed index option"));

        let c = resolve(&store, "RELIANCE26NOV1400CE", today, true, &spot).unwrap();
        assert_eq!((c.index.as_str(), c.trading_symbol.as_str(), c.strike, c.option_type.as_str()), ("RELIANCE", "RELIANCE26NOV1400CE", 1_400.0, "CE"));
        assert_eq!((c.ws_key.as_str(), c.lot_size, c.expiry, c.segment.as_str()), ("nse_fo|41222", 500, d(2026, 11, 24), "nse_fo"));
        assert_eq!(c.underlying_key.as_deref(), Some("nse_cm|2885"));
        let p = resolve(&store, "RELIANCE26NOV1400PE", today, true, &spot).unwrap();
        assert_eq!((p.option_type.as_str(), p.ws_key.as_str()), ("PE", "nse_fo|41223"));
        // On its expiry day it still resolves (the route refuses it from 15:38 by `squaring_off`).
        assert!(resolve(&store, "RELIANCE26NOV1400CE", d(2026, 11, 24), true, &spot).is_ok());
        // An index the app only charts: an option on it, with no cash row to watch.
        let nxt = resolve(&store, "NIFTYNXT5026NOV70000CE", today, true, &spot).unwrap();
        assert_eq!((nxt.index.as_str(), nxt.strike, nxt.underlying_key), ("NIFTYNXT50", 70_000.0, None));
    }

    #[test]
    fn never_a_future_a_stock_an_expired_or_a_bse_contract() {
        let store = store();
        let today = d(2026, 11, 10);
        let refused = |symbol: &str| err(resolve(&store, symbol, today, true, &spot));
        assert_eq!(refused("RELIANCE26NOVFUT").1, "RELIANCE26NOVFUT is a future, not an option — only options are bought on paper");
        assert_eq!(refused("RELIANCE-EQ").1, "RELIANCE-EQ is a stock, not an option — only options are bought on paper");
        assert_eq!(refused("RELIANCE26OCT1400CE").1, "RELIANCE26OCT1400CE expired on 27 Oct 2026");
        // Surrounding spaces are trimmed, as the owner's buy trims them.
        assert!(resolve(&store, "RELIANCE26NOV1400CE  ", today, true, &spot).is_ok());
        // A stock option listed on BSE only: NSE only.
        assert_eq!(refused("TCS26NOV3000CE").1, "TCS26NOV3000CE trades on BSE — only NSE stock options are bought on paper");
        // The strike heuristic reads nothing sensible: refused, not guessed.
        assert_eq!(refused("WEIRD26NOV500CE").1, "Can't read the strike of WEIRD26NOV500CE");
        // Unknown, blank, or an index option that has expired.
        assert_eq!(refused("TATAMOTORS26NOV700CE").1, "TATAMOTORS26NOV700CE is not a listed option");
        assert_eq!(refused("   ").1, "trading_symbol is required");
        // The day after its expiry, yesterday's contract is gone.
        assert_eq!(err(resolve(&store, "RELIANCE26NOV1400CE", d(2026, 11, 25), true, &spot)).1, "RELIANCE26NOV1400CE expired on 24 Nov 2026");
    }

    #[test]
    fn the_longest_underlying_name_wins_and_ampersands_are_fine() {
        let store = store();
        let today = d(2026, 11, 10);
        let mm = resolve(&store, "M&M26NOV2800CE", today, true, &spot).unwrap();
        assert_eq!((mm.index.as_str(), mm.strike, mm.lot_size), ("M&M", 2_800.0, 350));
        let fin = resolve(&store, "M&MFIN26NOV300CE", today, true, &spot).unwrap();
        assert_eq!((fin.index.as_str(), fin.strike, fin.lot_size), ("M&MFIN", 300.0, 2_000));
        // No cash row in the catalog for them: no underlying to watch, the premium still is.
        assert_eq!((mm.underlying_key, fin.underlying_key), (None, None));
    }
}
