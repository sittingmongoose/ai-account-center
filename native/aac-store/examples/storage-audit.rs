//! Offline sizing probe. Pass a COPY of aac.redb, never the dashboard's live database.
use aac_store::db;
use redb::{ReadableDatabase, ReadableTable};
use serde_json::json;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: storage-audit <copied-aac.redb>");
    let database = redb::Builder::new().open_read_only(path)?;
    let read = database.begin_read()?;
    let mut tables = Vec::new();
    for (name, def) in [
        ("reading_day", db::READING_DAY),
        ("account_state", db::ACCOUNT_STATE),
        ("period", db::PERIOD),
        ("rollup_hours", db::ROLLUP_HOURS),
        ("rollup_day", db::ROLLUP_DAY),
    ] {
        let table = read.open_table(def)?;
        let rows: Vec<Vec<u8>> = table
            .iter()?
            .map(|row| row.map(|(_, v)| v.value().to_vec()))
            .collect::<Result<_, _>>()?;
        let original: usize = rows.iter().map(Vec::len).sum();
        let mut levels = Vec::new();
        for level in [1, 3, 6, 9] {
            let start = Instant::now();
            let mut bytes = 0;
            let mut eligible = 0;
            for row in &rows {
                let packed = zstd::bulk::compress(row, level)?;
                // Only use envelopes when they save at least 32 bytes, including a 9-byte header.
                if row.len() >= 256 && packed.len() + 9 + 32 < row.len() {
                    bytes += packed.len() + 9;
                    eligible += 1;
                } else {
                    bytes += row.len();
                }
            }
            levels.push(json!({"level":level,"bytes":bytes,"eligible":eligible,"encodeMs":start.elapsed().as_secs_f64()*1000.0}));
        }
        tables.push(json!({"table":name,"rows":rows.len(),"valueBytes":original,"levels":levels}));
    }
    println!("{}", serde_json::to_string_pretty(&tables)?);
    Ok(())
}
