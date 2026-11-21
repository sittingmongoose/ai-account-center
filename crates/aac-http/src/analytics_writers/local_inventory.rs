//! The same Claude roots and walk as the scanner, without opening transcripts.
use aac_usage::{collector::Collector, discover, fsx, read::claude, util};
use serde_json::Map;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub(super) fn original_names(
    home: &Path,
    env: &discover::Env,
    extra: &[PathBuf],
) -> BTreeSet<String> {
    let plan = discover::claude_roots(home, env, extra, &discover::t3_instances(home));
    let candidates: Vec<_> = plan
        .defaults
        .into_iter()
        .chain(plan.others)
        .map(|(path, _)| path)
        .collect();
    let mut walk = Collector::new(0, 2.0, Map::new());
    let dirs = fsx::dedup_dirs(&walk.reachable_dirs("claude", &candidates));
    let mut names = BTreeSet::new();
    for dir in dirs {
        for file in fsx::walk_files(
            "claude",
            &dir,
            &|name| name.ends_with(".jsonl"),
            &mut walk,
            None,
        ) {
            if !claude::is_mirror(&file)
                && let Some(name) = file.file_name().and_then(|name| name.to_str())
            {
                names.insert(util::sha256_hex(name.as_bytes()));
            }
            if names.len() >= 20_000 || walk.expired() {
                return names;
            }
        }
    }
    names
}
