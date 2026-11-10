//! The Kotak scrip-master CSVs on disk: `data/scrip/<segment>.csv` under the
//! working directory, beside `trades.db` (`backend/data/` on the VM, which is
//! gitignored and which `update.sh`'s `git reset --hard` leaves alone).
//!
//! Every download is written here and parsed into the in-memory
//! [`ScripStore`], and the text is then dropped. The merged raw CSV used to
//! be kept in `AppState` as well, a second copy of tens of MB on a 954 MB
//! VM that shares them with the engine's 50 ms tick. The admin's CSV
//! downloads (`/api/scrip/download`, `/api/auth/kotak/scrip-master/raw`)
//! stream these files back in the shape that merged text had: one header,
//! then every segment's rows, and the instrument catalog
//! (`crate::instruments`) is built from them.
//!
//! The engine never reads these files back: its `ScripStore` is filled by a
//! login's download and by nothing else, so after a restart every signal
//! is refused with "Scrip Master not loaded" until the login, exactly as
//! before the files existed. Only the catalog restores from disk, and it
//! serves search alone, labelled with its day.
//!
//! `ScripStore::parse_csv` always receives the text exactly as Kotak sent
//! it, parsed off the download.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::body::{Body, Bytes};
use futures::stream::StreamExt as _;
use tokio::io::AsyncBufReadExt as _;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use trading_engine::ScripStore;

/// The segments the engine resolves contracts from, in the order the merged
/// download lists them. `bse_cm` is never among them: its rows would pass
/// `resolve_signal`'s filter with the default expiry.
pub const SEGMENTS: [&str; 3] = ["nse_fo", "bse_fo", "nse_cm"];

/// `DATA_DIR` unset.
const DEFAULT_DATA_DIR: &str = "data";

/// Chunk the merged download is streamed in.
const CHUNK: usize = 64 * 1024;

/// Where the server keeps files between runs: `DATA_DIR` (default `data`),
/// relative to the working directory like `trades.db`.
pub fn data_dir() -> PathBuf {
    data_dir_from(std::env::var("DATA_DIR").ok().as_deref())
}

/// [`data_dir`] for a `DATA_DIR` value; blank counts as unset.
fn data_dir_from(raw: Option<&str>) -> PathBuf {
    raw.map(str::trim)
        .filter(|v| !v.is_empty())
        .map_or_else(|| PathBuf::from(DEFAULT_DATA_DIR), PathBuf::from)
}

/// Makes every temp file name unique, so two downloads writing the same
/// segment at once (a manual Connect during the 09:10 recycle) each rename
/// a complete file over the old one.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// The directory the segment files live in. Cheap to clone and share.
#[derive(Clone)]
pub struct ScripFiles {
    dir: Arc<PathBuf>,
}

/// What one pass over [`SEGMENTS`] produced — see [`download`].
pub struct Downloaded {
    /// Every segment that was fetched, parsed and merged; `None` when none was.
    pub store: Option<ScripStore>,
    /// `(segment, why)` for each segment that was not.
    pub failed: Vec<(&'static str, String)>,
}

impl ScripFiles {
    /// `data/scrip` under [`data_dir`].
    pub fn at_default_location() -> Self {
        Self::new(data_dir().join("scrip"))
    }

    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: Arc::new(dir.into()) }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `<segment>.csv` in the directory.
    pub fn path(&self, segment: &str) -> PathBuf {
        self.dir.join(format!("{segment}.csv"))
    }

    /// Creates the directory and removes temp files a crash left behind.
    /// Once, at startup.
    pub fn prepare(&self) -> io::Result<()> {
        std::fs::create_dir_all(&*self.dir)?;
        for entry in std::fs::read_dir(&*self.dir)? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "tmp") {
                let _ = std::fs::remove_file(&path);
            }
        }
        Ok(())
    }

    /// Writes `csv` as `<segment>.csv`: into a temp file beside it, synced,
    /// then renamed over the old one, so a reader never sees a partial file
    /// and a crash mid-write leaves the previous file in place. Blocking.
    pub fn write(&self, segment: &str, csv: &str) -> io::Result<()> {
        std::fs::create_dir_all(&*self.dir)?;
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let temp = self.dir.join(format!(".{segment}.csv.{}-{seq}.tmp", std::process::id()));
        let written = (|| {
            let mut file = std::fs::File::create(&temp)?;
            file.write_all(csv.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&temp, self.path(segment))
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        written
    }

    /// `<segment>.csv` as it was written; `None` when there is none. Blocking.
    pub fn read(&self, segment: &str) -> io::Result<Option<String>> {
        match std::fs::read_to_string(self.path(segment)) {
            Ok(csv) => Ok(Some(csv)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The segments on disk, in [`SEGMENTS`] order.
    pub fn present(&self) -> Vec<&'static str> {
        SEGMENTS.into_iter().filter(|segment| self.path(segment).is_file()).collect()
    }

    /// The files as one CSV body: the first file's header, then every file's
    /// non-empty rows, in [`SEGMENTS`] order — the shape the merged text had
    /// when it was kept in memory. Streamed in [`CHUNK`]s, never read whole.
    /// `None` when no file is on disk.
    pub fn merged_body(&self) -> Option<Body> {
        let paths: Vec<PathBuf> = self.present().into_iter().map(|segment| self.path(segment)).collect();
        if paths.is_empty() {
            return None;
        }
        let (tx, rx) = mpsc::channel::<Bytes>(2);
        tokio::spawn(stream_merged(paths, tx));
        Some(Body::from_stream(ReceiverStream::new(rx).map(Ok::<Bytes, std::convert::Infallible>)))
    }
}

/// Whether a row is blank, as `str::trim` judged it in the merged text.
fn blank(row: &[u8]) -> bool {
    match std::str::from_utf8(row) {
        Ok(s) => s.trim().is_empty(),
        Err(_) => row.trim_ascii().is_empty(),
    }
}

/// Sends `paths` down `tx` in the merged shape; stops when the reader hangs
/// up. Rows are split as `str::lines` split the merged text: at `\n`, with
/// a `\r` before it dropped, and each is sent with a `\n`.
async fn stream_merged(paths: Vec<PathBuf>, tx: mpsc::Sender<Bytes>) {
    let mut chunk: Vec<u8> = Vec::with_capacity(CHUNK);
    let mut header_sent = false;
    for path in paths {
        let file = match tokio::fs::File::open(&path).await {
            Ok(file) => file,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "Scrip Master CSV unreadable — left out of the download");
                continue;
            }
        };
        let mut reader = tokio::io::BufReader::with_capacity(CHUNK, file);
        let mut line: Vec<u8> = Vec::new();
        let mut first_line = true;
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line).await {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "Scrip Master CSV read failed — the download is cut short");
                    break;
                }
            }
            let mut row: &[u8] = &line;
            if let Some(stripped) = row.strip_suffix(b"\n") {
                row = stripped.strip_suffix(b"\r").unwrap_or(stripped);
            }
            let is_header = std::mem::replace(&mut first_line, false);
            if is_header {
                if header_sent {
                    continue;
                }
                header_sent = true;
            } else if blank(row) {
                continue;
            }
            chunk.extend_from_slice(row);
            chunk.push(b'\n');
            if chunk.len() >= CHUNK {
                let full = std::mem::replace(&mut chunk, Vec::with_capacity(CHUNK));
                if tx.send(Bytes::from(full)).await.is_err() {
                    return;
                }
            }
        }
    }
    if !chunk.is_empty() {
        let _ = tx.send(Bytes::from(chunk)).await;
    }
}

/// Fetches every segment in [`SEGMENTS`] off `client`'s session, saves each
/// to `files` and parses it. Every caller that loads the Scrip Master (the
/// login's background fetch, the startup restore) goes through here, so the
/// text written to disk is the text the store was parsed from. Parsing and
/// writing tens of MB are blocking work, done off the runtime's workers so
/// the engine's tick never waits on them.
pub async fn download(client: &kotak_client::KotakClient, files: &ScripFiles) -> Downloaded {
    let mut store: Option<ScripStore> = None;
    let mut failed = Vec::new();
    for segment in SEGMENTS {
        let csv = match client.get_scrip_master_csv(segment).await {
            Ok(csv) => csv,
            Err(e) => {
                tracing::error!(segment = %segment, "Failed to fetch Scrip Master: {}", e);
                failed.push((segment, e.to_string()));
                continue;
            }
        };
        let files = files.clone();
        let parsed = tokio::task::spawn_blocking(move || {
            if let Err(e) = files.write(segment, &csv) {
                tracing::error!(
                    segment = %segment, path = %files.path(segment).display(), error = %e,
                    "Scrip Master CSV not saved — the engine's store is loaded, but the admin's CSV download serves the previous file until the next load"
                );
            }
            ScripStore::parse_csv(&csv, segment)
        })
        .await;
        match parsed {
            Ok(parsed) => store.get_or_insert_with(ScripStore::default).merge(parsed),
            Err(e) => {
                tracing::error!(segment = %segment, error = %e, "Scrip Master parse failed");
                failed.push((segment, format!("parse failed: {e}")));
            }
        }
    }
    Downloaded { store, failed }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Kotak's column names, with the stray `dStrikePrice;` the real header has.
    const HEADER: &str = "pSymbol,pExchSeg,pInstType,pSymbolName,pTrdSymbol,pOptionType,lLotSize,lExpiryDate,dStrikePrice;,pTickSize,pISIN,pDesc";

    /// nse_fo: Kotak's epoch (`lExpiryDate` + 315511200 is 2026-11-03), a
    /// blank line and `\r\n` endings as a downloaded file may have them.
    const NSE_FO: &str = "pSymbol,pExchSeg,pInstType,pSymbolName,pTrdSymbol,pOptionType,lLotSize,lExpiryDate,dStrikePrice;,pTickSize,pISIN,pDesc\r\n\
        51386,nse_fo,OPTIDX,NIFTY,NIFTY26N0323000CE,CE,65,1478152800,2300000.00,0.05,,NIFTY 03NOV26 23000 CE\r\n\
        51387,nse_fo,OPTIDX,NIFTY,NIFTY26N0323000PE,PE,65,1478152800,2300000.00,0.05,,NIFTY 03NOV26 23000 PE\r\n\
        \r\n\
        35013,nse_fo,FUTIDX,NIFTY,NIFTY26NOVFUT,XX,65,1479967200,0.00,0.05,,NIFTY 24NOV26 FUT\r\n\
        \"41222\",nse_fo,OPTSTK,RELIANCE,RELIANCE26NOV1400CE,CE,500,1479967200,140000.00,0.05,,RELIANCE 24NOV26 1400 CE (₹)\r\n";

    /// bse_fo: a plain Unix epoch, a whitespace-only line, no final newline.
    const BSE_FO: &str = "pSymbol,pExchSeg,pInstType,pSymbolName,pTrdSymbol,pOptionType,lLotSize,lExpiryDate,dStrikePrice;,pTickSize,pISIN,pDesc\n\
        874001,bse_fo,OPTIDX,SENSEX,SENSEX26N0575100CE,CE,20,1793664000,7510000.00,0.05,,SENSEX 05NOV26 75100 CE\n\
        \x20\x20\n\
        874002,bse_fo,OPTIDX,SENSEX,SENSEX26N0575100PE,PE,20,1793664000,7510000.00,0.05,,SENSEX 05NOV26 75100 PE";

    /// nse_cm: the cash header has no strike, option or expiry column.
    const NSE_CM: &str = "pSymbol,pExchSeg,pInstType,pSymbolName,pTrdSymbol,lLotSize,pTickSize,pISIN,pDesc\n\
        11536,nse_cm,EQ,TCS,TCS-EQ,1,0.05,INE467B01029,Tata Consultancy Services\n\
        2885,nse_cm,EQ,RELIANCE,RELIANCE-EQ,1,0.05,INE002A01018,Reliance Industries\n";

    /// A directory of its own under the system temp dir, empty.
    fn scratch(name: &str) -> ScripFiles {
        let dir = std::env::temp_dir().join(format!("auto-trader-scrip-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ScripFiles::new(dir)
    }

    fn records(store: &ScripStore) -> serde_json::Value {
        serde_json::to_value(store).unwrap()
    }

    /// `merge_csv_sections`, as it merged the downloads in memory before
    /// they were kept on disk: the oracle for the streamed download.
    fn merged_as_before(csvs: &[&str]) -> String {
        let mut combined = String::new();
        for (index, csv) in csvs.iter().enumerate() {
            let mut lines = csv.lines();
            let Some(header) = lines.next() else { continue };
            if index == 0 {
                combined.push_str(header);
                combined.push('\n');
            }
            for line in lines {
                if !line.trim().is_empty() {
                    combined.push_str(line);
                    combined.push('\n');
                }
            }
        }
        combined
    }

    /// nse_fo rows enough to span several chunks of the stream.
    fn big_nse_fo() -> String {
        let mut csv = format!("{HEADER}\r\n");
        for strike in 0..4000u32 {
            let strike_rs = 20000 + strike * 50;
            csv.push_str(&format!(
                "{},nse_fo,OPTIDX,NIFTY,NIFTY26N03{strike_rs}CE,CE,65,1478152800,{strike_rs}00.00,0.05,,NIFTY 03NOV26 {strike_rs} CE\r\n",
                60000 + strike
            ));
        }
        csv
    }

    #[test]
    fn parse_csv_sees_the_download_byte_for_byte_after_the_round_trip() {
        let files = scratch("round-trip");
        files.write("nse_fo", NSE_FO).unwrap();

        let read_back = files.read("nse_fo").unwrap().expect("written");
        assert_eq!(read_back, NSE_FO, "the bytes on disk are the bytes Kotak sent");
        let before = ScripStore::parse_csv(NSE_FO, "nse_fo");
        let after = ScripStore::parse_csv(&read_back, "nse_fo");
        assert_eq!(records(&before), records(&after));
        // The fixture is real enough to produce records the engine resolves.
        let nifty = &after.records["NIFTY"];
        assert_eq!(nifty.len(), 3);
        assert_eq!(nifty[0].expiry_date, chrono::NaiveDate::from_ymd_opt(2026, 11, 3).unwrap());
        assert_eq!(nifty[0].normalized_strike(), Some(23000.0));
        assert_eq!(after.records["RELIANCE"][0].instrument_token, "41222");

        // No temp file is left behind, and a second write replaces the first.
        let leftovers: Vec<_> = std::fs::read_dir(files.dir()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("nse_fo.csv")]);
        files.write("nse_fo", BSE_FO).unwrap();
        assert_eq!(files.read("nse_fo").unwrap().as_deref(), Some(BSE_FO));
        assert_eq!(files.read("bse_fo").unwrap(), None);
        assert_eq!(files.present(), vec!["nse_fo"]);

        std::fs::remove_dir_all(files.dir()).unwrap();
    }

    /// The files are the catalog's and the admin downloads' only: nothing
    /// here parses them back into a `ScripStore`, so a restart leaves the
    /// engine's store empty until a login's download, as on main.
    #[test]
    fn nothing_here_restores_the_engines_store_from_disk() {
        let files = scratch("no-restore");
        files.write("nse_cm", NSE_CM).unwrap();
        files.write("bse_fo", BSE_FO).unwrap();
        files.write("nse_fo", NSE_FO).unwrap();
        assert_eq!(files.present(), vec!["nse_fo", "bse_fo", "nse_cm"]);
        // What is on disk reads back byte for byte, segment by segment — the
        // catalog's loader and the merged download work from that; a
        // segment never downloaded is simply absent.
        assert_eq!(files.read("bse_fo").unwrap().as_deref(), Some(BSE_FO));
        std::fs::remove_file(files.path("nse_fo")).unwrap();
        assert_eq!(files.read("nse_fo").unwrap(), None);
        assert_eq!(files.present(), vec!["bse_fo", "nse_cm"]);
        std::fs::remove_dir_all(files.dir()).unwrap();
    }

    #[tokio::test]
    async fn the_download_streams_in_the_shape_the_merged_text_had() {
        let files = scratch("merged");
        assert!(files.merged_body().is_none(), "nothing on disk, nothing to download");
        let nse_fo = big_nse_fo();
        files.write("nse_fo", &nse_fo).unwrap();
        files.write("bse_fo", BSE_FO).unwrap();
        files.write("nse_cm", NSE_CM).unwrap();

        let body = files.merged_body().expect("three files");
        let streamed = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let want = merged_as_before(&[&nse_fo, BSE_FO, NSE_CM]);
        assert!(want.len() > 3 * CHUNK, "the fixture spans several chunks");
        assert_eq!(std::str::from_utf8(&streamed).unwrap(), want);
        // One header, every row, no blank lines, `\n` endings throughout.
        let lines: Vec<&str> = want.lines().collect();
        assert_eq!(lines.iter().filter(|l| l.starts_with("pSymbol,")).count(), 1);
        assert_eq!(lines.len(), 1 + 4000 + 2 + 2);
        assert!(!want.contains('\r') && !want.contains("\n\n"));

        // Without bse_fo the next file's rows follow nse_fo's directly.
        std::fs::remove_file(files.path("bse_fo")).unwrap();
        let streamed = axum::body::to_bytes(files.merged_body().unwrap(), usize::MAX).await.unwrap();
        assert_eq!(std::str::from_utf8(&streamed).unwrap(), merged_as_before(&[&nse_fo, NSE_CM]));

        std::fs::remove_dir_all(files.dir()).unwrap();
    }

    #[test]
    fn prepare_makes_the_directory_and_sweeps_temp_files() {
        let files = scratch("prepare");
        files.prepare().unwrap();
        assert!(files.dir().is_dir());
        std::fs::write(files.dir().join(".nse_fo.csv.1-0.tmp"), "half").unwrap();
        files.write("nse_fo", NSE_FO).unwrap();
        files.prepare().unwrap();
        let left: Vec<_> = std::fs::read_dir(files.dir()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, vec![std::ffi::OsString::from("nse_fo.csv")]);
        std::fs::remove_dir_all(files.dir()).unwrap();
    }

    #[test]
    fn the_data_dir_is_relative_to_the_working_directory_unless_set() {
        // `data_dir_from`, not `data_dir`: tests share the process environment.
        assert_eq!(data_dir_from(None), PathBuf::from("data"));
        assert_eq!(data_dir_from(Some("  ")), PathBuf::from("data"));
        assert_eq!(data_dir_from(Some(" /var/lib/auto-trader ")), PathBuf::from("/var/lib/auto-trader"));
        assert_eq!(ScripFiles::new(data_dir_from(None).join("scrip")).path("nse_fo"), PathBuf::from("data/scrip/nse_fo.csv"));
    }
}
