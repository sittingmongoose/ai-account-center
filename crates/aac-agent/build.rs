//! Record Cargo's actual target triple for the hash-verified agent handshake.
fn main() {
    if let Ok(target) = std::env::var("TARGET") {
        println!("cargo:rustc-env=AAC_AGENT_TARGET={target}");
    }
}
