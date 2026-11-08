//! Prints the compression ratio and time of a sealed segment at several zstd levels. Run with
//! AAC_SEGMENT=<raw .seglog path> cargo test --release --no-default-features --test zstd_levels -- --ignored --nocapture
#[test]
#[ignore]
fn zstd_levels() {
    let path = std::env::var("AAC_SEGMENT").expect("AAC_SEGMENT");
    let raw = std::fs::read(path).unwrap();
    for level in [3, 6, 9, 12, 15, 19] {
        let started = std::time::Instant::now();
        let out = zstd::bulk::compress(&raw, level).unwrap();
        println!(
            "level {level:>2}: {:>9} -> {:>8} bytes ({:.1}x) in {} ms",
            raw.len(),
            out.len(),
            raw.len() as f64 / out.len() as f64,
            started.elapsed().as_millis()
        );
    }
}
