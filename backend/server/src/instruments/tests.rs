//! The catalog end to end on synthetic CSVs in Kotak's shape: the load, the
//! atomic swap, the ranking, the typed-contract grammar, and that the
//! catalog reads strikes and expiries exactly as the engine's parser does.

use std::collections::HashSet;
use std::path::PathBuf;

use chrono::NaiveDate;
use trading_engine::ScripStore;

use super::load::{expiry_of, strike_of, Kind};
use super::*;
use crate::scrip_files::ScripFiles;

/// Kotak's documented header, with the stray `dStrikePrice;` and the padded
/// `dTickSize ` the real files have (ticks in paise, like strikes). nse_fo:
/// the Unix epoch minus 315511200.
const NSE_FO: &str = "pSymbol,pExchSeg,pInstType,pSymbolName,pTrdSymbol,pOptionType,lLotSize,lExpiryDate,dStrikePrice;,dTickSize ,pISIN,pDesc\r\n\
51380,nse_fo,OPTIDX,NIFTY,NIFTY26O2722900CE,CE,65,1477548000,2290000.00,5,,NIFTY 27OCT26 22900 CE\r\n\
51386,nse_fo,OPTIDX,NIFTY,NIFTY26N0323000CE,CE,65,1478152800,2300000.00,5,,NIFTY 03NOV26 23000 CE\r\n\
51387,nse_fo,OPTIDX,NIFTY,NIFTY26N0323000PE,PE,65,1478152800,2300000.00,5,,NIFTY 03NOV26 23000 PE\r\n\
51388,nse_fo,OPTIDX,NIFTY,NIFTY26N0323050CE,CE,65,1478152800,2305000.00,5,,NIFTY 03NOV26 23050 CE\r\n\
51389,nse_fo,OPTIDX,NIFTY,NIFTY26N0323050PE,PE,65,1478152800,2305000.00,5,,NIFTY 03NOV26 23050 PE\r\n\
51390,nse_fo,OPTIDX,NIFTY,NIFTY26N0322950CE,CE,65,1478152800,2295000.00,5,,NIFTY 03NOV26 22950 CE\r\n\
51391,nse_fo,OPTIDX,NIFTY,NIFTY26N0322950PE,PE,65,1478152800,2295000.00,5,,NIFTY 03NOV26 22950 PE\r\n\
\r\n\
52001,nse_fo,OPTIDX,NIFTY,NIFTY26NOV23000CE,CE,65,1479967200,2300000.00,5,,NIFTY 24NOV26 23000 CE\r\n\
52002,nse_fo,OPTIDX,NIFTY,NIFTY26NOV23000PE,PE,65,1479967200,2300000.00,5,,NIFTY 24NOV26 23000 PE\r\n\
52003,nse_fo,OPTIDX,NIFTY,NIFTY26NOV23100CE,CE,65,1479967200,2310000.00,5,,NIFTY 24NOV26 23100 CE\r\n\
52004,nse_fo,OPTIDX,NIFTY,NIFTY26NOV22900PE,PE,65,1479967200,2290000.00,5,,NIFTY 24NOV26 22900 PE\r\n\
52005,nse_fo,OPTIDX,NIFTY,NIFTY26DEC23000CE,CE,65,1482991200,2300000.00,5,,NIFTY 29DEC26 23000 CE\r\n\
35013,nse_fo,FUTIDX,NIFTY,NIFTY26NOVFUT,XX,65,1479967200,0.00,5,,NIFTY 24NOV26 FUT\r\n\
35014,nse_fo,FUTIDX,NIFTY,NIFTY26DECFUT,XX,65,1482991200,0.00,5,,NIFTY 29DEC26 FUT\r\n\
44001,nse_fo,OPTIDX,BANKNIFTY,BANKNIFTY26NOV45300CE,CE,30,1479967200,45300.00,5,,BANKNIFTY 24NOV26 45300 CE\r\n\
44002,nse_fo,OPTIDX,BANKNIFTY,BANKNIFTY26NOV45300PE,PE,30,1479967200,45300.00,5,,BANKNIFTY 24NOV26 45300 PE\r\n\
\"41222\",nse_fo,OPTSTK,RELIANCE,RELIANCE26NOV1400CE,CE,500,1479967200,140000.00,5,,RELIANCE 24NOV26 1400 CE\r\n\
41223,nse_fo,OPTSTK,RELIANCE,RELIANCE26NOV1400PE,PE,500,1479967200,140000.00,5,,RELIANCE 24NOV26 1400 PE\r\n\
41224,nse_fo,OPTSTK,RELIANCE,RELIANCE26NOV1420CE,CE,500,1479967200,142000.00,5,,RELIANCE 24NOV26 1420 CE\r\n\
41225,nse_fo,OPTSTK,RELIANCE,RELIANCE26NOV1420PE,PE,500,1479967200,142000.00,5,,RELIANCE 24NOV26 1420 PE\r\n\
41226,nse_fo,OPTSTK,RELIANCE,RELIANCE26NOV1380CE,CE,500,1479967200,138000.00,5,,RELIANCE 24NOV26 1380 CE\r\n\
41227,nse_fo,OPTSTK,RELIANCE,RELIANCE26NOV1380PE,PE,500,1479967200,138000.00,5,,RELIANCE 24NOV26 1380 PE\r\n\
41228,nse_fo,OPTSTK,RELIANCE,RELIANCE26NOV1440CE,CE,500,1479967200,144000.00,5,,RELIANCE 24NOV26 1440 CE\r\n\
41229,nse_fo,OPTSTK,RELIANCE,RELIANCE26DEC1400CE,CE,500,1482991200,140000.00,5,,RELIANCE 29DEC26 1400 CE\r\n\
41230,nse_fo,OPTSTK,RELIANCE,RELIANCE26DEC1400PE,PE,500,1482991200,140000.00,5,,RELIANCE 29DEC26 1400 PE\r\n\
43001,nse_fo,FUTSTK,RELIANCE,RELIANCE26NOVFUT,XX,500,1479967200,0.00,5,,RELIANCE 24NOV26 FUT\r\n\
43002,nse_fo,FUTSTK,RELIANCE,RELIANCE26DECFUT,XX,500,1482991200,0.00,5,,RELIANCE 29DEC26 FUT\r\n\
45001,nse_fo,OPTSTK,IDEA,IDEA26NOV7.5CE,CE,80000,1479967200,750.00,5,,IDEA 24NOV26 7.5 CE\r\n\
45002,nse_fo,OPTSTK,IDEA,IDEA26NOV7.5PE,PE,80000,1479967200,750.00,5,,IDEA 24NOV26 7.5 PE\r\n\
46001,nse_fo,OPTSTK,M&M,M&M26NOV2800CE,CE,350,1479967200,280000.00,5,,M&M 24NOV26 2800 CE\r\n\
46002,nse_fo,OPTSTK,M&M,M&M26NOV2800PE,PE,350,1479967200,280000.00,5,,M&M 24NOV26 2800 PE\r\n\
47001,nse_fo,OPTSTK,BAJAJ-AUTO,BAJAJ-AUTO26NOV9000CE,CE,75,1479967200,900000.00,5,,BAJAJ-AUTO 24NOV26 9000 CE\r\n\
47002,nse_fo,FUTSTK,BAJAJ-AUTO,BAJAJ-AUTO26NOVFUT,XX,75,1479967200,0.00,5,,BAJAJ-AUTO 24NOV26 FUT\r\n\
48001,nse_fo,OPTSTK,TATAMOTORS,TATAMOTORS26NOV700CE,CE,1400,1479967200,70000.00,5,,TATAMOTORS 24NOV26 700 CE\r\n\
48002,nse_fo,FUTSTK,TATAMOTORS,TATAMOTORS26NOVFUT,XX,1400,1479967200,0.00,5,,TATAMOTORS 24NOV26 FUT\r\n\
49001,nse_fo,OPTIDX,NIFTYNXT50,NIFTYNXT5026NOV70000CE,CE,25,1479967200,7000000.00,5,,NIFTYNXT50 24NOV26 70000 CE\r\n\
50001,nse_fo,OPTSTK,WEIRD,WEIRD26NOV500CE,CE,100,1479967200,12345.00,5,,WEIRD 24NOV26 500 CE\r\n\
50002,nse_fo,OPTSTK,NOEXP,NOEXP26NOV100CE,CE,100,,10000.00,5,,NOEXP 24NOV26 100 CE\r\n\
50003,nse_fo,,ODD,ODD26NOVXYZ,XX,100,1479967200,0.00,5,,\r\n\
41222,nse_fo,OPTSTK,RELIANCE,RELIANCE26NOV1400CE,CE,500,1479967200,140000.00,5,,a second row with the first token\r\n";

/// bse_fo: the plain epoch; SENSEX and BANKEX index contracts, and a stock
/// option the catalog does not take from BSE.
const BSE_FO: &str = "pSymbol,pExchSeg,pInstType,pSymbolName,pTrdSymbol,pOptionType,lLotSize,lExpiryDate,dStrikePrice;,dTickSize ,pISIN,pDesc\n\
874001,bse_fo,OPTIDX,SENSEX,SENSEX26N0575100CE,CE,20,1793836800,7510000.00,5,,SENSEX 05NOV26 75100 CE\n\
874002,bse_fo,OPTIDX,SENSEX,SENSEX26N0575100PE,PE,20,1793836800,7510000.00,5,,SENSEX 05NOV26 75100 PE\n\
874003,bse_fo,FUTIDX,SENSEX,SENSEX26NOVFUT,XX,20,1795651200,0.00,5,,SENSEX 26NOV26 FUT\n\
875001,bse_fo,OPTIDX,BANKEX,BANKEX26NOV60000CE,CE,30,1795651200,6000000.00,5,,BANKEX 26NOV26 60000 CE\n\
876001,bse_fo,OPTSTK,RELIANCE,RELIANCE26NOV1400CE,CE,500,1795478400,140000.00,5,,RELIANCE 24NOV26 1400 CE";

/// nse_cm: the cash header has no strike, option or expiry column; series
/// are the trading symbol's suffix.
const NSE_CM: &str = "pSymbol,pExchSeg,pInstType,pSymbolName,pTrdSymbol,lLotSize,dTickSize ,pISIN,pDesc\n\
2885,nse_cm,EQ,RELIANCE,RELIANCE-EQ,1,5,INE002A01018,Reliance Industries\n\
11536,nse_cm,EQ,TCS,TCS-EQ,1,5,INE467B01029,Tata Consultancy Services\n\
2031,nse_cm,EQ,M&M,M&M-EQ,1,5,INE101A01026,Mahindra & Mahindra\n\
16669,nse_cm,EQ,BAJAJ-AUTO,BAJAJ-AUTO-EQ,1,5,INE917I01010,Bajaj Auto\n\
10604,nse_cm,EQ,NIFTYBEES,NIFTYBEES-EQ,1,1,INF204KB14I2,Nippon India ETF Nifty 50 BeES\n\
3456,nse_cm,EQ,TATAMOTORS,TATAMOTORS-EQ,1,5,INE155A01022,Tata Motors\n\
14366,nse_cm,EQ,IDEA,IDEA-EQ,1,1,INE669E01016,Vodafone Idea\n\
99002,nse_cm,BE,TFTSTOCK,TFTSTOCK-BE,1,5,INE000000002,Trade For Trade Co\n\
99001,nse_cm,EQ,SMEONE,SMEONE-SM,1,5,INE000000001,Some SME\n\
99003,nse_cm,EQ,JUNK,JUNK-BZ,1,5,,Junk Series\n\
99004,nse_cm,,Nifty 50,Nifty 50,1,5,,\n\
99005,nse_cm,EQ,XA01,XA01-EQ,1,5,,Xa 01\n\
99006,nse_cm,EQ,XA02,XA02-EQ,1,5,,Xa 02\n\
99007,nse_cm,EQ,XA03,XA03-EQ,1,5,,Xa 03\n\
99008,nse_cm,EQ,XA04,XA04-EQ,1,5,,Xa 04\n\
99009,nse_cm,EQ,XA05,XA05-EQ,1,5,,Xa 05\n\
99010,nse_cm,EQ,XA06,XA06-EQ,1,5,,Xa 06\n\
99011,nse_cm,EQ,XA07,XA07-EQ,1,5,,Xa 07\n\
99012,nse_cm,EQ,XA08,XA08-EQ,1,5,,Xa 08\n\
99013,nse_cm,EQ,XA09,XA09-EQ,1,5,,Xa 09\n\
99014,nse_cm,EQ,XA10,XA10-EQ,1,5,,Xa 10\n";

/// A cash file as another day's export might come: a BOM, the columns in
/// another order with unknown ones among them, `dTickSize` padded, no
/// `pDesc` and no `pISIN`, and a quoted name with a comma.
const NSE_CM_OTHER_SHAPE: &str = "\u{feff}pGroup,pSymbol,pExchSeg,lFreezeQty,pTrdSymbol,pSymbolName,dTickSize ,lLotSize,pScripRefKey\n\
EQ,2885,nse_cm,0,RELIANCE-EQ,RELIANCE,5,1,x\n\
EQ,500,nse_cm,0,\"LT-EQ\",LT,5,1,x\n";

/// The real files' shape, from the 2026-11-04 scrip master: the header's
/// first 21 columns verbatim (the files go on for 58 more the catalog never
/// reads — surveillance text and the like), no BOM, LF endings. Cash rows
/// carry the series in `pGroup` and as the `-EQ`/`-BE` suffix, a blank
/// `pInstType`, `-1` in the expiry, multiplier and strike, and ticks in
/// paise (`10` on RELIANCE, `1` on IDEA, `100` on BAJAJ-AUTO); the four
/// index rows have no suffix and no series. nse_fo rows have `pDesc` blank
/// on every row, `pGroup` `XX`, strikes as plain integers or in scientific
/// notation, `-1` for a future's strike, and the epoch minus 315511200
/// landing on 14:00 IST of the expiry day. bse_fo says `IO`/`SO`/`IF`/`SF`
/// for `pInstType`, never OPTIDX, with the plain epoch at 23:59:59 IST.
const REAL_HEADER: &str = "pSymbol,pGroup,pExchSeg,pInstType,pSymbolName,pTrdSymbol,pOptionType,pScripRefKey,pISIN,pAssetCode,pSubGroup,pCombinedSymbol,pDesc,pAmcCode,pContractId,dTickSize ,lLotSize,lExpiryDate ,lMultiplier ,lPrecision,dStrikePrice;";

const REAL_NSE_CM: &str = "pSymbol,pGroup,pExchSeg,pInstType,pSymbolName,pTrdSymbol,pOptionType,pScripRefKey,pISIN,pAssetCode,pSubGroup,pCombinedSymbol,pDesc,pAmcCode,pContractId,dTickSize ,lLotSize,lExpiryDate ,lMultiplier ,lPrecision,dStrikePrice;\n\
2885,EQ,nse_cm,,RELIANCE,RELIANCE-EQ,,RELIANCE,INE002A01018,,,,RELIANCE INDUSTRIES LTD,,,10,1,-1,-1,2,-1\n\
1333,EQ,nse_cm,,HDFCBANK,HDFCBANK-EQ,,HDFCBANK,INE040A01034,,,,HDFC BANK LTD,,,5,1,-1,-1,2,-1\n\
14366,EQ,nse_cm,,IDEA,IDEA-EQ,,IDEA,INE669E01016,,,,VODAFONE IDEA LIMITED,,,1,1,-1,-1,2,-1\n\
16669,EQ,nse_cm,,BAJAJ-AUTO,BAJAJ-AUTO-EQ,,BAJAJ-AUTO,INE917I01010,,,,BAJAJ AUTO LIMITED,,,100,1,-1,-1,2,-1\n\
26000,,nse_cm,,Nifty 50,Nifty 50,,Nifty 50,,,,,Nifty 50,,,5,1,-1,-1,2,-1\n\
99999,SM,nse_cm,,SMECO,SMECO-SM,,SMECO,INE000000001,,,,SOME SME CO,,,5,1,-1,-1,2,-1\n\
\n";

const REAL_NSE_FO: &str = "pSymbol,pGroup,pExchSeg,pInstType,pSymbolName,pTrdSymbol,pOptionType,pScripRefKey,pISIN,pAssetCode,pSubGroup,pCombinedSymbol,pDesc,pAmcCode,pContractId,dTickSize ,lLotSize,lExpiryDate ,lMultiplier ,lPrecision,dStrikePrice;\n\
52001,XX,nse_fo,OPTIDX,NIFTY,NIFTY26N0917000CE,CE,NIFTY,,,,,,,,5,65,1478701800,1,2,1700000\n\
49001,XX,nse_fo,OPTIDX,NIFTYNXT50,NIFTYNXT5026NOV69600CE,CE,NIFTYNXT50,,,,,,,,5,25,1479997800,1,2,6.96e+06\n\
41222,XX,nse_fo,OPTSTK,RELIANCE,RELIANCE26NOV1280CE,CE,RELIANCE,,,,,,,,5,500,1479997800,1,2,128000\n\
43001,XX,nse_fo,FUTSTK,RELIANCE,RELIANCE26NOVFUT,XX,RELIANCE,,,,,,,,5,500,1479997800,1,2,-1\n\
45001,XX,nse_fo,OPTSTK,IDEA,IDEA26NOV7.5CE,CE,IDEA,,,,,,,,5,80000,1479997800,1,2,750\n\
\n";

const REAL_BSE_FO: &str = "pSymbol,pGroup,pExchSeg,pInstType,pSymbolName,pTrdSymbol,pOptionType,pScripRefKey,pISIN,pAssetCode,pSubGroup,pCombinedSymbol,pDesc,pAmcCode,pContractId,dTickSize ,lLotSize,lExpiryDate ,lMultiplier ,lPrecision,dStrikePrice;\n\
874001,XX,bse_fo,IO,SENSEX,SENSEX26N0564600CE,CE,SENSEX,,,,,,,,5,20,1793903399,1,2,6460000\n\
874003,XX,bse_fo,IF,SENSEX,SENSEX26NOVFUT,XX,SENSEX,,,,,,,,5,20,1795717799,1,2,-1\n\
876001,XX,bse_fo,SO,RELIANCE,RELIANCE26NOV1280CE,CE,RELIANCE,,,,,,,,5,500,1795544999,1,2,128000\n\
876002,XX,bse_fo,SF,RELIANCE,RELIANCE26NOVFUT,XX,RELIANCE,,,,,,,,5,500,1795544999,1,2,-1\n\
\n";

/// A scratch `data/` directory of its own: `scrip/` for the CSVs and the
/// catalog database beside it. Removed when dropped. The route tests
/// (`routes::v2::instruments`) build their catalog with it too.
pub(crate) struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    pub(crate) fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("auto-trader-catalog-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("scrip")).unwrap();
        Scratch { dir }
    }

    fn files(&self) -> ScripFiles {
        ScripFiles::new(self.dir.join("scrip"))
    }

    fn db(&self) -> PathBuf {
        self.dir.join("instruments.db")
    }

    fn write(&self, segment: &str, csv: &str) {
        // A rewrite within the same clock tick would look unchanged.
        std::thread::sleep(std::time::Duration::from_millis(5));
        self.files().write(segment, csv).unwrap();
    }

    pub(crate) fn write_all(&self) {
        self.write("nse_cm", NSE_CM);
        self.write("nse_fo", NSE_FO);
        self.write("bse_fo", BSE_FO);
    }

    /// Stamps every segment file on disk as written at `at`, as a download
    /// at that time would have.
    fn stamp(&self, at: chrono::DateTime<chrono::FixedOffset>) {
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(u64::try_from(at.timestamp()).unwrap());
        for segment in ["nse_cm", "nse_fo", "bse_fo"] {
            if let Ok(file) = std::fs::File::options().write(true).open(self.files().path(segment)) {
                file.set_modified(when).unwrap();
            }
        }
    }

    fn write_real_shape(&self) {
        self.write("nse_cm", REAL_NSE_CM);
        self.write("nse_fo", REAL_NSE_FO);
        self.write("bse_fo", REAL_BSE_FO);
    }

    /// A catalog with the `search` switch on.
    pub(crate) async fn catalog(&self) -> Arc<Catalog> {
        self.catalog_with(FeatureFlags { search: true, ..Default::default() }).await
    }

    async fn catalog_with(&self, flags: FeatureFlags) -> Arc<Catalog> {
        Catalog::open(self.files(), &self.db(), Arc::new(tokio::sync::RwLock::new(flags))).await
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

/// 2026-11-04: the weekly NIFTY expiry of the 3rd is gone, the 24th is next.
fn today() -> NaiveDate {
    d(2026, 11, 4)
}

fn no_spot(_: &str) -> Option<f64> {
    None
}

fn loaded(outcome: Result<LoadOutcome, LoadError>) -> LoadReport {
    match outcome {
        Ok(LoadOutcome::Loaded(report)) => report,
        Ok(LoadOutcome::Unchanged) => panic!("expected a build, the catalog said unchanged"),
        Ok(LoadOutcome::SameDay) => panic!("expected a build, the catalog waited for the close"),
        Err(e) => panic!("expected a build: {e}"),
    }
}

async fn ids(catalog: &Catalog, sql: &str) -> Vec<String> {
    sqlx::query_scalar::<_, String>(sql).fetch_all(catalog.pool.as_ref().unwrap()).await.unwrap()
}

async fn count(catalog: &Catalog, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sql).fetch_one(catalog.pool.as_ref().unwrap()).await.unwrap()
}

fn symbols(rows: &[SearchRow]) -> Vec<&str> {
    rows.iter().map(|r| r.trading_symbol.as_str()).collect()
}

fn empty(answer: &SearchAnswer) -> bool {
    answer.indices.is_empty() && answer.stocks.is_empty() && answer.futures.is_empty() && answer.options.is_empty()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fts5_is_compiled_into_the_bundled_sqlite() {
    let scratch = Scratch::new("fts5");
    let (pool, fts) = crate::db::init_catalog_db(&scratch.db()).await.unwrap();
    assert!(fts, "the bundled SQLite is built with SQLITE_ENABLE_FTS5");
    sqlx::query("CREATE VIRTUAL TABLE probe USING fts5(symbol, name)").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO probe (symbol, name) VALUES ('TCS', 'Tata Consultancy Services')").execute(&pool).await.unwrap();
    let hit: String = sqlx::query_scalar("SELECT symbol FROM probe WHERE probe MATCH '\"tata\"* \"cons\"*'").fetch_one(&pool).await.unwrap();
    assert_eq!(hit, "TCS");
    // Opening again is additive: nothing is dropped or recreated.
    let (pool2, fts2) = crate::db::init_catalog_db(&scratch.db()).await.unwrap();
    assert!(fts2);
    let still: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM probe").fetch_one(&pool2).await.unwrap();
    assert_eq!(still, 1);
    pool.close().await;
    pool2.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_fixture_loads_into_the_catalog() {
    let scratch = Scratch::new("load");
    let catalog = scratch.catalog().await;
    assert!(matches!(catalog.load_if_changed("test").await, Err(LoadError::NoFiles)));
    assert_eq!(catalog.status().rows.len(), 0);
    scratch.write_all();

    let report = loaded(catalog.load_if_changed("test").await);
    let by_segment = |segment: &str| report.segments.iter().find(|r| r.segment == segment).unwrap();
    // nse_cm: 7 EQ stocks and ETFs, 1 BE, 10 filler equities; SME, BZ and the index line are out.
    let cm = by_segment("nse_cm");
    assert_eq!((cm.kept, cm.skipped), (18, 3));
    assert_eq!(cm.skip_reasons.get("series not EQ or BE"), Some(&2));
    assert_eq!(cm.skip_reasons.get("no series"), Some(&1));
    assert!(cm.missing.is_empty(), "{:?}", cm.missing);
    assert!(cm.header.starts_with("pSymbol,pExchSeg"));
    // nse_fo: every contract but the one with no expiry and the one that is neither;
    // the repeated token is a duplicate, the unreadable strike a note.
    let fo = by_segment("nse_fo");
    assert_eq!((fo.kept, fo.skipped, fo.duplicates, fo.unresolved_strikes), (38, 2, 1, 1));
    assert_eq!(fo.skip_reasons.get("no expiry"), Some(&1));
    assert_eq!(fo.skip_reasons.get("neither an option nor a future"), Some(&1));
    // bse_fo: the index contracts only.
    let bse = by_segment("bse_fo");
    assert_eq!((bse.kept, bse.skipped), (4, 1));
    assert_eq!(bse.skip_reasons.get("bse_fo contract on a stock"), Some(&1));
    assert_eq!(report.index_rows, 6);

    let status = catalog.status();
    assert_eq!(status.rows.get("nse_cm"), Some(&18));
    assert_eq!(status.rows.get("nse_fo"), Some(&37), "the duplicate row is counted once in the table");
    assert_eq!(status.rows.get("bse_fo"), Some(&4));
    assert_eq!(status.rows.get("index"), Some(&6));
    assert_eq!(status.total_rows(), 65);
    // Per kind, as the files were read: 18 equities, 6 indices, the futures
    // and options of both F&O files (the duplicated token at each row).
    assert_eq!(fo.kinds.get("FUT"), Some(&6));
    assert_eq!((fo.kinds.get("CE"), fo.kinds.get("PE")), (Some(&20), Some(&12)));
    let kinds: Vec<(&str, i64)> = status.kinds.iter().map(|(k, n)| (k.as_str(), *n)).collect();
    assert_eq!(kinds, vec![("CE", 22), ("EQ", 18), ("FUT", 7), ("INDEX", 6), ("PE", 13)]);
    assert_eq!(status.kinds.values().sum::<i64>(), 66, "one more than the table: the duplicate");
    assert!(status.fts && !status.loading && status.last_error.is_none());
    assert_eq!(status.loaded_at.as_deref(), Some(report.loaded_at.as_str()));
    assert_eq!(status.day.as_deref(), Some(report.day.as_str()));
    assert_eq!(count(&catalog, "SELECT COUNT(*) FROM instruments").await, 65);
    assert_eq!(count(&catalog, "SELECT COUNT(*) FROM instrument_load").await, 3);
    assert_eq!(count(&catalog, "SELECT COUNT(*) FROM instruments_fts").await, status.underlyings as i64);

    // An equity row.
    let row: (String, String, String, String, String, Option<String>, Option<f64>, i64, f64, Option<String>, String, String) = sqlx::query_as(
        "SELECT id, trading_symbol, symbol, name, kind, expiry, strike, lot_size, tick_size, isin, exchange, ws_key FROM instruments WHERE id = 'nse_cm:2885'",
    )
    .fetch_one(catalog.pool.as_ref().unwrap())
    .await
    .unwrap();
    assert_eq!(row.0, "nse_cm:2885");
    assert_eq!((row.1.as_str(), row.2.as_str(), row.3.as_str(), row.4.as_str()), ("RELIANCE-EQ", "RELIANCE", "Reliance Industries", "EQ"));
    assert_eq!((row.5, row.6, row.7, row.8), (None, None, 1, 0.05));
    assert_eq!((row.9.as_deref(), row.10.as_str(), row.11.as_str()), (Some("INE002A01018"), "NSE", "nse_cm|2885"));
    // Symbols with `&` and `-` survive as they are, series stripped.
    assert_eq!(ids(&catalog, "SELECT symbol FROM instruments WHERE id IN ('nse_cm:2031', 'nse_cm:16669') ORDER BY id").await, vec!["BAJAJ-AUTO", "M&M"]);
    assert_eq!(ids(&catalog, "SELECT trading_symbol FROM instruments WHERE symbol = 'TFTSTOCK'").await, vec!["TFTSTOCK-BE"]);
    assert!(ids(&catalog, "SELECT id FROM instruments WHERE symbol IN ('SMEONE', 'JUNK')").await.is_empty());

    // Options: weekly and monthly expiries, strikes at ×100 and ×1, a half-rupee one, one unreadable.
    let strike = |id: &str| {
        let catalog = Arc::clone(&catalog);
        let id = id.to_string();
        async move {
            sqlx::query_as::<_, (Option<String>, Option<f64>, String, String, i64)>(
                "SELECT expiry, strike, underlying, kind, lot_size FROM instruments WHERE id = ?",
            )
            .bind(id)
            .fetch_one(catalog.pool.as_ref().unwrap())
            .await
            .unwrap()
        }
    };
    assert_eq!(strike("nse_fo:51386").await, (Some("2026-11-03".into()), Some(23_000.0), "NIFTY".into(), "CE".into(), 65));
    assert_eq!(strike("nse_fo:52002").await, (Some("2026-11-24".into()), Some(23_000.0), "NIFTY".into(), "PE".into(), 65));
    assert_eq!(strike("nse_fo:44001").await, (Some("2026-11-24".into()), Some(45_300.0), "BANKNIFTY".into(), "CE".into(), 30));
    assert_eq!(strike("nse_fo:41222").await, (Some("2026-11-24".into()), Some(1_400.0), "RELIANCE".into(), "CE".into(), 500));
    assert_eq!(strike("nse_fo:45001").await, (Some("2026-11-24".into()), Some(7.5), "IDEA".into(), "CE".into(), 80_000));
    assert_eq!(strike("nse_fo:50001").await, (Some("2026-11-24".into()), None, "WEIRD".into(), "CE".into(), 100));
    assert_eq!(strike("nse_fo:35013").await, (Some("2026-11-24".into()), None, "NIFTY".into(), "FUT".into(), 65));
    assert_eq!(strike("bse_fo:874001").await, (Some("2026-11-05".into()), Some(75_100.0), "SENSEX".into(), "CE".into(), 20));
    assert_eq!(ids(&catalog, "SELECT exchange FROM instruments WHERE id = 'bse_fo:875001'").await, vec!["BSE"]);
    assert!(ids(&catalog, "SELECT id FROM instruments WHERE id = 'bse_fo:876001'").await.is_empty(), "no stock contract from bse_fo");
    assert!(ids(&catalog, "SELECT id FROM instruments WHERE symbol = 'NOEXP'").await.is_empty());

    // The six virtual index rows, keyed as the feed names them.
    assert_eq!(
        ids(&catalog, "SELECT id FROM instruments WHERE kind = 'INDEX' ORDER BY id").await,
        vec!["bse_cm:BANKEX", "bse_cm:SENSEX", "nse_cm:NIFTY MID SELECT", "nse_cm:Nifty 50", "nse_cm:Nifty Bank", "nse_cm:Nifty Fin Service"]
    );
    assert_eq!(ids(&catalog, "SELECT ws_key FROM instruments WHERE id = 'nse_cm:Nifty 50'").await, vec!["idx|NIFTY 50"]);
    assert_eq!(ids(&catalog, "SELECT symbol FROM instruments WHERE id = 'bse_cm:SENSEX'").await, vec!["SENSEX"]);

    // The search list: equities, the six indices, F&O-only underlyings; F&O noted.
    let index = catalog.index();
    assert_eq!(index.len(), status.underlyings);
    let reliance = index.get("RELIANCE").unwrap();
    assert_eq!((reliance.has_fo, reliance.id.as_deref(), reliance.isin.as_deref()), (true, Some("nse_cm:2885"), Some("INE002A01018")));
    assert_eq!(reliance.fo_segment.as_deref(), Some("nse_fo"));
    assert_eq!(index.get("TCS").unwrap().has_fo, false);
    let nifty = index.get("NIFTY").unwrap();
    assert_eq!((nifty.kind, nifty.has_fo, nifty.id.as_deref()), (UnderlyingKind::Index, true, Some("nse_cm:Nifty 50")));
    let nxt = index.get("NIFTYNXT50").unwrap();
    assert_eq!((nxt.kind, nxt.fo_index, nxt.id.is_none(), nxt.group()), (UnderlyingKind::FoOnly, true, true, Group::Indices));
    let weird = index.get("WEIRD").unwrap();
    assert_eq!((weird.kind, weird.fo_index, weird.group()), (UnderlyingKind::FoOnly, false, Group::Stocks));
    assert!(index.get("SMEONE").is_none());

    // The same files again: nothing to do.
    assert!(matches!(catalog.load_if_changed("again").await, Ok(LoadOutcome::Unchanged)));
    assert_eq!(count(&catalog, "SELECT COUNT(*) FROM instruments").await, 65);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_restores_the_search_list_without_a_build() {
    let scratch = Scratch::new("restart");
    scratch.write_all();
    {
        let catalog = scratch.catalog().await;
        loaded(catalog.load_if_changed("first").await);
        catalog.pool.as_ref().unwrap().close().await;
    }
    let catalog = scratch.catalog().await;
    let status = catalog.status();
    assert_eq!(status.rows.get("nse_fo"), Some(&37));
    assert_eq!(status.rows.get("index"), Some(&6));
    assert_eq!((status.kinds.get("CE"), status.kinds.get("EQ"), status.kinds.get("INDEX")), (Some(&22), Some(&18), Some(&6)));
    assert!(status.day.is_some() && status.loaded_at.is_some());
    assert!(status.underlyings > 0);
    let answer = catalog.search("reliance", today(), &no_spot).await;
    assert_eq!(answer.stocks[0].id, "nse_cm:2885");
    assert_eq!(symbols(&answer.futures), vec!["RELIANCE26NOVFUT", "RELIANCE26DECFUT"]);
    assert!(matches!(catalog.load_if_changed("unchanged").await, Ok(LoadOutcome::Unchanged)));
}

/// IST wall-clock time.
fn ist(y: i32, m: u32, day: u32, h: u32, min: u32) -> chrono::DateTime<chrono::FixedOffset> {
    d(y, m, day).and_hms_opt(h, min, 0).unwrap().and_local_timezone(shared_domain::ist_offset()).single().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relogin_download_during_hours_is_not_built_again() {
    // The calendar's special sessions are process-wide: none while this runs.
    let _sessions = shared_domain::special_sessions_test_guard();
    let scratch = Scratch::new("same-day");
    let catalog = scratch.catalog().await;
    // 2026-11-04 is a Wednesday. The morning login's download (no bse_fo
    // that day) is built during hours: the catalog has nothing of the day.
    scratch.write("nse_cm", NSE_CM);
    scratch.write("nse_fo", NSE_FO);
    scratch.stamp(ist(2026, 11, 4, 9, 20));
    loaded(catalog.load_if_changed_at("login", ist(2026, 11, 4, 9, 21)).await);
    assert_eq!(catalog.status().day.as_deref(), Some("2026-11-04"));
    // A relogin brings a segment the build lacked: built.
    scratch.write_all();
    scratch.stamp(ist(2026, 11, 4, 11, 0));
    loaded(catalog.load_if_changed_at("relogin", ist(2026, 11, 4, 11, 1)).await);
    let built = catalog.status();
    assert_eq!(built.rows.get("bse_fo"), Some(&4));
    // The next relogin writes the same day's files again, newer mtimes and
    // all: not built while the market is open.
    scratch.write_all();
    scratch.stamp(ist(2026, 11, 4, 13, 0));
    assert!(matches!(catalog.load_if_changed_at("relogin", ist(2026, 11, 4, 13, 1)).await, Ok(LoadOutcome::SameDay)));
    let after = catalog.status();
    assert_eq!((after.loaded_at, after.rows, after.loading), (built.loaded_at.clone(), built.rows.clone(), false));
    // Once the market is shut the same files are built, and then unchanged.
    loaded(catalog.load_if_changed_at("after the close", ist(2026, 11, 4, 15, 45)).await);
    assert!(matches!(catalog.load_if_changed_at("again", ist(2026, 11, 4, 15, 46)).await, Ok(LoadOutcome::Unchanged)));
    // The next day's first download is built during hours.
    scratch.write_all();
    scratch.stamp(ist(2026, 11, 5, 9, 20));
    loaded(catalog.load_if_changed_at("next day", ist(2026, 11, 5, 9, 30)).await);
    assert_eq!(catalog.status().day.as_deref(), Some("2026-11-05"));
    // Without a clock (`load_if_changed`), a change is always built.
    scratch.write_all();
    scratch.stamp(ist(2026, 11, 5, 11, 0));
    loaded(catalog.load_if_changed("unconditional").await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_build_leaves_the_catalog_as_it_was_and_a_good_one_replaces_it_whole() {
    let scratch = Scratch::new("swap");
    scratch.write_all();
    let catalog = scratch.catalog().await;
    loaded(catalog.load_if_changed("first").await);
    let before = catalog.status();

    // nse_fo.csv becomes unreadable: a directory where the file was.
    std::fs::remove_file(scratch.files().path("nse_fo")).unwrap();
    std::fs::create_dir(scratch.files().path("nse_fo")).unwrap();
    let err = match catalog.load_if_changed("broken").await {
        Err(e) => e,
        Ok(_) => panic!("a build off an unreadable file must fail"),
    };
    assert!(matches!(err, LoadError::Io { segment: "nse_fo", .. }), "{err}");
    let after = catalog.status();
    assert_eq!((after.rows, after.loaded_at, after.day), (before.rows, before.loaded_at, before.day));
    assert_eq!(after.last_error.as_deref().map(|e| e.starts_with("nse_fo.csv")), Some(true));
    assert!(!after.loading);
    assert_eq!(count(&catalog, "SELECT COUNT(*) FROM instruments").await, 65);
    assert!(ids(&catalog, "SELECT name FROM sqlite_master WHERE name = 'instruments_new'").await.is_empty(), "no staging table is left behind");
    assert_eq!(catalog.search("nifty", today(), &no_spot).await.indices[0].symbol, "NIFTY");

    // A smaller nse_fo replaces everything: nothing of the old rows remains.
    std::fs::remove_dir(scratch.files().path("nse_fo")).unwrap();
    scratch.write(
        "nse_fo",
        "pSymbol,pExchSeg,pInstType,pSymbolName,pTrdSymbol,pOptionType,lLotSize,lExpiryDate,dStrikePrice;,pTickSize,pISIN,pDesc\n\
         43001,nse_fo,FUTSTK,RELIANCE,RELIANCE26NOVFUT,XX,500,1479967200,0.00,5,,RELIANCE 24NOV26 FUT\n",
    );
    let report = loaded(catalog.load_if_changed("smaller").await);
    assert_eq!(report.segments.iter().find(|r| r.segment == "nse_fo").unwrap().kept, 1);
    let status = catalog.status();
    assert_eq!(status.rows.get("nse_fo"), Some(&1));
    assert!(status.last_error.is_none());
    assert_eq!(count(&catalog, "SELECT COUNT(*) FROM instruments").await, 18 + 1 + 4 + 6);
    assert!(ids(&catalog, "SELECT id FROM instruments WHERE id = 'nse_fo:51386'").await.is_empty());
    // NIFTY keeps its index row but has no NSE contracts now; TATAMOTORS lost its F&O.
    assert!(catalog.index().get("NIFTY").unwrap().has_fo == false);
    assert!(!catalog.index().get("TATAMOTORS").unwrap().has_fo);
    assert!(catalog.search("nifty", today(), &no_spot).await.futures.is_empty());
    assert_eq!(symbols(&catalog.search("reliance", today(), &no_spot).await.futures), vec!["RELIANCE26NOVFUT"]);
    // The indexes carry the new name.
    let indexes = ids(&catalog, "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'instruments' AND name LIKE 'idx_%' ORDER BY name").await;
    assert_eq!(indexes, vec!["idx_instruments_chain", "idx_instruments_isin", "idx_instruments_symbol"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_ranks_and_groups_with_at_most_eight_per_group() {
    let scratch = Scratch::new("search");
    scratch.write_all();
    let catalog = scratch.catalog().await;
    loaded(catalog.load_if_changed("test").await);

    // Exact symbol first, then the prefix matches, F&O underlyings ahead.
    let answer = catalog.search("rel", today(), &no_spot).await;
    assert_eq!(answer.parsed, "plain");
    assert_eq!(answer.stocks.iter().map(|r| r.symbol.as_str()).collect::<Vec<_>>(), vec!["RELIANCE"]);
    assert_eq!((answer.stocks[0].kind, answer.stocks[0].has_fo, answer.stocks[0].exchange.as_str()), (Kind::Eq, true, "NSE"));
    assert_eq!(answer.stocks[0].tier, Some(super::search::Tier::SymbolPrefix));
    assert_eq!(answer.futures[0].tier, None);
    assert!(answer.indices.is_empty());

    // Name words through FTS: `tata cons` is TCS, `tata` lists the F&O stock first.
    let answer = catalog.search("tata cons", today(), &no_spot).await;
    assert_eq!(answer.stocks.iter().map(|r| r.symbol.as_str()).collect::<Vec<_>>(), vec!["TCS"]);
    let answer = catalog.search("tata", today(), &no_spot).await;
    assert_eq!(answer.stocks.iter().map(|r| r.symbol.as_str()).collect::<Vec<_>>(), vec!["TATAMOTORS", "TCS"]);
    assert_eq!(symbols(&answer.futures), vec!["TATAMOTORS26NOVFUT"]);
    assert_eq!(symbols(&answer.options), vec!["TATAMOTORS26NOV700CE"]);
    // The same answer with the word tier computed in memory.
    let index = catalog.index();
    let fts: HashSet<String> = catalog.fts_symbols("tata").await.unwrap();
    assert_eq!(fts, ["TATAMOTORS", "TCS"].iter().map(|s| s.to_string()).collect());
    let with_fts: Vec<&str> = index.rank("tata", Some(&fts)).iter().map(|h| h.entry.symbol.as_str()).collect();
    let in_memory: Vec<&str> = index.rank("tata", None).iter().map(|h| h.entry.symbol.as_str()).collect();
    assert_eq!(with_fts, in_memory);
    assert_eq!(with_fts, vec!["TATAMOTORS", "TCS"]);

    // Indices and stocks are separate groups: NIFTY exactly, then the indices named
    // after it (the one with contracts first); an F&O-only index underlying has no row.
    let answer = catalog.search("nifty", today(), &no_spot).await;
    assert_eq!(answer.indices.iter().map(|r| r.symbol.as_str()).collect::<Vec<_>>(), vec!["NIFTY", "BANKNIFTY", "FINNIFTY", "MIDCPNIFTY"]);
    assert_eq!(answer.indices[0].tier, Some(super::search::Tier::Exact));
    assert_eq!(answer.indices[1].tier, Some(super::search::Tier::WordPrefix));
    assert_eq!((answer.indices[0].id.as_str(), answer.indices[0].kind, answer.indices[0].ws_key.as_str()), ("nse_cm:Nifty 50", Kind::Index, "idx|NIFTY 50"));
    assert_eq!(answer.stocks.iter().map(|r| r.symbol.as_str()).collect::<Vec<_>>(), vec!["NIFTYBEES"]);
    assert_eq!(symbols(&answer.futures), vec!["NIFTY26NOVFUT", "NIFTY26DECFUT"]);
    // NIFTYNXT50 has no row of its own, but its options are still reachable.
    assert_eq!(symbols(&catalog.search("niftynxt", today(), &no_spot).await.options), vec!["NIFTYNXT5026NOV70000CE"]);
    // Symbols with `&` and `-`, exactly and by a word.
    assert_eq!(catalog.search("m&m", today(), &no_spot).await.stocks[0].symbol, "M&M");
    assert_eq!(symbols(&catalog.search("M&M", today(), &no_spot).await.options), vec!["M&M26NOV2800CE", "M&M26NOV2800PE"]);
    assert_eq!(catalog.search("auto", today(), &no_spot).await.stocks[0].symbol, "BAJAJ-AUTO");
    assert_eq!(symbols(&catalog.search("bajaj-auto", today(), &no_spot).await.futures), vec!["BAJAJ-AUTO26NOVFUT"]);
    // An ISIN finds the stock; a substring finds it last.
    assert_eq!(catalog.search("INE467B01029", today(), &no_spot).await.stocks[0].symbol, "TCS");
    assert_eq!(catalog.search("lianc", today(), &no_spot).await.stocks[0].symbol, "RELIANCE");
    // SENSEX options come from bse_fo with BSE on them.
    let answer = catalog.search("sensex", today(), &no_spot).await;
    assert_eq!(answer.indices[0].id, "bse_cm:SENSEX");
    assert_eq!(symbols(&answer.futures), vec!["SENSEX26NOVFUT"]);
    assert_eq!(symbols(&answer.options), vec!["SENSEX26N0575100CE", "SENSEX26N0575100PE"]);
    assert_eq!(answer.options[0].exchange, "BSE");
    // At most eight per group.
    let answer = catalog.search("xa", today(), &no_spot).await;
    assert_eq!(answer.stocks.len(), PER_GROUP);
    assert_eq!(answer.stocks[0].symbol, "XA01");
    // Nothing matches nothing.
    assert!(empty(&catalog.search("zzz", today(), &no_spot).await));
    assert!(empty(&catalog.search("   ", today(), &no_spot).await));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn derivatives_come_from_the_nearest_expiry_around_the_spot() {
    let scratch = Scratch::new("expand");
    scratch.write_all();
    let catalog = scratch.catalog().await;
    loaded(catalog.load_if_changed("test").await);

    // No spot: the middle of the nearest chain. RELIANCE 24 NOV strikes 1380..1440, middle 1420.
    let answer = catalog.search("reliance", today(), &no_spot).await;
    assert_eq!(symbols(&answer.futures), vec!["RELIANCE26NOVFUT", "RELIANCE26DECFUT"]);
    assert_eq!(
        symbols(&answer.options),
        vec!["RELIANCE26NOV1400CE", "RELIANCE26NOV1400PE", "RELIANCE26NOV1420CE", "RELIANCE26NOV1420PE", "RELIANCE26NOV1440CE"]
    );
    assert_eq!(answer.options[0].expiry.as_deref(), Some("2026-11-24"));
    assert_eq!((answer.options[0].strike, answer.options[0].lot_size), (Some(1400.0), 500));
    // With the spot known: the strikes nearest it.
    let spot = |symbol: &str| (symbol == "RELIANCE").then_some(1_381.0);
    let answer = catalog.search("reliance", today(), &spot).await;
    assert_eq!(
        symbols(&answer.options),
        vec!["RELIANCE26NOV1380CE", "RELIANCE26NOV1380PE", "RELIANCE26NOV1400CE", "RELIANCE26NOV1400PE", "RELIANCE26NOV1420CE", "RELIANCE26NOV1420PE"]
    );

    // Expired contracts are never listed: on the 3rd the weekly NIFTY expiry is nearest, on the 4th the monthly.
    let on_expiry_day = catalog.search("nifty", d(2026, 11, 3), &no_spot).await;
    assert!(on_expiry_day.options.iter().all(|r| r.expiry.as_deref() == Some("2026-11-03")), "{:?}", symbols(&on_expiry_day.options));
    assert_eq!(symbols(&on_expiry_day.options), vec!["NIFTY26N0322950CE", "NIFTY26N0322950PE", "NIFTY26N0323000CE", "NIFTY26N0323000PE", "NIFTY26N0323050CE", "NIFTY26N0323050PE"]);
    let after = catalog.search("nifty", today(), &no_spot).await;
    assert!(after.options.iter().all(|r| r.expiry.as_deref() == Some("2026-11-24")), "{:?}", symbols(&after.options));
    assert!(catalog.search("nifty", d(2027, 1, 1), &no_spot).await.options.is_empty());
    assert!(catalog.search("nifty", d(2027, 1, 1), &no_spot).await.futures.is_empty());
    assert_eq!(catalog.option_expiries("NIFTY", d(2026, 10, 1)).await, vec![d(2026, 10, 27), d(2026, 11, 3), d(2026, 11, 24), d(2026, 12, 29)]);
    assert_eq!(catalog.strikes_near("NIFTY", d(2026, 11, 24), 23_000.0, 2, 2).await, vec![22_900.0, 23_000.0, 23_100.0]);
    assert_eq!(catalog.median_strike("RELIANCE", d(2026, 11, 24)).await, Some(1_420.0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_contracts_resolve_to_listed_rows() {
    let scratch = Scratch::new("typed");
    scratch.write_all();
    let catalog = scratch.catalog().await;
    loaded(catalog.load_if_changed("test").await);

    let answer = catalog.search("RELIANCE NOV 1400 PE", today(), &no_spot).await;
    assert_eq!(answer.parsed, "contract");
    assert_eq!(symbols(&answer.options), vec!["RELIANCE26NOV1400PE"]);
    assert!(answer.futures.is_empty());
    assert_eq!(answer.stocks[0].symbol, "RELIANCE");
    // No month: every expiry with that strike, nearest first.
    assert_eq!(symbols(&catalog.search("reliance 1400 ce", today(), &no_spot).await.options), vec!["RELIANCE26NOV1400CE", "RELIANCE26DEC1400CE"]);
    assert_eq!(symbols(&catalog.search("nifty 23000ce", today(), &no_spot).await.options), vec!["NIFTY26NOV23000CE", "NIFTY26DEC23000CE"]);
    assert_eq!(symbols(&catalog.search("nifty 23000ce", d(2026, 11, 3), &no_spot).await.options), vec!["NIFTY26N0323000CE", "NIFTY26NOV23000CE", "NIFTY26DEC23000CE"]);
    assert_eq!(symbols(&catalog.search("NIFTY 03NOV 23000 CE", d(2026, 11, 3), &no_spot).await.options), vec!["NIFTY26N0323000CE"]);
    assert_eq!(symbols(&catalog.search("NIFTY DEC26 23000 CE", today(), &no_spot).await.options), vec!["NIFTY26DEC23000CE"]);
    // A month nothing is listed for falls back to every expiry.
    assert_eq!(symbols(&catalog.search("NIFTY JAN 23000 CE", today(), &no_spot).await.options), vec!["NIFTY26NOV23000CE", "NIFTY26DEC23000CE"]);
    // A strike nothing is listed at: the nearest strikes of the nearest expiry, that side only.
    assert_eq!(symbols(&catalog.search("RELIANCE 1410 CE", today(), &no_spot).await.options), vec!["RELIANCE26NOV1380CE", "RELIANCE26NOV1400CE", "RELIANCE26NOV1420CE", "RELIANCE26NOV1440CE"]);
    // ×1 strikes, half-rupee strikes, `&` and `-` in the symbol.
    assert_eq!(symbols(&catalog.search("banknifty 45300 pe", today(), &no_spot).await.options), vec!["BANKNIFTY26NOV45300PE"]);
    assert_eq!(symbols(&catalog.search("idea 7.5 ce", today(), &no_spot).await.options), vec!["IDEA26NOV7.5CE"]);
    assert_eq!(symbols(&catalog.search("M&M 2800 PUT", today(), &no_spot).await.options), vec!["M&M26NOV2800PE"]);
    assert_eq!(symbols(&catalog.search("bajaj-auto 9000 ce", today(), &no_spot).await.options), vec!["BAJAJ-AUTO26NOV9000CE"]);
    // A name resolves the underlying too.
    assert_eq!(symbols(&catalog.search("tata motors 700 ce", today(), &no_spot).await.options), vec!["TATAMOTORS26NOV700CE"]);
    // Futures.
    let answer = catalog.search("TATA FUT", today(), &no_spot).await;
    assert_eq!(answer.parsed, "future");
    assert_eq!(symbols(&answer.futures), vec!["TATAMOTORS26NOVFUT"]);
    assert!(answer.options.is_empty());
    assert_eq!(symbols(&catalog.search("reliance fut", today(), &no_spot).await.futures), vec!["RELIANCE26NOVFUT", "RELIANCE26DECFUT"]);
    assert_eq!(symbols(&catalog.search("sensex fut", today(), &no_spot).await.futures), vec!["SENSEX26NOVFUT"]);
    // A stock with no F&O has no derivatives to list, and a non-contract stays plain.
    let answer = catalog.search("tcs 100 ce", today(), &no_spot).await;
    assert_eq!(answer.parsed, "contract");
    assert!(answer.options.is_empty());
    assert_eq!(answer.stocks[0].symbol, "TCS");
    assert_eq!(catalog.search("tcs ce", today(), &no_spot).await.parsed, "plain");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_columns_are_tolerated_and_the_missing_ones_named() {
    let scratch = Scratch::new("columns");
    scratch.write("nse_cm", NSE_CM_OTHER_SHAPE);
    let catalog = scratch.catalog().await;
    let report = loaded(catalog.load_if_changed("test").await);
    let cm = &report.segments[0];
    assert_eq!((cm.segment, cm.kept, cm.skipped), ("nse_cm", 2, 0));
    assert_eq!(cm.missing, vec!["pInstType", "pISIN", "pDesc"]);
    assert!(cm.header.starts_with("pGroup,pSymbol"), "the BOM is dropped: {}", cm.header);
    // Without pDesc the name is the symbol; the padded tick-size column is read.
    let rows = sqlx::query_as::<_, (String, String, f64, Option<String>)>("SELECT symbol, name, tick_size, isin FROM instruments WHERE kind = 'EQ' ORDER BY symbol")
        .fetch_all(catalog.pool.as_ref().unwrap())
        .await
        .unwrap();
    assert_eq!(rows, vec![("LT".into(), "LT".into(), 0.05, None), ("RELIANCE".into(), "RELIANCE".into(), 0.05, None)]);
    assert_eq!(catalog.search("lt", today(), &no_spot).await.stocks[0].trading_symbol, "LT-EQ");

    // A header without the columns rows need leaves that file out, and the rest loads.
    scratch.write("nse_cm", "pExchSeg,pSymbolName\nnse_cm,RELIANCE\n");
    scratch.write("nse_fo", NSE_FO);
    let report = loaded(catalog.load_if_changed("test").await);
    assert_eq!(report.segments.iter().map(|r| (r.segment, r.kept)).collect::<Vec<_>>(), vec![("nse_fo", 38), ("nse_cm", 0)]);
    assert!(report.segments[1].missing.contains(&"pSymbol"));
    assert_eq!(catalog.status().rows.get("nse_cm"), Some(&0));
    assert!(catalog.search("reliance", today(), &no_spot).await.stocks.is_empty());
    assert_eq!(symbols(&catalog.search("reliance", today(), &no_spot).await.futures), vec!["RELIANCE26NOVFUT", "RELIANCE26DECFUT"]);
}

#[test]
fn the_catalog_agrees_with_the_engine_on_strikes_and_expiries() {
    // Every contract the engine's parser resolves, the catalog reads the same.
    for (csv, segment) in [(NSE_FO, "nse_fo"), (BSE_FO, "bse_fo")] {
        let store = ScripStore::parse_csv(csv, segment);
        let mut checked = 0;
        for record in store.records.values().flatten() {
            let line = csv.lines().find(|l| l.contains(&format!(",{},", record.trading_symbol))).unwrap();
            let fields: Vec<&str> = line.split(',').map(|f| f.trim().trim_matches('"')).collect();
            let (expiry_text, strike_text) = (fields[7], fields[8]);
            if expiry_text.is_empty() {
                continue;
            }
            assert_eq!(expiry_of(segment, expiry_text), Some(record.expiry_date), "{}", record.trading_symbol);
            let raw = strike_text.parse::<f64>().unwrap();
            if let Some(engine) = record.normalized_strike() {
                assert_eq!(strike_of(&record.trading_symbol, &record.option_type, raw), Some(engine), "{}", record.trading_symbol);
            }
            checked += 1;
        }
        assert!(checked >= 4, "{segment}: {checked} records compared");
    }
    // And the one strike the engine cannot read, the catalog reads.
    let store = ScripStore::parse_csv(NSE_FO, "nse_fo");
    let idea = &store.records["IDEA"][0];
    assert_eq!(idea.normalized_strike(), None);
    assert_eq!(strike_of(&idea.trading_symbol, &idea.option_type, idea.strike_price), Some(7.5));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_quote_hub_gets_the_cash_keys_and_upstox_keys_from_the_search_list() {
    let scratch = Scratch::new("quote-keys");
    let catalog = scratch.catalog().await;
    assert!(catalog.cash_keys().is_empty());
    assert_eq!(catalog.upstox_key("nse_cm|2885"), None);
    scratch.write_all();
    loaded(catalog.load_if_changed("test").await);

    // Every equity and index row's price-map key, no contract's.
    let keys: HashSet<String> = catalog.cash_keys().into_iter().collect();
    assert_eq!(keys.len(), 18 + 6);
    for key in ["nse_cm|2885", "nse_cm|99002", "idx|NIFTY 50", "idx|SENSEX", "idx|NIFTY MID SELECT"] {
        assert!(keys.contains(key), "{key}");
    }
    assert!(keys.iter().all(|k| k.starts_with("nse_cm|") || k.starts_with("idx|")), "{keys:?}");

    // An equity by its ISIN, an index by Upstox's name; a contract, an
    // equity with no ISIN and an F&O-only underlying have none.
    assert_eq!(catalog.upstox_key("nse_cm|2885").as_deref(), Some("NSE_EQ|INE002A01018"));
    assert_eq!(catalog.upstox_key("nse_cm|11536").as_deref(), Some("NSE_EQ|INE467B01029"));
    assert_eq!(catalog.upstox_key("idx|NIFTY 50").as_deref(), Some("NSE_INDEX|Nifty 50"));
    assert_eq!(catalog.upstox_key("idx|SENSEX").as_deref(), Some("BSE_INDEX|SENSEX"));
    assert_eq!(catalog.upstox_key("nse_fo|51386"), None);
    assert_eq!(catalog.upstox_key("nse_cm|99003"), None, "JUNK is not in the catalog");
    assert_eq!(catalog.upstox_key("nse_cm|9999999"), None);

    // Both survive a restart from the saved search list.
    let again = scratch.catalog().await;
    assert_eq!(again.cash_keys().len(), 24);
    assert_eq!(again.upstox_key("nse_cm|2885").as_deref(), Some("NSE_EQ|INE002A01018"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_quote_route_checks_its_keys_against_the_catalog() {
    let scratch = Scratch::new("known-keys");
    let catalog = scratch.catalog().await;
    // An empty catalog knows nothing.
    assert!(catalog.known_keys(&["nse_cm|2885".to_string()]).await.is_empty());
    scratch.write_all();
    loaded(catalog.load_if_changed("test").await);

    let keys: Vec<String> = [
        "nse_cm|2885",      // an equity, from the search list
        "idx|NIFTY 50",     // an index, likewise
        "nse_fo|51386",     // a contract, by its id
        "bse_fo|874001",    // a BSE index option
        "nse_cm|99002",     // the BE row: in the table, not the search list
        "nse_fo|999999",    // no such token
        "nse_cm|Nifty 50",  // the REST form of an index is not a price-map key
        "idx_close|NIFTY 50",
        "junk",
        "|",
        "nse_fo|",
    ]
    .iter()
    .map(|k| k.to_string())
    .collect();
    let known = catalog.known_keys(&keys).await;
    let mut known: Vec<&str> = known.iter().map(|k| k.as_str()).collect();
    known.sort();
    assert_eq!(known, vec!["bse_fo|874001", "idx|NIFTY 50", "nse_cm|2885", "nse_cm|99002", "nse_fo|51386"]);

    assert_eq!(catalog.spot_key("RELIANCE").as_deref(), Some("nse_cm|2885"));
    assert_eq!(catalog.spot_key("NIFTY").as_deref(), Some("idx|NIFTY 50"));
    assert_eq!(catalog.spot_key("NIFTYNXT50"), None, "an F&O-only underlying has no spot row");
    assert_eq!(catalog.spot_key("nope"), None);
}

/// The watchlists' lookup: a row for each key that names one, by price-map
/// key or id; and a failed lookup is an error, never "no such rows".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rows_for_keys_names_rows_and_reports_a_failed_lookup() {
    let scratch = Scratch::new("rows-for-keys");
    let catalog = scratch.catalog().await;
    // An empty catalog has no rows, and that is not an error.
    assert!(catalog.rows_for_keys(&["nse_fo|51386".to_string()]).await.unwrap().is_empty());
    scratch.write_all();
    loaded(catalog.load_if_changed("test").await);

    let keys: Vec<String> = ["nse_cm|2885", "nse_cm:2885", "nse_fo|51386", "nse_fo:51386", "idx|NIFTY 50", "nse_cm|Nifty 50", "nse_fo|999999", "junk"]
        .iter()
        .map(|k| k.to_string())
        .collect();
    let rows = catalog.rows_for_keys(&keys).await.unwrap();
    let mut named: Vec<(&str, &str)> = rows.iter().map(|(k, r)| (k.as_str(), r.ws_key.as_str())).collect();
    named.sort();
    assert_eq!(
        named,
        vec![
            ("idx|NIFTY 50", "idx|NIFTY 50"),
            ("nse_cm:2885", "nse_cm|2885"),
            ("nse_cm|2885", "nse_cm|2885"),
            ("nse_fo:51386", "nse_fo|51386"),
            ("nse_fo|51386", "nse_fo|51386"),
        ]
    );

    // The catalog database breaks: a lookup that reaches it is an error;
    // keys answered from the search list in memory alone still have rows.
    catalog.close_db_for_tests().await;
    assert!(catalog.rows_for_keys(&["nse_cm|2885".to_string(), "nse_fo|51386".to_string()]).await.is_err());
    assert!(catalog.rows_for_keys(&["nse_fo:51386".to_string()]).await.is_err(), "an id is looked up in the database");
    let cash = catalog.rows_for_keys(&["nse_cm|2885".to_string(), "idx|NIFTY 50".to_string()]).await.unwrap();
    assert_eq!(cash.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_instrument_page_row_carries_its_underlying_and_nearest_expiry() {
    let scratch = Scratch::new("instrument");
    scratch.write_all();
    let catalog = scratch.catalog().await;
    loaded(catalog.load_if_changed("test").await);

    // An F&O stock: lot and tick from its cash row, the nearest expiry from its contracts.
    let reliance = catalog.instrument("nse_cm:2885", today()).await.unwrap().unwrap();
    assert_eq!((reliance.row.kind, reliance.row.symbol.as_str(), reliance.row.name.as_str()), (Kind::Eq, "RELIANCE", "Reliance Industries"));
    assert_eq!((reliance.row.lot_size, reliance.row.tick_size, reliance.row.has_fo, reliance.row.exchange.as_str()), (1, 0.05, true, "NSE"));
    assert_eq!((reliance.segment.as_str(), reliance.token.as_str(), reliance.isin.as_deref()), ("nse_cm", "2885", Some("INE002A01018")));
    assert_eq!(reliance.underlying_key.as_deref(), Some("nse_cm|2885"));
    assert_eq!(reliance.nearest_expiry.as_deref(), Some("2026-11-24"));
    assert_eq!(reliance.row.tier, None);
    // A stock with no F&O has no expiry to show.
    let tcs = catalog.instrument("nse_cm:11536", today()).await.unwrap().unwrap();
    assert_eq!((tcs.row.has_fo, tcs.nearest_expiry), (false, None));
    // An index: on the 3rd its weekly expiry, on the 4th the monthly.
    let nifty = catalog.instrument("nse_cm:Nifty 50", d(2026, 11, 3)).await.unwrap().unwrap();
    assert_eq!((nifty.row.kind, nifty.row.ws_key.as_str(), nifty.nearest_expiry.as_deref()), (Kind::Index, "idx|NIFTY 50", Some("2026-11-03")));
    assert_eq!(catalog.instrument("nse_cm:Nifty 50", today()).await.unwrap().unwrap().nearest_expiry.as_deref(), Some("2026-11-24"));
    assert_eq!(catalog.instrument("nse_cm:Nifty 50", d(2027, 1, 1)).await.unwrap().unwrap().nearest_expiry, None);
    // A contract: its strike and expiry, its underlying's spot key, no nearest expiry of its own.
    let option = catalog.instrument("nse_fo:51386", today()).await.unwrap().unwrap();
    assert_eq!((option.row.kind, option.row.strike, option.row.expiry.as_deref()), (Kind::Ce, Some(23_000.0), Some("2026-11-03")));
    assert_eq!((option.row.lot_size, option.row.underlying.as_str(), option.row.has_fo), (65, "NIFTY", true));
    assert_eq!((option.underlying_key.as_deref(), option.nearest_expiry), (Some("idx|NIFTY 50"), None));
    let sensex_fut = catalog.instrument("bse_fo:874003", today()).await.unwrap().unwrap();
    assert_eq!((sensex_fut.row.kind, sensex_fut.row.exchange.as_str(), sensex_fut.underlying_key.as_deref()), (Kind::Fut, "BSE", Some("idx|SENSEX")));
    // An underlying with no row of its own.
    let nxt = catalog.instrument("nse_fo:49001", today()).await.unwrap().unwrap();
    assert_eq!((nxt.underlying_key, nxt.row.has_fo), (None, true));
    // Unknown ids.
    assert!(catalog.instrument("nse_cm:0", today()).await.unwrap().is_none());
    assert!(catalog.instrument("status", today()).await.unwrap().is_none());
    assert!(catalog.instrument("", today()).await.unwrap().is_none());
    // A catalog whose database is gone answers an error, not "no instrument".
    catalog.pool.as_ref().unwrap().close().await;
    assert!(catalog.instrument("nse_cm:2885", today()).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_real_files_shape_loads_with_paise_ticks_and_every_column_found() {
    let scratch = Scratch::new("real-shape");
    scratch.write_real_shape();
    let catalog = scratch.catalog().await;
    let report = loaded(catalog.load_if_changed("test").await);
    let by_segment = |segment: &str| report.segments.iter().find(|r| r.segment == segment).unwrap();

    // The real header names every column the catalog wants, in every file.
    for segment in ["nse_cm", "nse_fo", "bse_fo"] {
        let r = by_segment(segment);
        assert_eq!(r.header, REAL_HEADER, "{segment}");
        assert!(r.missing.is_empty(), "{segment}: {:?}", r.missing);
    }
    // nse_cm: the four equities; the index line (no series) and the SME series are out.
    let cm = by_segment("nse_cm");
    assert_eq!((cm.kept, cm.skipped), (4, 2));
    assert_eq!((cm.skip_reasons.get("no series"), cm.skip_reasons.get("series not EQ or BE")), (Some(&1), Some(&1)));
    // nse_fo: every row; bse_fo: the SENSEX contracts, not the stock's.
    let fo = by_segment("nse_fo");
    assert_eq!((fo.kept, fo.skipped, fo.unresolved_strikes), (5, 0, 0));
    let bse = by_segment("bse_fo");
    assert_eq!((bse.kept, bse.skipped), (2, 2));
    assert_eq!(bse.skip_reasons.get("bse_fo contract on a stock"), Some(&2));

    // Ticks are paise in the files and rupees in the catalog.
    let tick = |id: &str| {
        let catalog = Arc::clone(&catalog);
        let id = id.to_string();
        async move {
            sqlx::query_scalar::<_, f64>("SELECT tick_size FROM instruments WHERE id = ?")
                .bind(id)
                .fetch_one(catalog.pool.as_ref().unwrap())
                .await
                .unwrap()
        }
    };
    assert_eq!(tick("nse_cm:2885").await, 0.10, "RELIANCE-EQ: 10 paise");
    assert_eq!(tick("nse_cm:1333").await, 0.05, "HDFCBANK-EQ");
    assert_eq!(tick("nse_cm:14366").await, 0.01, "IDEA-EQ: 1 paisa");
    assert_eq!(tick("nse_cm:16669").await, 1.0, "BAJAJ-AUTO-EQ: 100 paise");
    assert_eq!(tick("nse_fo:52001").await, 0.05, "every option: 5 paise");
    assert_eq!(tick("bse_fo:874001").await, 0.05);
    assert_eq!(tick("nse_cm:Nifty 50").await, 0.05, "the virtual index row agrees with its options");
    let index = catalog.index();
    assert_eq!(index.get("RELIANCE").unwrap().tick_size, 0.10, "the search list carries the cash row's tick");
    assert_eq!((index.get("RELIANCE").unwrap().name.as_str(), index.get("RELIANCE").unwrap().isin.as_deref()), ("RELIANCE INDUSTRIES LTD", Some("INE002A01018")));
    assert!(index.get("SMECO").is_none());

    // Strikes: a plain integer, scientific notation, a half rupee; expiries
    // from the segment's epoch rule land on the day the symbol encodes; a
    // blank pDesc gets a derived name.
    let row = |id: &str| {
        let catalog = Arc::clone(&catalog);
        let id = id.to_string();
        async move {
            sqlx::query_as::<_, (String, Option<String>, Option<f64>, String, i64, String)>(
                "SELECT kind, expiry, strike, name, lot_size, exchange FROM instruments WHERE id = ?",
            )
            .bind(id)
            .fetch_one(catalog.pool.as_ref().unwrap())
            .await
            .unwrap()
        }
    };
    assert_eq!(row("nse_fo:52001").await, ("CE".into(), Some("2026-11-09".into()), Some(17_000.0), "NIFTY 09 NOV 26 17000 CE".into(), 65, "NSE".into()));
    assert_eq!(row("nse_fo:49001").await, ("CE".into(), Some("2026-11-24".into()), Some(69_600.0), "NIFTYNXT50 24 NOV 26 69600 CE".into(), 25, "NSE".into()));
    assert_eq!(row("nse_fo:41222").await, ("CE".into(), Some("2026-11-24".into()), Some(1_280.0), "RELIANCE 24 NOV 26 1280 CE".into(), 500, "NSE".into()));
    assert_eq!(row("nse_fo:45001").await, ("CE".into(), Some("2026-11-24".into()), Some(7.5), "IDEA 24 NOV 26 7.5 CE".into(), 80_000, "NSE".into()));
    assert_eq!(row("nse_fo:43001").await, ("FUT".into(), Some("2026-11-24".into()), None, "RELIANCE 24 NOV 26 FUT".into(), 500, "NSE".into()));
    assert_eq!(row("bse_fo:874001").await, ("CE".into(), Some("2026-11-05".into()), Some(64_600.0), "SENSEX 05 NOV 26 64600 CE".into(), 20, "BSE".into()));
    assert_eq!(row("bse_fo:874003").await, ("FUT".into(), Some("2026-11-26".into()), None, "SENSEX 26 NOV 26 FUT".into(), 20, "BSE".into()), "an `IF` row is a future by its symbol");
    assert_eq!(row("nse_cm:2885").await, ("EQ".into(), None, None, "RELIANCE INDUSTRIES LTD".into(), 1, "NSE".into()));
    assert!(ids(&catalog, "SELECT id FROM instruments WHERE id IN ('bse_fo:876001', 'bse_fo:876002', 'nse_cm:26000', 'nse_cm:99999')").await.is_empty());

    // Search over it: symbols, a name's words, an ISIN, a typed contract, futures.
    let answer = catalog.search("reliance", today(), &no_spot).await;
    assert_eq!((answer.stocks[0].symbol.as_str(), answer.stocks[0].tick_size, answer.stocks[0].has_fo), ("RELIANCE", 0.10, true));
    assert_eq!(symbols(&answer.futures), vec!["RELIANCE26NOVFUT"]);
    assert_eq!(symbols(&answer.options), vec!["RELIANCE26NOV1280CE"]);
    assert_eq!(catalog.search("hdfc bank", today(), &no_spot).await.stocks[0].symbol, "HDFCBANK");
    assert_eq!(catalog.search("INE669E01016", today(), &no_spot).await.stocks[0].symbol, "IDEA");
    assert_eq!(symbols(&catalog.search("nifty 17000 ce", today(), &no_spot).await.options), vec!["NIFTY26N0917000CE"]);
    assert_eq!(symbols(&catalog.search("niftynxt 69600 ce", today(), &no_spot).await.options), vec!["NIFTYNXT5026NOV69600CE"]);
    assert_eq!(symbols(&catalog.search("idea 7.5 ce", today(), &no_spot).await.options), vec!["IDEA26NOV7.5CE"]);
    let sensex = catalog.search("sensex", today(), &no_spot).await;
    assert_eq!((sensex.indices[0].symbol.as_str(), symbols(&sensex.futures), symbols(&sensex.options)), ("SENSEX", vec!["SENSEX26NOVFUT"], vec!["SENSEX26N0564600CE"]));
    assert!(catalog.search("nifty", today(), &no_spot).await.stocks.is_empty(), "the cash file's index line is not a stock");
    let page = catalog.instrument("nse_cm:2885", today()).await.unwrap().unwrap();
    assert_eq!((page.row.tick_size, page.nearest_expiry.as_deref(), page.isin.as_deref()), (0.10, Some("2026-11-24"), Some("INE002A01018")));
}

/// The chain index is `(underlying, kind, expiry, strike)` so every lookup
/// the search makes is a seek: no query walks an underlying's contracts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_chain_lookup_seeks_the_chain_index_by_underlying_and_kind() {
    let scratch = Scratch::new("plans");
    scratch.write_all();
    let catalog = scratch.catalog().await;
    loaded(catalog.load_if_changed("test").await);
    let pool = catalog.pool.as_ref().unwrap();

    let futures = format!("{SELECT_ROW} WHERE underlying = ? AND kind = 'FUT' AND expiry >= ? ORDER BY expiry LIMIT ?");
    let strikes_under = "SELECT DISTINCT strike FROM instruments WHERE underlying = ? AND expiry = ? AND strike <= ? AND kind IN ('CE', 'PE') ORDER BY strike DESC LIMIT ?";
    let options_at = format!("{SELECT_ROW} WHERE underlying = ? AND expiry = ? AND strike BETWEEN ? AND ? AND kind IN ('CE', 'PE') ORDER BY kind");
    let queries: Vec<(&str, &str, usize)> = vec![
        ("futures_of", futures.as_str(), 3),
        ("nearest_expiry", NEAREST_EXPIRY, 2),
        ("option_expiries", OPTION_EXPIRIES, 2),
        ("strikes_near", strikes_under, 4),
        ("options_at", options_at.as_str(), 4),
    ];
    for (name, sql, binds) in queries {
        let explain = format!("EXPLAIN QUERY PLAN {sql}");
        let mut query = sqlx::query_as::<_, (i64, i64, i64, String)>(&explain);
        for _ in 0..binds {
            query = query.bind("NIFTY");
        }
        let plan: Vec<String> = query.fetch_all(pool).await.unwrap().into_iter().map(|(_, _, _, detail)| detail).collect();
        let on_table: Vec<&String> = plan.iter().filter(|d| d.contains(" instruments ")).collect();
        assert!(!on_table.is_empty(), "{name}: {plan:?}");
        for step in on_table {
            assert!(
                step.starts_with("SEARCH instruments USING") && step.contains("idx_instruments_chain (underlying=? AND kind=?"),
                "{name}: {step}\n{plan:#?}"
            );
        }
    }
    // The queries answer what they did before the index changed.
    assert_eq!(catalog.option_expiries("NIFTY", d(2026, 10, 1)).await, vec![d(2026, 10, 27), d(2026, 11, 3), d(2026, 11, 24), d(2026, 12, 29)]);
    assert_eq!(catalog.option_expiries("RELIANCE", today()).await, vec![d(2026, 11, 24), d(2026, 12, 29)]);
    assert!(catalog.option_expiries("TCS", today()).await.is_empty());
    assert_eq!(catalog.nearest_expiry("NIFTY", d(2026, 11, 4)).await, Some(d(2026, 11, 24)));
    assert_eq!(catalog.nearest_expiry("NIFTY", d(2026, 11, 3)).await, Some(d(2026, 11, 3)));
    assert_eq!(catalog.nearest_expiry("TCS", today()).await, None);
}

/// Off, the `search` switch stops the background rebuild; the direct build
/// (and the one the switch's turning on starts) still works.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_search_switch_off_skips_the_background_rebuild() {
    let scratch = Scratch::new("gate");
    scratch.write_all();
    let flags = Arc::new(tokio::sync::RwLock::new(FeatureFlags::default()));
    let catalog = Catalog::open(scratch.files(), &scratch.db(), Arc::clone(&flags)).await;
    catalog.rebuild_in_background("test, switch off");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let _not_building = catalog.loading.lock().await;
    assert_eq!(catalog.status().total_rows(), 0, "nothing was built");
    drop(_not_building);

    flags.write().await.search = true;
    catalog.rebuild_in_background("test, switch on");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while catalog.status().total_rows() == 0 {
        assert!(std::time::Instant::now() < deadline, "the build did not run with the switch on");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(catalog.status().total_rows(), 65);
}
