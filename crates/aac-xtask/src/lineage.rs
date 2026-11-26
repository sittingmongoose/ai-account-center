//! Deployment ancestry and private installation storage.
use crate::build::{Result, read_json};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
const OVERRIDE: &str = "AAC_ALLOW_DEPLOY_REGRESSION";
/// Whether a string is a full lowercase Git commit identifier.
pub fn commit(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn branch(v: &Value) -> Value {
    v.as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.as_bytes()[0].is_ascii_alphanumeric()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
        })
        .map_or(Value::Null, |s| json!(s))
}
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).to_string())
}
/// Read checkout lineage and refresh the main reference when necessary.
pub fn read_source(root: &Path) -> Option<Value> {
    let top = git(root, &["rev-parse", "--show-toplevel"])?;
    if fs::canonicalize(top.trim()).ok()? != fs::canonicalize(root).ok()? {
        return None;
    }
    let sha = git(root, &["rev-parse", "HEAD"])?.trim().to_owned();
    let rows: Vec<String> = git(root, &["rev-list", "--max-count=100000", "HEAD"])?
        .lines()
        .filter(|s| commit(s))
        .map(str::to_owned)
        .collect();
    if !commit(&sha) || rows.first() != Some(&sha) {
        return None;
    }
    let dirty = git(
        root,
        &[
            "status",
            "--porcelain",
            "--untracked-files=normal",
            "--",
            "src",
            "scripts",
            "lib",
            "config",
            "crates",
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            "web-dashboard",
            "browser-bridge",
            "macos-bar",
            "windows-bar",
            "package.json",
            "bun.lock",
            "tsconfig.json",
        ],
    )?;
    let name = git(root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let name = name.trim();
    let ancestor = || {
        git(
            root,
            &[
                "merge-base",
                "--is-ancestor",
                &sha,
                "refs/remotes/origin/main",
            ],
        )
        .is_some()
    };
    let mut main = ancestor();
    if !main {
        git(
            root,
            &[
                "fetch",
                "--quiet",
                "origin",
                "+refs/heads/main:refs/remotes/origin/main",
            ],
        );
        main = ancestor();
    }
    Some(
        json!({"version":1,"commit":sha,"branch":if name=="HEAD" {Value::Null} else {branch(&json!(name))},"dirty":!dirty.trim().is_empty(),"onMain":main,"shallow":git(root,&["rev-parse","--is-shallow-repository"] )?.trim()=="true","builtAt":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"lineage":rows}),
    )
}
/// Write build metadata into the destination directory.
pub fn record(root: &Path, out: &Path) -> Result<Value> {
    let v=read_source(root).unwrap_or_else(||json!({"version":1,"commit":null,"branch":null,"dirty":null,"onMain":null,"builtAt":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"lineage":[]}));
    fs::create_dir_all(out)?;
    fs::write(out.join("build-info.json"), format!("{v}\n"))?;
    Ok(v)
}
/// Validate and normalize recorded build metadata.
pub fn parse_build(v: &Value) -> Option<Value> {
    if v["version"] != 1 {
        return None;
    }
    if v["commit"].is_null() && v.get("commit").is_some() {
        return Some(json!({"commit":null,"branch":null,"dirty":null,"onMain":null,"lineage":[]}));
    }
    let c = v["commit"].as_str()?;
    let rows = v["lineage"].as_array()?;
    if !commit(c)
        || !v["dirty"].is_boolean()
        || rows.first() != Some(&v["commit"])
        || !rows.iter().all(|c| c.as_str().is_some_and(commit))
    {
        return None;
    }
    Some(
        json!({"commit":c,"branch":branch(&v["branch"]),"dirty":v["dirty"],"onMain":v["onMain"]==true,"shallow":v["shallow"]==true,"lineage":rows}),
    )
}
/// Validate the running dashboard deployment record.
pub fn parse_ledger(v: &Value) -> Option<Value> {
    (v["version"] == 1 && v["commit"].as_str().is_some_and(commit)).then(
        || json!({"commit":v["commit"],"branch":branch(&v["branch"]),"dirty":v["dirty"]==true}),
    )
}
fn describe(v: &Value) -> String {
    let sha = v["commit"].as_str().unwrap_or("");
    let short = &sha[..sha.len().min(8)];
    v["branch"]
        .as_str()
        .map_or_else(|| short.to_owned(), |b| format!("{short} ({b})"))
}
/// Explain why a build cannot be installed.
pub fn build_problem(v: &Value) -> Option<String> {
    if v["commit"].as_str().is_none() {
        return Some("This package does not record the commit it was built from, so it cannot be shown to be a committed build of origin/main. Build it from a git checkout of main.".into());
    }
    if v["dirty"] == true {
        return Some(format!(
            "This package was built from {} with uncommitted changes. A later build could not keep them. Commit, merge into main and rebuild.",
            describe(v)
        ));
    }
    if v["onMain"] != true {
        return Some(format!(
            "This package was built from {}, which is not on origin/main. A fix installed before it is merged is removed by the next build of main. Merge it into main, push, then rebuild and install.",
            describe(v)
        ));
    }
    None
}
/// Explain whether installation would discard a deployed fix.
pub fn running_problem(v: &Value, ledger: &Value) -> Option<String> {
    if ledger.is_null() {
        return None;
    }
    let running = describe(ledger);
    if ledger["dirty"] == true {
        return Some(format!(
            "The dashboard on this computer runs {running} with uncommitted changes, which this package cannot prove it keeps."
        ));
    }
    if !v["lineage"]
        .as_array()
        .is_some_and(|a| a.contains(&ledger["commit"]))
    {
        let c = ledger["commit"].as_str().unwrap_or("");
        let b = ledger["branch"].as_str().unwrap_or(&c[..c.len().min(8)]);
        return Some(format!(
            "This package ({}) does not contain {running}, which the dashboard on this computer runs. Installing it would remove deployed fixes. Merge {b} into main, rebuild from main and install that package.{}",
            describe(v),
            if v["shallow"] == true {
                " It was built from a shallow clone with incomplete history; build from a full clone."
            } else {
                ""
            }
        ));
    }
    None
}
/// Apply the explicit emergency rollback override to a refusal.
pub fn verdict(problem: Option<String>, override_allowed: bool) -> Value {
    match problem {
        None => json!({"ok":true,"message":null}),
        Some(p) if override_allowed => {
            json!({"ok":true,"message":format!("{p} Installing anyway because {OVERRIDE}=1.")})
        }
        Some(p) => {
            json!({"ok":false,"message":format!("{p} Only for an intentional rollback or emergency, set {OVERRIDE}=1.")})
        }
    }
}
fn bounded_json(p: &Path) -> Value {
    fs::symlink_metadata(p)
        .ok()
        .filter(|m| m.is_file() && m.len() <= 8 * 1024 * 1024)
        .and_then(|_| read_json(p).ok())
        .unwrap_or(Value::Null)
}
/// Check a package against a bounded deployment ledger.
pub fn check(root: &Path, ccs: &Path, override_allowed: bool) -> Value {
    let v = parse_build(&bounded_json(&root.join("dist/build-info.json"))).unwrap_or(Value::Null);
    let file = ccs.join("deployed-build.json");
    let ledger = parse_ledger(&bounded_json(&file));
    let unreadable = match fs::symlink_metadata(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        _ => ledger.is_none(),
    };
    verdict(build_problem(&v).or_else(||unreadable.then(||format!("The running build record {} cannot be read, so this package cannot be checked against it. Restart the dashboard to rewrite it.",file.display()))).or_else(||running_problem(&v,&ledger.unwrap_or(Value::Null))),override_allowed)
}
/// Resolve compatible account storage environment settings.
pub fn ccs_dir() -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    if let Some(v) = std::env::var_os("CCS_DIR").filter(|v| !v.is_empty()) {
        return Ok(cwd.join(v));
    }
    let home = std::env::var_os("CCS_HOME")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }))
        .ok_or("Cannot locate home directory")?;
    Ok(cwd.join(home).join(".ccs"))
}
/// Require the exact clean landed release checkout.
pub fn require_landed(root: &Path, v: &Value, expected: &str) -> Result<()> {
    if !commit(expected) || v["commit"] != expected {
        return Err("Check out the exact landed release commit".into());
    }
    if v["shallow"] != false {
        return Err("Use a full source checkout for deployment lineage".into());
    }
    if v["onMain"] != true {
        return Err("Wait for origin/main to contain the release before staging".into());
    }
    if v["dirty"] != false
        || git(root, &["status", "--porcelain", "--untracked-files=normal"])
            .is_none_or(|s| !s.trim().is_empty())
    {
        return Err("Use a clean landed release checkout".into());
    }
    Ok(())
}
/// Create private storage while preserving existing files and symlinks.
pub fn initialize(dir: &Path) -> Result<()> {
    match fs::metadata(dir) {
        Ok(m) if m.is_dir() => return Ok(()),
        Ok(_) => return Err("Account configuration path is not a directory.".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    if fs::symlink_metadata(dir).is_ok() {
        return Err("Account configuration path is unavailable.".into());
    }
    let mut b = fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)?;
    Ok(())
}
/// Whether this package is a source checkout.
pub fn source_checkout(root: &Path) -> bool {
    root.join(".git").exists() || root.join("src/ccs.ts").exists()
}
/// Print a lineage result and preserve its exit status.
pub fn print_verdict(v: Value) -> Result<()> {
    if let Some(s) = v["message"].as_str() {
        if v["ok"] == true {
            println!("[!] {s}");
        } else {
            eprintln!("[X] {s}");
        }
    }
    if v["ok"] == true {
        Ok(())
    } else {
        Err(Box::new(crate::SilentExit(1)))
    }
}
/// Dispatch this module’s command line operations.
pub fn run(root: &Path, args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("record-build") => {
            let v = record(root, &root.join("dist"))?;
            if v["commit"].is_string() {
                println!(
                    "[OK] Build lineage: {}, {} commits{}{}",
                    describe(&v),
                    v["lineage"].as_array().map_or(0, Vec::len),
                    if v["onMain"] == true {
                        ", on origin/main"
                    } else {
                        ", NOT on origin/main (installation will refuse it)"
                    },
                    if v["dirty"] == true {
                        ", with uncommitted changes (installation will refuse it)"
                    } else {
                        ""
                    }
                );
            } else {
                println!(
                    "[!] Build lineage: not a git checkout; installation cannot verify this build."
                );
            }
            Ok(())
        }
        Some("preflight") => print_verdict(verdict(
            read_source(root).map_or_else(
                || Some("The checkout commits could not be read with git.".into()),
                |v| build_problem(&v),
            ),
            std::env::var(OVERRIDE).as_deref() == Ok("1"),
        )),
        Some("check") => print_verdict(check(
            root,
            &ccs_dir()?,
            std::env::var(OVERRIDE).as_deref() == Ok("1"),
        )),
        _ => Err("Usage: aac-xtask release lineage record-build|preflight|check".into()),
    }
}
