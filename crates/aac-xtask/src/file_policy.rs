//! Port of the one-file security invariant and counted cut receipts.
use crate::build::{Result, sha};
use regex::Regex;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};
const PATTERNS: &[(&str, &str)] = &[
    ("O_NOFOLLOW", r"\bO_NOFOLLOW\b"),
    ("custom_flags", r"\bcustom_flags\s*\("),
    ("OpenOptions", r"\bOpenOptions\b"),
    ("File::open", r"\bFile::open\s*\("),
    ("File::create", r"\bFile::create\s*\("),
    ("fs::read", r"\bfs::read\s*\("),
    ("fs::read_to_string", r"\bfs::read_to_string\s*\("),
    ("fs::write", r"\bfs::write\s*\("),
    ("fs::rename", r"\bfs::rename\s*\("),
    ("persist", r"\.persist(?:_noclobber)?\s*\("),
];
fn rx(s: &str) -> Regex {
    regex::RegexBuilder::new(
        &s.replace(r"\b", r"(?-u:\b)")
            .replace(r"\s", r"[ \t\r\n\x0B\x0C]"),
    )
    .build()
    .expect("policy regex")
}
fn matching(source: &str, open: usize) -> usize {
    let b = source.as_bytes();
    let mut i = open;
    let mut depth = 0;
    while i < b.len() {
        if b[i..].starts_with(b"//") {
            i = source[i..].find('\n').map_or(b.len(), |n| i + n);
            continue;
        }
        if b[i..].starts_with(b"/*") {
            i = source[i + 2..].find("*/").map_or(b.len(), |n| i + n + 4);
            continue;
        }
        if (b[i] == b'r' || b[i] == b'b')
            && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_'))
        {
            let mut at = i;
            if b[at] == b'b' {
                at += 1;
            }
            if b.get(at) == Some(&b'r') {
                at += 1;
                let start = at;
                while b.get(at) == Some(&b'#') {
                    at += 1;
                }
                if b.get(at) == Some(&b'"') {
                    let end = format!("\"{}", "#".repeat(at - start));
                    i = source[at + 1..]
                        .find(&end)
                        .map_or(b.len(), |n| at + 1 + n + end.len());
                    continue;
                }
            }
        }
        if b[i] == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if b[i] == b'\'' {
            let end = if b.get(i + 1) == Some(&b'\\') {
                i + 3
            } else {
                i + 2
            };
            if b.get(end) == Some(&b'\'') {
                i = end + 1;
                continue;
            }
        }
        if b[i] == b'{' {
            depth += 1;
        }
        if b[i] == b'}' {
            depth -= 1;
            if depth == 0 {
                return i;
            }
        }
        i += 1;
    }
    b.len().saturating_sub(1)
}
/// Blank test modules with original line numbers preserved.
pub fn without_tests(source: &str) -> String {
    let test = rx(r"^\s*#\[cfg\((?:test|all\((?:test\s*,.*|.*?,\s*test)\))\)\]\s*$");
    let module = rx(r"^\s*(pub(\([^)]*\))?\s+)?mod\s+[A-Za-z0-9_]+\s*\{");
    let lines = source.split('\n').collect::<Vec<_>>();
    let mut starts = Vec::new();
    let mut at = 0;
    for line in &lines {
        starts.push(at);
        at += line.len() + 1;
    }
    let mut out = source.as_bytes().to_vec();
    for (i, line) in lines.iter().enumerate() {
        if !test.is_match(line) {
            continue;
        }
        let mut j = i + 1;
        while j < lines.len() && (lines[j].trim().starts_with("#[") || lines[j].trim().is_empty()) {
            j += 1;
        }
        if j >= lines.len() || !module.is_match(lines[j]) {
            continue;
        }
        let open = starts[j] + lines[j].find('{').unwrap_or(0);
        let close = matching(source, open);
        for c in &mut out[starts[i]..=close] {
            if *c != b'\n' {
                *c = b' ';
            }
        }
    }
    String::from_utf8(out).expect("blanked UTF8")
}
/// Scan operations, consuming each allowed cut receipt exactly once.
pub fn scan(files: &[(String, String)], allow: &Value, receipt: &Value) -> Value {
    let mut used = BTreeSet::new();
    let mut violations = Vec::new();
    let mut outstanding: BTreeMap<String, Vec<String>> = receipt
        .as_object()
        .into_iter()
        .flatten()
        .map(|(f, e)| {
            (
                f.clone(),
                e["uses"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
            )
        })
        .collect();
    let patterns = PATTERNS
        .iter()
        .map(|(n, r)| (*n, rx(r)))
        .collect::<Vec<_>>();
    let allow = allow.as_array().expect("policy array");
    for (relative, source) in files {
        if relative.starts_with("crates/aac-files/src/")
            || source.starts_with("#![cfg(test)]\n")
            || source.starts_with("#![cfg(test)]\r\n")
        {
            continue;
        }
        for (i, line) in without_tests(source).split('\n').enumerate() {
            let code = line.split("//").next().unwrap_or("");
            for (name, pattern) in &patterns {
                if !pattern.is_match(code) {
                    continue;
                }
                if let Some(e) = allow.iter().find(|e| {
                    let p = e["path"].as_str().unwrap_or("");
                    if p.ends_with('/') {
                        relative.starts_with(p)
                    } else {
                        relative == p
                    }
                }) {
                    used.insert(e["path"].as_str().unwrap_or("").to_owned());
                    continue;
                }
                let key = format!("{name}:{}", sha(code.trim().as_bytes()));
                let admitted = outstanding
                    .get_mut(relative)
                    .and_then(|uses| uses.iter().position(|s| s == &key).map(|i| uses.remove(i)));
                if admitted.is_none() {
                    violations.push(format!("{relative}:{}: {name}", i + 1));
                }
            }
        }
    }
    let mut stale = allow
        .iter()
        .filter_map(|e| e["path"].as_str())
        .filter(|p| !used.contains(*p))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for (f, remaining) in outstanding {
        if !remaining.is_empty() {
            stale.push(format!("{f} ({} cut receipts)", remaining.len()));
        }
    }
    let mut unexplained = allow
        .iter()
        .filter(|e| e["reason"].as_str().unwrap_or("").trim().len() < 40)
        .filter_map(|e| e["path"].as_str())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for (f, e) in receipt.as_object().into_iter().flatten() {
        if e["reason"].as_str().unwrap_or("").trim().len() < 40 {
            unexplained.push(f.clone());
        }
    }
    json!({"violations":violations,"stale":stale,"unexplained":unexplained})
}
fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) -> Result<()> {
    let mut entries = fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if e.path().is_dir() {
            walk(root, &e.path(), out)?;
        } else if e.path().extension().is_some_and(|s| s == "rs") {
            out.push((
                e.path()
                    .strip_prefix(root)?
                    .to_string_lossy()
                    .replace('\\', "/"),
                fs::read_to_string(e.path())?,
            ));
        }
    }
    Ok(())
}
/// Check all Rust source against the pinned policy and cut receipt data.
pub fn run(root: &Path) -> Result<()> {
    let p: Value = serde_json::from_str(include_str!("file-policy.json"))?;
    let planted=vec![
      ("crates/x/src/a.rs".into(), "fn f() { let _ = std::fs::File::open(\"p\"); }\n".into()),
      ("crates/x/src/b.rs".into(), "fn g() { o.custom_flags(libc::O_NOFOLLOW); }\n".into()),
      ("crates/x/src/c.rs".into(), "fn h() { fs::rename(a, b).ok(); t.persist(&p).ok(); }\n".into()),
      ("crates/x/src/d.rs".into(), "fn ok() {}\n#[cfg(test)]\nmod tests {\n    fn t() { let s = \"}\"; let c = '{'; std::fs::write(p, b\"x\"); }\n}\nfn after() { fs::write(p, b\"y\"); }\n".into()),
      ("crates/aac-files/src/e.rs".into(), "fn i() { std::fs::File::open(\"p\"); }\n".into()),
      ("crates/x/src/f.rs".into(), "// File::open( in a comment\nfn j() {}\n".into()),
      ("crates/x/src/g.rs".into(), "fn k() { let _ = std::fs::read(p); let _ = fs::read_to_string(p); }\nfn listing() { fs::read_dir(p); fs::read_link(p); }\n".into()),
      ("crates/x/src/h.rs".into(), "fn ok() {}\n#[cfg(all(test, unix))]\nmod tests {\n    fn t() { fs::write(p, br\"\\\"); let s = \"}\"; }\n}\nfn after() { fs::read(p); }\n".into()),
    ];
    let result = scan(&planted, &json!([]), &json!({}));
    let mut actual = result["violations"]
        .as_array()
        .ok_or("self-test violations")?
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    actual.sort_unstable();
    let expected = [
        "crates/x/src/a.rs:1: File::open",
        "crates/x/src/b.rs:1: O_NOFOLLOW",
        "crates/x/src/b.rs:1: custom_flags",
        "crates/x/src/c.rs:1: fs::rename",
        "crates/x/src/c.rs:1: persist",
        "crates/x/src/d.rs:6: fs::write",
        "crates/x/src/g.rs:1: fs::read",
        "crates/x/src/g.rs:1: fs::read_to_string",
        "crates/x/src/h.rs:6: fs::read",
    ];
    if actual != expected {
        return Err(format!("self-test: {actual:?}").into());
    }
    let stale = scan(
        &planted,
        &json!([{"path":"crates/x/src/zz.rs","reason":"x".repeat(40)}]),
        &json!({}),
    );
    if stale["stale"].as_array().is_none_or(|a| a.len() != 1) {
        return Err("self-test: a stale allowlist entry was not reported".into());
    }
    let (file, operation) = &planted[0];
    let receipt = json!({file:{"reason":"Only the original operation is admitted by this receipt.","uses":[format!("File::open:{}",sha(operation.trim().as_bytes()))]}});
    if scan(&[(file.clone(), operation.repeat(2))], &json!([]), &receipt)["violations"]
        .as_array()
        .is_none_or(|a| a.len() != 1)
    {
        return Err("self-test: a new use matched a spent cut receipt".into());
    }
    if scan(&[(file.clone(), String::new())], &json!([]), &receipt)["stale"]
        .as_array()
        .is_none_or(|a| a.len() != 1)
    {
        return Err("self-test: a stale cut receipt was not reported".into());
    }
    println!("one-file-policy self-test: the scan catches every planted copy");
    let mut files = Vec::new();
    for group in ["crates", "native"] {
        let mut crates = fs::read_dir(root.join(group))?.collect::<std::io::Result<Vec<_>>>()?;
        crates.sort_by_key(|e| e.file_name());
        for c in crates {
            let src = c.path().join("src");
            if src.is_dir() {
                walk(root, &src, &mut files)?;
            }
        }
    }
    let v = scan(&files, &p["allowlist"], &p["receipt"]);
    for (key, prefix, suffix) in [
        ("violations", "", " (use aac_files)"),
        ("stale", "stale allowlist entry ", ""),
        ("unexplained", "allowlist entry without a reason ", ""),
    ] {
        for s in v[key].as_array().ok_or("policy result")? {
            eprintln!(
                "one-file-policy: {prefix}{}{suffix}",
                s.as_str().unwrap_or("")
            );
        }
    }
    if ["violations", "stale", "unexplained"]
        .iter()
        .any(|k| v[k].as_array().is_none_or(|a| !a.is_empty()))
    {
        return Err(Box::new(crate::SilentExit(1)));
    }
    println!(
        "one-file-policy: no new file mechanics outside aac-files ({} allowlisted, {} cut files pinned)",
        p["allowlist"].as_array().ok_or("allowlist")?.len(),
        p["receipt"].as_object().ok_or("receipt")?.len()
    );
    Ok(())
}
