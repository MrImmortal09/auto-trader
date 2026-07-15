use shared_domain::TradeSignal;
use trading_engine::ScripStore;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut store = ScripStore::default();
    
    // Mock NSE CSV (no BHEL option)
    let nse_csv = "instrumentToken,instrumentName,name,lastPrice,expiry,strike,tickSize,lotSize,instrumentType,optionType,exchange\n\
                   1001,BHEL,BHEL,100,30-Jul-2026,440.0,0.05,2625,OPTSTK,CE,nse_fo";
                   
    // Mock BSE CSV
    let bse_csv = "instrumentToken,instrumentName,name,lastPrice,expiry,strike,tickSize,lotSize,instrumentType,optionType,exchange\n\
                   2001,BHEL,BHEL,100,30-Jul-2026,440.0,0.05,2625,OPTSTK,CE,bse_fo";
    
    store.merge(ScripStore::parse_csv(nse_csv, "nse_fo"));
    store.merge(ScripStore::parse_csv(bse_csv, "bse_fo"));

    println!("Store BHEL records: {:?}", store.records.get("BHEL").map(|v| v.len()));
    if let Some(records) = store.records.get("BHEL") {
        for r in records {
            println!("  Record: {} {} {} {}", r.exchange_segment_code, r.strike_price, r.option_type, r.expiry_date);
        }
    }

    let signal = TradeSignal {
        instrument_name: "BHEL".to_string(),
        strike: Some(440.0),
        option_type: Some("CE".to_string()),
        expiry: Some("30-JUL-2026".to_string()),
        action: "BUY".to_string(),
        entry_condition: "ABOVE".to_string(),
        entry_price: 5.1,
        targets: vec![7.0, 10.0],
        stop_loss: 4.0,
        source: "telegram".to_string(),
        signal_id: None,
    };

    if let Some(record) = store.resolve_signal(&signal) {
        println!("Resolved to: {}", record.exchange_segment_code);
    } else {
        println!("No record found!");
    }
}
