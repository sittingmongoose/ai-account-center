//! Offline repository hygiene checks and deterministic test inventory.
use crate::build::{Result, sha};
use indexmap::IndexSet;
use regex::Regex;
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::Path,
    process::Command,
    sync::LazyLock,
};
fn policy() -> Value {
    serde_json::from_str(include_str!("guard-policy.json")).expect("static guard policy")
}
fn rx(s: &str) -> Regex {
    regex::RegexBuilder::new(
        &s.replace(r"\b", r"(?-u:\b)")
            .replace(r"\s", r"[ \t\r\n\x0B\x0C]"),
    )
    .build()
    .expect("static regex")
}
fn tracked(root: &Path, others: bool) -> Result<Vec<String>> {
    let mut c = Command::new("git");
    c.arg("-C").arg(root).args(["ls-files", "-z"]);
    if others {
        c.args(["--cached", "--others", "--exclude-standard"]);
    }
    let o = c.output()?;
    if !o.status.success() {
        return Err("Cannot list tracked files".into());
    }
    Ok(String::from_utf8(o.stdout)?
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect())
}
/// One hit per unique word per line; address prefixes retain duplicate occurrences.
pub fn scan_text(text: &str, words: &BTreeSet<String>, ips: &BTreeSet<String>) -> Vec<Value> {
    scan_cached(text, words, ips, &mut HashMap::new())
}
fn cached_sha(value: &str, cache: &mut HashMap<String, String>) -> String {
    cache
        .entry(value.to_owned())
        .or_insert_with(|| sha(value.as_bytes()))
        .clone()
}
fn scan_cached(
    text: &str,
    words: &BTreeSet<String>,
    ips: &BTreeSet<String>,
    cache: &mut HashMap<String, String>,
) -> Vec<Value> {
    let mut hits = Vec::new();
    static IPV4: LazyLock<Regex> =
        LazyLock::new(|| rx(r"([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})\.[0-9]{1,3}"));
    static IPV6: LazyLock<Regex> =
        LazyLock::new(|| rx(r"(?i)::ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})\b"));
    for (i, line) in text.split('\n').enumerate() {
        let mut candidates = IndexSet::new();
        for raw in line
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|s| !s.is_empty())
        {
            candidates.insert(raw.to_ascii_lowercase());
            let b = raw.as_bytes();
            let mut start = 0;
            for n in 1..b.len() {
                if (b[n - 1].is_ascii_lowercase() && b[n].is_ascii_uppercase())
                    || (b[n - 1].is_ascii_alphabetic() && b[n].is_ascii_digit())
                    || (b[n - 1].is_ascii_digit() && b[n].is_ascii_alphabetic())
                {
                    candidates.insert(raw[start..n].to_ascii_lowercase());
                    start = n;
                }
            }
            if start > 0 {
                candidates.insert(raw[start..].to_ascii_lowercase());
            }
        }
        for w in candidates {
            let h = cached_sha(&w, cache);
            if words.contains(&h) {
                hits.push(json!({"line":i+1,"hash":h,"kind":"word"}));
            }
        }
        for c in IPV4.captures_iter(line) {
            let at = c.get(0).expect("match").start();
            if at > 0
                && (line.as_bytes()[at - 1].is_ascii_digit() || line.as_bytes()[at - 1] == b'.')
            {
                continue;
            }
            let prefix = format!(
                "{}.{}.{}",
                c[1].parse::<u16>().unwrap_or(0),
                c[2].parse::<u16>().unwrap_or(0),
                c[3].parse::<u16>().unwrap_or(0)
            );
            let h = cached_sha(&prefix, cache);
            if ips.contains(&h) {
                hits.push(json!({"line":i+1,"hash":h,"kind":"ipv4"}));
            }
        }
        for c in IPV6.captures_iter(line) {
            let high = u16::from_str_radix(&c[1], 16).unwrap_or(0);
            let low = u16::from_str_radix(&c[2], 16).unwrap_or(0);
            let h = cached_sha(&format!("{}.{}.{}", high >> 8, high & 255, low >> 8), cache);
            if ips.contains(&h) {
                hits.push(json!({"line":i+1,"hash":h,"kind":"ipv4"}));
            }
        }
    }
    hits
}
/// Scan tracked text with the pinned hashed personal-detail policy.
pub fn personal(root: &Path) -> Result<Vec<Value>> {
    let p = policy();
    let words = serde_json::from_value(p["words"].clone())?;
    let ips = serde_json::from_value(p["ipv4"].clone())?;
    let mut problems = Vec::new();
    let mut cache = HashMap::new();
    for file in tracked(root, false)? {
        if file.split('/').any(|s| s == "node_modules") {
            continue;
        }
        let path = root.join(&file);
        let Ok(m) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !m.is_file() || m.len() > 8 * 1024 * 1024 {
            continue;
        }
        let b = fs::read(path)?;
        if b[..b.len().min(8000)].contains(&0) {
            continue;
        }
        let hits = scan_cached(&String::from_utf8_lossy(&b), &words, &ips, &mut cache);
        let mut groups: Vec<(String, Vec<&Value>)> = Vec::new();
        for hit in &hits {
            let h = hit["hash"].as_str().unwrap_or("");
            if let Some((_, v)) = groups.iter_mut().find(|(k, _)| k == h) {
                v.push(hit);
            } else {
                groups.push((h.to_string(), vec![hit]));
            }
        }
        for (h, rows) in groups {
            if rows.len() as u64 > p["allowed"][&file][&h].as_u64().unwrap_or(0) {
                problems.push(json!({"file":file,"kind":rows[0]["kind"],"lines":rows.iter().map(|h|h["line"].clone()).collect::<Vec<_>>(),"count":rows.len()}));
            }
        }
    }
    Ok(problems)
}
fn walk(root: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    if !root.is_dir() {
        return Ok(());
    }
    let mut entries = fs::read_dir(root)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if e.file_type()?.is_dir() {
            if ![
                "node_modules",
                "__pycache__",
                ".git",
                "target",
                "bin",
                "obj",
            ]
            .contains(&e.file_name().to_string_lossy().as_ref())
            {
                walk(&e.path(), out)?;
            }
        } else if e.file_type()?.is_file() {
            out.push(e.path());
        }
    }
    Ok(())
}
/// Find packaged source files containing CRLF line endings.
pub fn crlf(root: &Path) -> Result<Vec<String>> {
    let mut files = Vec::new();
    for f in [
        "scripts",
        "config",
        "crates",
        "native",
        "web-dashboard/leptos",
        "web-dashboard/public",
    ] {
        walk(&root.join(f), &mut files)?;
    }
    let text =
        rx(r"\.(py|js|cjs|mjs|ts|rs|json|md|sh|ps1|cs|txt|toml|yaml|yml|html|css|svg)$|^[^.]+$");
    let mut found = Vec::new();
    for p in files {
        if text.is_match(&p.file_name().unwrap_or_default().to_string_lossy())
            && fs::metadata(&p)?.len() <= 16 * 1024 * 1024
            && fs::read(&p)?.windows(2).any(|b| b == b"\r\n")
        {
            found.push(p.strip_prefix(root)?.to_string_lossy().replace('\\', "/"));
        }
    }
    found.sort();
    Ok(found)
}
/// Generate the deterministic test coverage inventory bytes and count.
pub fn inventory(root: &Path) -> Result<(String, usize)> {
    let mut rows = Vec::new();
    let files: BTreeSet<_> = tracked(root, true)?.into_iter().collect();
    let test = rx(
        r"(?:\.(?:test|spec)\.[cm]?[jt]s|(?:_test|test_)[A-Za-z0-9_]*\.py|Tests?\.swift|Tests?\.cs)$",
    );
    let rust = rx(r"#\[(?:tokio::)?test\]");
    for f in files {
        if f.starts_with("node_modules") || !root.join(&f).is_file() {
            continue;
        }
        let source = fs::read_to_string(root.join(&f)).unwrap_or_default();
        if !(test.is_match(&f)
            || (f.ends_with(".rs") && rust.is_match(&source))
            || (f.starts_with("web-dashboard/tests/browser/checks/")
                && rx(r"\.[cm]?[jt]s$").is_match(&f)))
        {
            continue;
        }
        let mut e = Vec::new();
        if rx(r"(?i)auth|session|origin|confirm|auto-switch|identity|reset.claim|guard|trusted|rate.limit").is_match(&f){e.push("safety");}
        if rx(r"\.listen\(|startServer\(|createServer\(|supertest|fetch\(|browser-check (run|audit|diff|report)|\.goto\(").is_match(&source){e.push("HTTP");}
        if f.ends_with(".py")
            || rx(r"python3|pythonHelper|runPython|Bun\.spawn|spawnSync|execFileSync")
                .is_match(&source)
        {
            e.push("helper");
        }
        if rx(r"\.each\(|testCases|cases\.json|vectors\.json").is_match(&source) {
            e.push("table");
        }
        let kind = e.first().copied().unwrap_or("internal");
        let mut row = json!({"file":f,"kind":kind,"evidence":e});
        if kind == "internal" {
            row["behaviourTest"] = json!("uncovered");
        }
        rows.push(row);
    }
    let n = rows.len();
    let v = json!({"schemaVersion":1,"policy":"Priority: safety, HTTP, helper, table, internal. Static classification only; internal mappings deliberately say uncovered until proven by a behaviour test.","tests":rows});
    Ok((format!("{}\n", serde_json::to_string_pretty(&v)?), n))
}
/// Strip Markdown code while preserving prose links.
pub fn without_code(text: &str) -> String {
    let mut out = String::new();
    let mut fence: Option<String> = None;
    for line in text.split_inclusive('\n') {
        if let Some(f) = &fence {
            if line.starts_with(f) {
                fence = None;
            }
            continue;
        }
        if line.starts_with("```") || line.starts_with("~~~") {
            fence = Some(line[..3].to_string());
            continue;
        }
        out.push_str(line);
    }
    let out = out.replace("\\`", "");
    let mut clean = String::new();
    let bytes = out.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'`' {
            let n = bytes[at..].iter().take_while(|b| **b == b'`').count();
            let marker = "`".repeat(n);
            if let Some(end) = out[at + n..]
                .find(&marker)
                .filter(|end| !out[at + n..at + n + end].contains('\n'))
            {
                at += n + end + n;
                continue;
            }
        }
        let c = out[at..].chars().next().expect("character");
        clean.push(c);
        at += c.len_utf8();
    }
    clean
}
/// Check retained documentation contracts and relative links.
pub fn docs(root: &Path) -> Result<Vec<String>> {
    let p = policy();
    let mut failures = Vec::new();
    for f in p["removed"].as_array().ok_or("removed")? {
        let f = f.as_str().ok_or("path")?;
        if root.join(f).exists() {
            failures.push(format!(
                "{f} is retired; maintain the current product guides instead"
            ));
        }
    }
    for f in p["retained"].as_array().ok_or("retained")? {
        let f = f.as_str().ok_or("path")?;
        if !root.join(f).exists() {
            failures.push(format!("{f} is missing from the retained product"));
        }
    }
    for r in p["requirements"].as_array().ok_or("requirements")? {
        let f = r[0].as_str().ok_or("path")?;
        let s = r[1].as_str().ok_or("text")?;
        match fs::read_to_string(root.join(f)) {
            Ok(t) if t.contains(s) => {}
            Ok(_) => failures.push(format!("{f} is missing: {s}")),
            Err(_) => failures.push(format!("{f} is missing")),
        }
    }
    let mut practical = vec![
        "README.md",
        "CLAUDE.md",
        "CONTRIBUTING.md",
        "SECURITY.md",
        "docker/README.md",
        "macos-bar/README.md",
    ]
    .into_iter()
    .map(|f| root.join(f))
    .collect::<Vec<_>>();
    let mut more = Vec::new();
    walk(&root.join("docs"), &mut more)?;
    practical.extend(
        more.iter()
            .filter(|f| rx(r"\.mdx?$").is_match(&f.to_string_lossy()))
            .cloned(),
    );
    let mut issues = Vec::new();
    walk(&root.join(".github/ISSUE_TEMPLATE"), &mut issues)?;
    practical.extend(
        issues
            .into_iter()
            .filter(|f| rx(r"\.(md|ya?ml)$").is_match(&f.to_string_lossy())),
    );
    for old in p["removed"].as_array().ok_or("removed")? {
        for path in &practical {
            if fs::read_to_string(path)?.contains(old.as_str().unwrap_or("")) {
                failures.push(format!(
                    "{} references deleted guide: {}",
                    path.strip_prefix(root)?.display(),
                    old.as_str().unwrap_or("")
                ));
            }
        }
    }
    let mut markdown = vec![
        "README.md",
        "CLAUDE.md",
        "CONTRIBUTING.md",
        "SECURITY.md",
        "tests/README.md",
        "tests/npm/README.md",
        "web-dashboard/README.md",
        "docker/README.md",
        "macos-bar/README.md",
        "windows-bar/README.md",
    ]
    .into_iter()
    .map(|f| root.join(f))
    .collect::<Vec<_>>();
    markdown.extend(
        more.into_iter()
            .filter(|f| rx(r"\.mdx?$").is_match(&f.to_string_lossy())),
    );
    for path in markdown {
        let source = without_code(&fs::read_to_string(&path)?);
        let rel = path.strip_prefix(root)?.to_string_lossy();
        for c in rx(r"!?\[[^\]]*\]\(([^)]+)\)").captures_iter(&source) {
            let original = &c[1];
            let target = original
                .trim()
                .trim_start_matches('<')
                .trim_end_matches('>');
            if target.is_empty()
                || target.starts_with('#')
                || rx(r"(?i)^(https?:|mailto:|tel:)").is_match(target)
            {
                continue;
            }
            let target = target.split(['#', '?']).next().unwrap_or("");
            let mut decoded = Vec::new();
            let mut i = 0;
            let b = target.as_bytes();
            let mut invalid = false;
            while i < b.len() {
                if b[i] == b'%' {
                    if i + 2 >= b.len() {
                        invalid = true;
                        break;
                    }
                    if let Ok(v) =
                        u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16)
                    {
                        decoded.push(v);
                        i += 3;
                    } else {
                        invalid = true;
                        break;
                    }
                } else {
                    decoded.push(b[i]);
                    i += 1;
                }
            }
            let decoded = String::from_utf8(decoded);
            if invalid || decoded.is_err() {
                failures.push(format!("{rel} has invalid link encoding: {original}"));
                continue;
            }
            let decoded = decoded.unwrap_or_default();
            let dest = path.parent().ok_or("parent")?.join(&decoded);
            let mut normalized = std::path::PathBuf::new();
            for comp in dest.components() {
                match comp {
                    std::path::Component::ParentDir => {
                        normalized.pop();
                    }
                    std::path::Component::CurDir => {}
                    c => normalized.push(c.as_os_str()),
                }
            }
            if Path::new(&decoded).is_absolute() || !normalized.starts_with(root) {
                failures.push(format!("{rel} links outside the repository: {original}"));
            } else if !normalized.exists() {
                failures.push(format!("{rel} has missing relative link: {original}"));
            }
        }
    }
    Ok(failures)
}
/// Dispatch this module’s command line operations.
pub fn run(root: &Path, args: &[String]) -> Result<()> {
    let root_arg = args
        .iter()
        .position(|s| s == "--root")
        .and_then(|i| args.get(i + 1))
        .map(|s| std::env::current_dir().unwrap_or_default().join(s));
    let root = root_arg.as_deref().unwrap_or(root);
    match args.first().map(String::as_str) {
        Some("one-file-policy") => crate::file_policy::run(root),
        Some("quickstart") => quickstart(root),
        Some("personal") => {
            let p = personal(root)?;
            if p.is_empty() {
                println!("[OK] No blocked personal details in tracked files.");
                return Ok(());
            }
            eprintln!(
                "[X] Blocked personal details found in tracked files.\n    Replace them with neutral placeholders (see docs/code-standards.md)."
            );
            for p in p {
                eprintln!(
                    "    {}: {} on line(s) {}",
                    p["file"].as_str().unwrap_or(""),
                    p["kind"].as_str().unwrap_or(""),
                    p["lines"]
                        .as_array()
                        .ok_or("lines")?
                        .iter()
                        .take(10)
                        .map(Value::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            Err(Box::new(crate::SilentExit(1)))
        }
        Some("line-endings") => {
            let p = crlf(root)?;
            if p.is_empty() {
                println!("[OK] Packaged scripts use LF line endings.");
                Ok(())
            } else {
                eprintln!(
                    "[X] {} packaged file(s) have CRLF line endings, which break the helpers' SHA-256 pins:",
                    p.len()
                );
                for f in p.iter().take(20) {
                    eprintln!("    {f}");
                }
                if p.len() > 20 {
                    eprintln!("    ... and {} more", p.len() - 20);
                }
                eprintln!(
                    "    Build from a fresh checkout of the same commit, which .gitattributes keeps LF:\n    git worktree add <new folder> HEAD   (this working tree and its edits stay as they are)"
                );
                Err(Box::new(crate::SilentExit(1)))
            }
        }
        Some("docs") => {
            let p = docs(root)?;
            if p.is_empty() {
                println!(
                    "[OK] Documentation pointers, retained contracts, and relative links are current."
                );
                Ok(())
            } else {
                eprintln!("[X] Documentation freshness checks failed:");
                for f in p {
                    eprintln!("    {f}");
                }
                Err(Box::new(crate::SilentExit(1)))
            }
        }
        Some("inventory") => {
            let (bytes, n) = inventory(root)?;
            let file = root.join("tests/spec/test-inventory.json");
            if args.iter().any(|s| s == "--regenerate") {
                fs::write(file, bytes)?;
            } else if fs::read_to_string(file)? != bytes {
                return Err("Test inventory drift; regenerate and review".into());
            }
            println!("{n} test files inventoried");
            Ok(())
        }
        _ => Err(
            "Usage: aac-xtask check personal|line-endings|docs|inventory [--root <directory>]"
                .into(),
        ),
    }
}

/// Compare all marked quickstart blocks with the canonical snippet.
pub fn quickstart(root: &Path) -> Result<()> {
    fn block(text: &str) -> String {
        let mut active = false;
        let mut lines = Vec::new();
        for line in text.lines() {
            if line.contains("<!-- quickstart-snippet-start -->") {
                active = true;
            }
            if active {
                lines.push(line);
            }
            if line.contains("<!-- quickstart-snippet-end -->") {
                active = false;
            }
        }
        lines.join("\n")
    }
    let expected = block(&fs::read_to_string(
        root.join("docs/quickstart-snippet.md"),
    )?);
    let mut failed = false;
    for file in ["README.md", "docker/README.md"] {
        let found = block(&fs::read_to_string(root.join(file))?);
        if found == expected {
            println!("[OK] {file} snippet matches canonical");
        } else {
            failed = true;
            eprintln!(
                "[X] {file} quickstart snippet drift detected\n--- canonical (docs/quickstart-snippet.md) ---\n{expected}\n--- found in {file} ---\n{found}"
            );
        }
    }
    if failed {
        Err(Box::new(crate::SilentExit(1)))
    } else {
        Ok(())
    }
}
