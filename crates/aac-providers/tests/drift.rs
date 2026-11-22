//! Drift tests for the shared tables (DESIGN-M3-COMPLETE 2.4): the generated constants equal the
//! JSON, workspace crates hold no provider or tool list literals, and each Rust hand list outside
//! the workspace (Leptos, native aac-store and aac-usage) equals its projection of the table.
//! A one-sided edit of either side fails here.
use aac_providers::*;
use serde_json::Value;
use std::{fs, path::Path, path::PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn json(text: &str) -> Value {
    serde_json::from_str(text).expect("table JSON")
}

fn source(relative: &str) -> String {
    fs::read_to_string(root().join(relative)).unwrap_or_else(|e| panic!("{relative}: {e}"))
}

/// The string literals between `start` (which must occur once) and the next `end`.
fn literals(text: &str, start: &str, end: &str, at: &str) -> Vec<String> {
    assert_eq!(
        text.matches(start).count(),
        1,
        "{at}: anchor {start:?} must occur once"
    );
    let from = text.find(start).expect("anchor") + start.len();
    let body = &text[from..from + text[from..].find(end).expect("end")];
    body.split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

/// [`literals`] of the first `start` after `anchor` (which must occur once).
fn literals_after(text: &str, anchor: &str, start: &str, end: &str, at: &str) -> Vec<String> {
    assert_eq!(
        text.matches(anchor).count(),
        1,
        "{at}: anchor {anchor:?} must occur once"
    );
    let rest = &text[text.find(anchor).expect("anchor")..];
    let from = rest.find(start).expect("start");
    let rest = &rest[from..];
    literals(
        &rest[..rest.find(end).expect("end") + end.len()],
        start,
        end,
        at,
    )
}

fn ids<'a>(rows: impl Iterator<Item = &'a Provider>) -> Vec<String> {
    rows.map(|p| p.id.as_str().to_owned()).collect()
}

#[test]
fn generated_provider_rows_equal_the_json() {
    if SYNTHETIC {
        return;
    }
    let table = json(PROVIDERS_JSON);
    let rows = table["providers"].as_object().expect("providers");
    assert_eq!(rows.len(), PROVIDERS.len());
    for ((id, row), p) in rows.iter().zip(PROVIDERS.iter()) {
        assert_eq!(p.id.as_str(), id);
        assert_eq!(p.label, row["label"]);
        assert_eq!(p.long_label, row["longLabel"]);
        assert_eq!(p.dashboard, row["dashboard"]);
        assert_eq!(p.card, row["card"]);
        assert_eq!(p.key_store, row["keyStore"]);
        assert_eq!(p.quota, row["quota"]);
        match &p.analytics {
            None => assert!(row["analytics"].is_null(), "{id}"),
            Some(a) => {
                assert_eq!(
                    a.order,
                    row["analytics"]["order"].as_u64().expect("order") as u32
                );
                assert_eq!(a.label, row["analytics"]["label"]);
            }
        }
        match &p.page {
            None => assert!(row["page"].is_null(), "{id}"),
            Some(page) => {
                assert_eq!(page.sign_in, row["page"]["signIn"]);
                assert_eq!(page.multi, row["page"]["multi"]);
            }
        }
        assert_eq!(p.failover.label, row["failover"]["label"]);
        let kinds: Vec<&str> = row["failover"]["kinds"]
            .as_array()
            .expect("kinds")
            .iter()
            .map(|k| k.as_str().expect("kind"))
            .collect();
        assert_eq!(p.failover.kinds, kinds.as_slice());
    }
    let retry = &table["planRetryAfter"];
    assert_eq!(PLAN_RETRY_AFTER.floor_s, retry["floorS"]);
    assert_eq!(PLAN_RETRY_AFTER.ceiling_s, retry["ceilingS"]);
    assert_eq!(PLAN_RETRY_AFTER.default_s, retry["defaultS"]);
}

#[test]
fn generated_tool_and_host_rows_equal_the_json() {
    if SYNTHETIC {
        return;
    }
    let tools = json(TOOLS_JSON);
    let analytics = tools["analyticsTools"].as_object().expect("analyticsTools");
    assert_eq!(analytics.len(), ANALYTICS_TOOLS.len());
    for ((id, row), tool) in analytics.iter().zip(ANALYTICS_TOOLS.iter()) {
        assert_eq!(tool.id, id);
        assert_eq!(tool.label, row["label"]);
        assert_eq!(tool.experiment, row["experiment"]);
        assert_eq!(tool.extra_roots, row["extraRoots"]);
    }
    for (key, rows) in [
        ("scannerOnlyKinds", &SCANNER_ONLY_KINDS[..]),
        ("usageLogTools", &USAGE_LOG_TOOLS[..]),
    ] {
        let table = tools[key].as_object().expect(key);
        assert_eq!(table.len(), rows.len(), "{key}");
        for ((id, row), tool) in table.iter().zip(rows) {
            assert_eq!(
                (tool.id, tool.label),
                (id.as_str(), row["label"].as_str().expect("label"))
            );
        }
    }
    let mapping = tools["usageLogFieldMapping"].as_object().expect("mapping");
    let generated: Vec<(&str, bool)> = USAGE_LOG_FIELD_MAPPING
        .iter()
        .map(|k| (k.name, k.required))
        .collect();
    let expected: Vec<(&str, bool)> = mapping
        .iter()
        .map(|(k, v)| (k.as_str(), v["required"].as_bool().expect("required")))
        .collect();
    assert_eq!(generated, expected);
    let hosts = json(HOSTS_JSON);
    assert_eq!(
        LEGACY_STORAGE_HOSTS.to_vec(),
        hosts["legacyStorageHosts"]
            .as_array()
            .expect("hosts")
            .iter()
            .map(|h| h.as_str().expect("host"))
            .collect::<Vec<_>>()
    );
}

#[test]
fn legacy_storage_hosts_are_computers() {
    let computers = json(&source("web-dashboard/public/computers.json"));
    for host in LEGACY_STORAGE_HOSTS {
        assert!(
            computers.get(host).is_some(),
            "{host} is not in computers.json"
        );
    }
}

/// Words a list literal of providers or tools would hold. Two-element dispatch arrays (the
/// failover core pair) are reader code, not a copy of the table, so the bar is three.
fn table_words() -> Vec<&'static str> {
    let mut words: Vec<&str> = PROVIDERS.iter().map(|p| p.id.as_str()).collect();
    words.extend(scanner_kinds());
    words.extend(USAGE_LOG_TOOLS.iter().map(|t| t.id));
    words.sort_unstable();
    words.dedup();
    words
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Every `[...]` array literal and `matches!(...)` pattern in `text` that names three or more
/// table words.
fn list_literals(text: &str, words: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'['
            && let Some(len) = text[i..].find(']')
        {
            spans.push(&text[i..i + len]);
        }
    }
    let mut rest = text;
    while let Some(at) = rest.find("matches!(") {
        let tail = &rest[at..];
        let end = tail.find(')').unwrap_or(tail.len());
        spans.push(&tail[..end]);
        rest = &tail[end..];
    }
    for span in spans {
        let named: Vec<&str> = span
            .split('"')
            .skip(1)
            .step_by(2)
            .filter(|s| words.contains(s))
            .collect();
        if named.len() >= 3 {
            found.push(named.join(","));
        }
    }
    found
}

/// `text` without its `#[cfg(test)]` items. A test fixture may name table words: the guard
/// holds production code to the tables, and a fixture that spells a provider out is data, not a
/// copy of a table a consumer reads. Nothing else is exempt.
fn production_source(text: &str) -> String {
    let marker = "#[cfg(test)]";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(marker) {
        out.push_str(&rest[..at]);
        let item = &rest[at + marker.len()..];
        let Some(open) = item.find('{') else { break };
        if let Some(semicolon) = item.find(';').filter(|end| *end < open) {
            rest = &item[semicolon + 1..];
            continue;
        }
        match block_end(item, open) {
            Some(end) => rest = &item[end..],
            None => break,
        }
    }
    out.push_str(rest);
    out
}

/// The index just past the `}` that closes the `{` at `open`, skipping strings and comments.
fn block_end(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i = text[i..].find('\n').map_or(bytes.len(), |end| i + end);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let mut nested = 0usize;
                while i < bytes.len() {
                    if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                        nested += 1;
                        i += 2;
                    } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                        nested -= 1;
                        i += 2;
                        if nested == 0 {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            b'"' => i = string_end(text, i),
            b'\'' => i = char_or_lifetime_end(text, i),
            b'r' | b'b' => i = raw_string_end(text, i).unwrap_or(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// The index just past the string literal that opens at `at`.
fn string_end(text: &str, at: usize) -> usize {
    let bytes = text.as_bytes();
    let mut i = at + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    bytes.len()
}

/// The index just past a raw string (`r".."`, `r#".."#`, `br#".."#`) at `at`, when one starts
/// there; `None` for any other `r` or `b`.
fn raw_string_end(text: &str, at: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_') {
        return None;
    }
    let mut i = at;
    if bytes[i] == b'b' {
        i += 1;
    }
    if bytes.get(i) != Some(&b'r') {
        return None;
    }
    i += 1;
    let mut hashes = 0;
    while bytes.get(i) == Some(&b'#') {
        hashes += 1;
        i += 1;
    }
    if bytes.get(i) != Some(&b'"') {
        return None;
    }
    i += 1;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            let mut closing = 0;
            while bytes.get(i + 1 + closing) == Some(&b'#') {
                closing += 1;
            }
            if closing == hashes {
                return Some(i + 1 + closing);
            }
        }
        i += 1;
    }
    Some(bytes.len())
}

/// The index just past a char literal at `at`; a lifetime (`'a`) skips nothing.
fn char_or_lifetime_end(text: &str, at: usize) -> usize {
    let bytes = text.as_bytes();
    if bytes.get(at + 1) == Some(&b'\\') {
        let mut i = at + 2;
        while i < bytes.len() && bytes[i] != b'\'' {
            i += 1;
        }
        return (i + 1).min(bytes.len());
    }
    if bytes.get(at + 2) == Some(&b'\'') {
        return at + 3;
    }
    at + 1
}

#[test]
fn workspace_crates_hold_no_table_list_literals() {
    let words = table_words();
    let mut files = Vec::new();
    // aac-store and aac-status joined the workspace in R3 as the NAPI crates they were in
    // native/; their hand lists stay checked for equality instead (secret.rs below, the
    // Status corpora), as Leptos's are. Deriving them from this crate is later L4 work.
    const CHECKED_NOT_SCANNED: [&str; 3] = ["aac-providers", "aac-store", "aac-status"];
    for entry in fs::read_dir(root().join("crates")).expect("crates") {
        let path = entry.expect("crate").path();
        if path
            .file_name()
            .is_some_and(|n| !CHECKED_NOT_SCANNED.iter().any(|skip| n == *skip))
            && path.join("src").is_dir()
        {
            rust_files(&path.join("src"), &mut files);
        }
    }
    assert!(files.len() > 20, "found {} files", files.len());
    let mut offenders = Vec::new();
    for file in files {
        // `*_tests.rs` files are fixtures, like the `#[cfg(test)]` items `production_source`
        // removes: a fixture may name providers and tools, a consumer may not.
        if file
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with("_tests.rs"))
        {
            continue;
        }
        let text = production_source(&fs::read_to_string(&file).expect("source"));
        for list in list_literals(&text, &words) {
            offenders.push(format!("{}: {list}", file.display()));
        }
    }
    assert!(
        offenders.is_empty(),
        "derive these from aac-providers instead:\n{}",
        offenders.join("\n")
    );
}

/// The guard's own scope: a test fixture may spell a provider list out, production code may not
/// (and nothing else is exempt).
#[test]
fn the_scan_skips_test_fixtures_and_still_catches_production_literals() {
    let words = table_words();
    let fixture = "#[cfg(test)]\nmod tests {\n    // \"claude\", \"codex\", \"cursor\"\n    const P: [&str; 3] = [\"claude\", \"codex\", \"cursor\"];\n}\n";
    assert!(list_literals(&production_source(fixture), &words).is_empty());
    let production = "const P: [&str; 3] = [\"claude\", \"codex\", \"cursor\"];\n";
    assert_eq!(
        list_literals(&production_source(production), &words).len(),
        1
    );
    let after = format!("{fixture}const P: [&str; 3] = [\"claude\", \"codex\", \"cursor\"];\n");
    assert_eq!(list_literals(&production_source(&after), &words).len(), 1);
    // Fixtures carry braces in strings, raw strings and chars; the skip must survive them.
    let tricky = "#[cfg(test)]\nmod tests {\n    fn f() { let s = \"}{ \\\" } {\"; let r = r#\"{ \"#; let c = '{'; }\n}\nconst P: [&str; 3] = [\"claude\", \"codex\", \"cursor\"];\n";
    assert_eq!(list_literals(&production_source(tricky), &words).len(), 1);
}

#[test]
fn the_scan_finds_a_copied_list() {
    let words = table_words();
    let copy = r#"const P: &[&str] = &["claude", "codex", "cursor"]; fn f(p: &str) -> bool { matches!(p, "kimi-code" | "zai" | "opencode-go") }"#;
    assert_eq!(list_literals(copy, &words).len(), 2);
    assert!(list_literals(r#"["codex", "claude"].contains(&p)"#, &words).is_empty());
}

#[test]
fn leptos_hand_lists_equal_the_table() {
    if SYNTHETIC {
        return;
    }
    let dashboard = ids(dashboard_providers());
    let charts = source("web-dashboard/leptos/src/ui/charts.rs");
    let hue = literals(
        &charts,
        "pub fn provider_hue(provider: &str) -> String {",
        "=>",
        "charts.rs",
    );
    assert_eq!(hue, dashboard, "charts.rs provider_hue");
    let icons = source("web-dashboard/leptos/src/ui/icons.rs");
    let known = literals(&icons, "let known = matches!(", ");", "icons.rs");
    assert_eq!(known, dashboard, "icons.rs ProviderMark");
}

#[test]
fn native_hand_lists_equal_the_table() {
    if SYNTHETIC {
        return;
    }
    let secret = source("crates/aac-store/src/secret.rs");
    let mut account = literals(
        &secret,
        "pub const ACCOUNT_PROVIDERS: [&str; ",
        "];",
        "secret.rs",
    );
    let mut dashboard = ids(dashboard_providers());
    account.sort();
    dashboard.sort();
    assert_eq!(
        account, dashboard,
        "aac-store ACCOUNT_PROVIDERS (any order)"
    );
    let protocol = source("native/aac-usage/src/protocol.rs");
    let kinds = literals(&protocol, "pub const KINDS: [&str; ", "];", "protocol.rs");
    assert_eq!(
        kinds,
        scanner_kinds().collect::<Vec<_>>(),
        "aac-usage KINDS"
    );
    let extra = literals_after(
        &protocol,
        "for (kind, paths) in &extra_roots {",
        "let ok = [",
        "]",
        "protocol.rs",
    );
    let expected: Vec<&str> = ANALYTICS_TOOLS
        .iter()
        .filter(|t| t.extra_roots)
        .map(|t| t.id)
        .collect();
    assert_eq!(extra, expected, "aac-usage extraRoots kinds");
}

#[test]
fn synthetic_rows_join_the_end_of_each_table() {
    let last = PROVIDERS.last().expect("rows");
    assert_eq!(last.id.as_str() == "zz-synthetic", SYNTHETIC);
    assert_eq!(provider("zz-synthetic").is_some(), SYNTHETIC);
    assert_eq!(is_dashboard_provider("zz-synthetic"), SYNTHETIC);
    assert_eq!(
        card_providers().any(|p| p.id.as_str() == "zz-synthetic"),
        SYNTHETIC
    );
    assert_eq!(scanner_kinds().any(|k| k == "zz-tool"), SYNTHETIC);
    assert_eq!(is_usage_log_tool("zz-log"), SYNTHETIC);
}
