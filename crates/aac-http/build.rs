//! Stamps the build commit into the service (L1 build check). `AAC_BUILD_COMMIT` wins; otherwise
//! `git rev-parse HEAD` of this checkout; otherwise `unknown`. Never fails the build.
use std::process::Command;
use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn git(args: &[&str]) -> Option<String> {
    let dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_owned())
}

fn commit_word(text: &str) -> bool {
    (7..=40).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn stamp_commit() {
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

fn ui_files(root: &Path, directory: &Path, files: &mut Vec<(String, PathBuf)>) {
    for entry in fs::read_dir(directory).expect("read packaged UI directory") {
        let entry = entry.expect("read packaged UI entry");
        let kind = entry.file_type().expect("inspect packaged UI entry");
        let path = entry.path();
        if kind.is_dir() {
            ui_files(root, &path, files);
        } else if kind.is_file() {
            let relative = path.strip_prefix(root).expect("asset below UI root");
            files.push((
                relative
                    .to_str()
                    .expect("UTF-8 asset name")
                    .replace('\\', "/"),
                path,
            ));
        }
    }
}

fn main() {
    stamp_commit();
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let package = root.join("../../package.json");
    println!("cargo:rerun-if-changed={}", package.display());
    let package: serde_json::Value =
        serde_json::from_slice(&fs::read(package).expect("read application package"))
            .expect("parse application package");
    println!(
        "cargo:rustc-env=AAC_VERSION={}",
        package["version"].as_str().expect("application version")
    );

    println!("cargo:rerun-if-env-changed=AAC_UI_EMBED_DIR");
    let mut files = Vec::new();
    if let Some(ui) = env::var_os("AAC_UI_EMBED_DIR") {
        let ui = fs::canonicalize(ui).expect("resolve AAC_UI_EMBED_DIR");
        println!("cargo:rerun-if-changed={}", ui.display());
        ui_files(&ui, &ui, &mut files);
        files.sort_by(|a, b| a.0.cmp(&b.0));
        assert!(
            files.iter().any(|asset| asset.0 == "app/index.html"),
            "AAC_UI_EMBED_DIR has no Leptos frame"
        );
    }
    let mut output = String::from("static EMBEDDED_UI: &[(&str, &[u8])] = &[\n");
    for (name, source) in files {
        output.push_str(&format!("({name:?}, include_bytes!({source:?})),\n"));
    }
    output.push_str("];\n");
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").expect("output directory")).join("ui-assets.rs"),
        output,
    )
    .expect("write packaged asset index");
}
