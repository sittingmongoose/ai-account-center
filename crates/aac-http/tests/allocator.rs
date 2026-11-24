//! The shipped service binaries must link mimalloc as their global allocator:
//! under glibc malloc, freed compose transients stayed resident as multi-GB of
//! arena RSS (service-memory lane, measured on the 54c363fa7 build). The
//! allocator names itself in the binary's read-only data even when stripped.

#[test]
fn service_binaries_link_mimalloc() -> Result<(), Box<dyn std::error::Error>> {
    for (name, path) in [
        ("ai-account-center", env!("CARGO_BIN_EXE_ai-account-center")),
        ("ccs", env!("CARGO_BIN_EXE_ccs")),
        ("aac-http", env!("CARGO_BIN_EXE_aac-http")),
    ] {
        let bytes = std::fs::read(path)?;
        assert!(
            bytes
                .windows(b"mimalloc".len())
                .any(|window| window == b"mimalloc"),
            "{name} does not link mimalloc"
        );
    }
    Ok(())
}
