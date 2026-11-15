//! Stamps the build commit into the service (L1 build check). `AAC_BUILD_COMMIT` wins; otherwise
//! `git rev-parse HEAD` of this checkout; otherwise `unknown`. Never fails the build.
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let out = Command::new("git").args(args).current_dir(dir).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_owned())
}

fn commit_word(text: &str) -> bool {
    (7..=40).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn main() {
    #[cfg(feature = "node")]
    napi_build::setup();
    println!("cargo:rerun-if-env-changed=AAC_BUILD_COMMIT");
    // Rebuild when the checkout's HEAD or its branch ref moves.
    for path in [
        git(&["rev-parse", "--git-path", "HEAD"]),
        git(&["symbolic-ref", "-q", "HEAD"]).and_then(|r| git(&["rev-parse", "--git-path", &r])),
    ]
    .into_iter()
    .flatten()
    {
        if let Ok(dir) = std::env::var("CARGO_MANIFEST_DIR") {
            println!(
                "cargo:rerun-if-changed={}",
                std::path::Path::new(&dir).join(path).display()
            );
        }
    }
    let commit = std::env::var("AAC_BUILD_COMMIT")
        .ok()
        .filter(|c| commit_word(c))
        .or_else(|| git(&["rev-parse", "HEAD"]).filter(|c| commit_word(c)))
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=AAC_BUILD_COMMIT={commit}");
}
