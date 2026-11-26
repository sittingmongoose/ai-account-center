//! Standalone offline account usage reader.
use aac_snapshot::reader::{self, Options};
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|s| s == "--help" || s == "-h") {
        println!(
            "aac-usage [history|signins|rules|install-rules] [--dir FOLDER] [--provider NAME] [--account TEXT] [--all] [--json]\nhistory: [--hours N] [--window TEXT]"
        );
        return;
    }
    let result = Options::parse(&args).and_then(|o| {
        aac_core::UtcMs::now()
            .map_err(|_| "aac-usage: clock unavailable".to_string())
            .and_then(|now| reader::run(&o, now.get() as i64))
    });
    match result {
        Ok(out) => print!("{}", reader::ascii(&out)),
        Err(e) => {
            eprintln!("{}", reader::ascii(&e));
            std::process::exit(1);
        }
    }
}
