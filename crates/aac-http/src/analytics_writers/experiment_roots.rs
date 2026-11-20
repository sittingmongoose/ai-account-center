//! The resumable v1 discovery walk for the dashboard computer's PM-Experiments.
use super::experiment_signatures as signatures;
use aac_files::read::{self, Ancestors, ReadPolicy};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

const ROOTS_FILE: &str = "experiment-usage-roots-v1.json";
const WALK_FILE: &str = "experiment-usage-walk-v1.json";
const ROOTS_BYTES: u64 = 4 * 1024 * 1024;
const WALK_BYTES: u64 = 32 * 1024 * 1024;
const TTL_MS: i64 = 6 * 3_600_000;
const MAX_DEPTH: usize = 14;
const MAX_ROOTS: usize = 2048;
const MAX_STACK: usize = 100_000;
const MAX_DIRS: usize = 1_500_000;
const SLICE_DIRS: usize = 60_000;
const SLICE_MS: u64 = 8_000;
const SNIFFS: usize = 4;
const CLAUDE_DEPTH: usize = 2;
const SCANNER_ROOTS: usize = 256;
const SKIP: [&str; 10] = [
    "node_modules",
    ".git",
    "tests",
    "test",
    "fixtures",
    "__fixtures__",
    "target",
    "dist",
    "coverage",
    "test-results",
];

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Kind {
    Claude,
    Codex,
    Muse,
    Zcode,
}

impl Kind {
    const ALL: [Self; 4] = [Self::Claude, Self::Codex, Self::Muse, Self::Zcode];

    fn key(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Muse => "muse",
            Self::Zcode => "zcode",
        }
    }
}

pub(super) fn tracks(tool: &str) -> bool {
    Kind::ALL.into_iter().any(|kind| kind.key() == tool)
}

type Roots = BTreeMap<Kind, Vec<PathBuf>>;
type Digests = BTreeMap<PathBuf, String>;

fn empty_roots() -> Roots {
    Kind::ALL
        .into_iter()
        .map(|kind| (kind, Vec::new()))
        .collect()
}

fn contained(root: &Path, base: &Path) -> bool {
    root.strip_prefix(base).is_ok_and(|tail| {
        tail.components()
            .all(|part| matches!(part, Component::Normal(_)))
    })
}

fn valid_roots(roots: &Roots, base: &Path) -> bool {
    Kind::ALL.iter().all(|kind| {
        roots.get(kind).is_some_and(|paths| {
            paths.len() <= MAX_ROOTS && paths.iter().all(|path| contained(path, base))
        })
    })
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Snapshot {
    version: u8,
    base: PathBuf,
    roots: Roots,
    digests: Digests,
    completed_at: i64,
    scanning: bool,
    truncated: bool,
    capped: BTreeSet<Kind>,
}

impl Snapshot {
    fn empty(base: PathBuf) -> Self {
        Self {
            version: 1,
            base,
            roots: empty_roots(),
            digests: Digests::new(),
            completed_at: 0,
            scanning: true,
            truncated: false,
            capped: BTreeSet::new(),
        }
    }

    pub(super) fn scanner_roots(&self) -> Map<String, Value> {
        [Kind::Claude, Kind::Codex, Kind::Muse]
            .into_iter()
            .filter_map(|kind| {
                let paths = self.paths(kind).take(SCANNER_ROOTS).collect::<Vec<_>>();
                (!paths.is_empty()).then(|| (kind.key().to_owned(), serde_json::json!(paths)))
            })
            .collect()
    }

    pub(super) fn add_zcode_paths(&self, extra: &mut Value, overflow: &mut HashSet<&'static str>) {
        let Some(roots) = extra.as_object_mut() else {
            return;
        };
        let mut discovered = self.paths(Kind::Zcode).peekable();
        if discovered.peek().is_none() {
            return;
        }
        let paths = roots
            .entry("zcode")
            .or_insert_with(|| serde_json::json!([]));
        let Some(paths) = paths.as_array_mut() else {
            return;
        };
        let mut seen: HashSet<String> = paths
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        for path in discovered {
            if !seen.insert(path.to_owned()) {
                continue;
            }
            if paths.len() == MAX_ROOTS {
                overflow.insert("zcode");
                break;
            }
            paths.push(Value::String(path.to_owned()));
        }
    }

    fn paths(&self, kind: Kind) -> impl Iterator<Item = &str> {
        self.roots[&kind]
            .iter()
            .filter_map(|path| path.to_str().filter(|text| text.chars().count() <= 1024))
    }

    pub(super) fn limited(&self, tool: &str) -> bool {
        Kind::ALL
            .into_iter()
            .find(|kind| kind.key() == tool)
            .is_some_and(|kind| {
                let limit = if kind == Kind::Zcode {
                    MAX_ROOTS
                } else {
                    SCANNER_ROOTS
                };
                self.truncated
                    || self.capped.contains(&kind)
                    || self.roots[&kind].len() > limit
                    || self.paths(kind).count() != self.roots[&kind].len()
            })
    }

    pub(super) fn discovering(&self, tool: &str) -> bool {
        self.scanning && self.completed_at == 0 && tracks(tool)
    }

    pub(super) fn next_slice(&self, now: i64) -> i64 {
        if self.scanning {
            now
        } else {
            self.completed_at.saturating_add(TTL_MS).saturating_add(1)
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Walk {
    version: u8,
    base: PathBuf,
    started_at: i64,
    stack: Vec<(PathBuf, usize)>,
    found: Roots,
    digests: Digests,
    dirs: usize,
    truncated: bool,
    capped: BTreeSet<Kind>,
}

impl Walk {
    fn new(base: &Path, now: i64) -> Self {
        Self {
            version: 1,
            base: base.to_owned(),
            started_at: now,
            stack: vec![(PathBuf::new(), 0)],
            found: empty_roots(),
            digests: Digests::new(),
            dirs: 0,
            truncated: false,
            capped: BTreeSet::new(),
        }
    }

    fn valid(&self, base: &Path) -> bool {
        self.version == 1
            && self.base == base
            && self.dirs <= MAX_DIRS
            && valid_roots(&self.found, base)
            && self.stack.len() <= MAX_STACK
            && self.stack.iter().all(|(path, depth)| {
                *depth <= MAX_DEPTH
                    && path.components().count() == *depth
                    && path
                        .components()
                        .all(|part| matches!(part, Component::Normal(_)))
            })
    }

    fn accept(&mut self, kind: Kind, root: &Path) -> bool {
        if !contained(root, &self.base) {
            return false;
        }
        let list = self.found.entry(kind).or_default();
        if list.iter().any(|existing| {
            existing == root
                || (kind == Kind::Claude
                    && root
                        .strip_prefix(existing)
                        .is_ok_and(|tail| tail.components().count() <= CLAUDE_DEPTH))
        }) {
            return true;
        }
        if list.len() == MAX_ROOTS {
            self.capped.insert(kind);
            return false;
        }
        if kind == Kind::Zcode {
            let Some(digest) = signatures::zcode_digest(&self.base, root) else {
                return false;
            };
            self.digests.insert(root.to_owned(), digest);
        }
        list.push(root.to_owned());
        if matches!(kind, Kind::Codex | Kind::Muse)
            && let Ok(tail) = root.strip_prefix(&self.base)
        {
            self.stack.retain(|(child, _)| !child.starts_with(tail));
        }
        true
    }

    fn exclude(&mut self, parent: &Path) {
        self.stack.retain(|(entry, _)| !entry.starts_with(parent));
        let folder = self.base.join(parent);
        for roots in self.found.values_mut() {
            roots.retain(|root| !root.starts_with(&folder));
        }
        self.digests.retain(|root, _| !root.starts_with(&folder));
    }

    fn visit(&mut self, relative: &Path, depth: usize, deadline: Instant) -> bool {
        let dir = self.base.join(relative);
        if !relative.as_os_str().is_empty()
            && !dir
                .symlink_metadata()
                .is_ok_and(|meta| meta.file_type().is_dir())
        {
            return true;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            return true;
        };
        self.dirs += 1;
        let mut files = Vec::new();
        let mut children = Vec::new();
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if kind.is_file() {
                files.push(name);
            } else if kind.is_dir() && !SKIP.contains(&name.as_str()) {
                children.push(name);
            }
        }
        if !relative.as_os_str().is_empty() {
            if files.iter().any(|name| name == ".aac-synthetic")
                || (files.iter().any(|name| name == "MANIFEST.json")
                    && signatures::generator_manifest(&self.base, &dir))
            {
                if let Some(parent) = relative
                    .parent()
                    .filter(|path| !path.as_os_str().is_empty())
                {
                    self.exclude(parent);
                }
                return true;
            }
            if files.iter().any(|name| name == "SANDBOX.md") {
                return true;
            }
        }
        files.sort();
        let mut covered = false;
        let mut seen = BTreeSet::new();
        let log = |name: &str| name.ends_with(".jsonl") || is_compressed_rollout(name);
        for name in files.iter().filter(|name| log(name)).take(SNIFFS) {
            let compressed = is_compressed_rollout(name);
            let file = dir.join(name);
            let kind = signatures::usage_log(&self.base, &file, name, compressed, deadline);
            if compressed && kind.is_none() && Instant::now() >= deadline {
                self.stack.push((relative.to_owned(), depth));
                return false;
            }
            if let Some(kind) = kind.filter(|kind| seen.insert(*kind))
                && let Some(root) = signatures::root_for(kind, &file)
                && self.accept(kind, root)
                && kind != Kind::Claude
                && dir.starts_with(root)
            {
                covered = true;
            }
        }
        for name in files
            .iter()
            .filter(|name| {
                [".sqlite", ".sqlite3", ".db"]
                    .iter()
                    .any(|ext| name.ends_with(ext))
            })
            .take(SNIFFS)
        {
            let file = dir.join(name);
            if signatures::is_zcode(&self.base, &file) == Some(true) {
                self.accept(Kind::Zcode, &file);
            }
        }
        if depth == MAX_DEPTH {
            self.truncated |= !children.is_empty();
        } else if !covered {
            children.sort();
            for name in children.into_iter().rev() {
                if self.stack.len() == MAX_STACK {
                    self.truncated = true;
                    break;
                }
                self.stack.push((relative.join(name), depth + 1));
            }
        }
        true
    }
}

fn is_compressed_rollout(name: &str) -> bool {
    name.starts_with("rollout-") && name.ends_with(".jsonl.zst")
}

fn read_cached<T: DeserializeOwned>(cache: &Path, name: &str, cap: u64) -> Option<T> {
    let policy = ReadPolicy::capped(cap).ancestors(Ancestors::NoLinkBelow(cache.parent()?));
    match read::read(&cache.join(name), &policy) {
        read::Read::Bytes(bytes, _) => serde_json::from_slice(&bytes).ok(),
        _ => None,
    }
}

fn save(cache: &Path, name: &str, cap: u64, value: &impl Serialize) -> bool {
    let Ok(body) = serde_json::to_string(value) else {
        return false;
    };
    if body.len() as u64 > cap {
        return false;
    }
    if let Err(error) = fs::create_dir(cache)
        && error.kind() != std::io::ErrorKind::AlreadyExists
    {
        return false;
    }
    cache
        .symlink_metadata()
        .is_ok_and(|meta| meta.file_type().is_dir())
        && aac_files::share::write_atomically(cache, name, &body, 0o600).is_ok()
}

fn publish(mut old: Snapshot, walk: Walk, now: i64) -> Snapshot {
    let scanning = !walk.stack.is_empty();
    let merge = scanning || walk.truncated || !walk.capped.is_empty();
    let mut roots = walk.found;
    if merge {
        for (kind, paths) in &mut roots {
            let mut seen = HashSet::new();
            let mut all = old.roots.remove(kind).unwrap_or_default();
            all.append(paths);
            all.retain(|path| seen.insert(path.clone()));
            if all.len() > MAX_ROOTS {
                old.capped.insert(*kind);
                all.truncate(MAX_ROOTS);
            }
            all.sort();
            *paths = all;
        }
        old.digests.extend(walk.digests);
        old.capped.extend(walk.capped);
    } else {
        old.digests = walk.digests;
        old.capped = walk.capped;
    }
    let mut seen = HashSet::new();
    if let Some(zcode) = roots.get_mut(&Kind::Zcode) {
        zcode.sort();
        zcode.retain(|file| {
            let identity = old
                .digests
                .get(file)
                .cloned()
                .unwrap_or_else(|| file.to_string_lossy().into_owned());
            seen.insert(identity)
        });
    }
    Snapshot {
        roots,
        scanning,
        completed_at: if scanning { old.completed_at } else { now },
        truncated: walk.truncated || (scanning && old.truncated),
        ..old
    }
}

fn slice(
    cache: &Path,
    home: &Path,
    now: i64,
    max_slice_dirs: usize,
    deadline: Instant,
) -> Snapshot {
    let base = home.join("PM-Experiments");
    let old = read_cached::<Snapshot>(cache, ROOTS_FILE, ROOTS_BYTES)
        .filter(|file| file.version == 1 && file.base == base && valid_roots(&file.roots, &base))
        .unwrap_or_else(|| Snapshot::empty(base.clone()));
    let checkpoint =
        read_cached::<Walk>(cache, WALK_FILE, WALK_BYTES).filter(|walk| walk.valid(&base));
    if checkpoint.is_none()
        && old.completed_at > 0
        && now.saturating_sub(old.completed_at) <= TTL_MS
    {
        return old;
    }
    if checkpoint.is_none() && !base.is_dir() {
        let result = Snapshot {
            scanning: false,
            completed_at: now,
            ..Snapshot::empty(base)
        };
        save(cache, ROOTS_FILE, ROOTS_BYTES, &result);
        return result;
    }
    let mut walk = checkpoint.unwrap_or_else(|| Walk::new(&base, now));
    let start = walk.dirs;
    while walk.dirs - start < max_slice_dirs && Instant::now() < deadline {
        if walk.dirs >= MAX_DIRS {
            walk.truncated |= !walk.stack.is_empty();
            walk.stack.clear();
        }
        let Some((relative, depth)) = walk.stack.pop() else {
            break;
        };
        if !walk.visit(&relative, depth, deadline) {
            break;
        }
    }
    if !walk.stack.is_empty() {
        save(cache, WALK_FILE, WALK_BYTES, &walk);
    }
    let result = publish(old, walk, now);
    if save(cache, ROOTS_FILE, ROOTS_BYTES, &result) && !result.scanning {
        let _ = fs::remove_file(cache.join(WALK_FILE));
    }
    result
}

pub(super) fn run_slice(ccs: &Path, home: &Path, now: i64) -> Snapshot {
    slice(
        &ccs.join("cache"),
        home,
        now,
        SLICE_DIRS,
        Instant::now() + Duration::from_millis(SLICE_MS),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analytics_experiment_walk_resumes_and_excludes_sandbox_and_test_trees() {
        let home = tempfile::tempdir().expect("fixture home");
        let ccs = tempfile::tempdir().expect("fixture CCS_HOME");
        let cache = ccs.path().join("cache");
        let base = home.path().join("PM-Experiments");
        let claude = r#"{"type":"assistant","sessionId":"fixture","version":"1","cwd":"/fixture"}"#;
        for folder in [
            "claude/subagents",
            "generator/a-fake",
            "generator/b-marked",
            "readme",
            "tests",
        ] {
            let dir = base.join(folder);
            fs::create_dir_all(&dir).expect("fixture directory");
            fs::write(dir.join("capture.jsonl"), claude).expect("fixture Claude log");
        }
        fs::write(base.join("claude/capture.jsonl"), claude).expect("parent Claude log");
        fs::write(
            base.join("generator/b-marked/MANIFEST.json"),
            r#"{"trees":{}}"#,
        )
        .expect("generator marker");
        fs::write(base.join("readme/SANDBOX.md"), "").expect("sandbox marker");
        let muse = base.join("muse/sessions/2026/11/15/fixture");
        fs::create_dir_all(&muse).expect("Muse fixture");
        fs::write(
            muse.join("session.jsonl"),
            r#"{"record_type":"usage","recorded_at":1,"payload":{}}"#,
        )
        .expect("fixture Muse log");
        let zcode = base.join("zcode.db");
        rusqlite::Connection::open(&zcode).expect("fixture DB").execute_batch(
            "CREATE TABLE model_usage (model_id, provider_id, started_at, input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens);",
        ).expect("fixture schema");
        fs::copy(&zcode, base.join("zcode-copy.db")).expect("copy DB");

        let deadline = || Instant::now() + Duration::from_secs(3);
        let first = slice(&cache, home.path(), 1, 2, deadline());
        assert!(first.scanning);
        assert!(
            first.discovering("claude"),
            "the first round is still discovering"
        );
        assert_eq!(first.roots[&Kind::Claude], [base.join("claude")]);
        assert!(cache.join(WALK_FILE).is_file());
        let finished = slice(&cache, home.path(), 2, 100, deadline());
        assert!(!finished.scanning);
        assert_eq!(finished.roots[&Kind::Claude], [base.join("claude")]);
        assert_eq!(finished.roots[&Kind::Muse], [base.join("muse/sessions")]);
        assert_eq!(finished.roots[&Kind::Zcode].len(), 1);
        let mut extra = serde_json::json!({});
        finished.add_zcode_paths(&mut extra, &mut HashSet::new());
        assert_eq!(extra["zcode"].as_array().expect("zcode DBs").len(), 1);
        assert!(!cache.join(WALK_FILE).exists());
        let reused = slice(&cache, home.path(), 3, 1, deadline());
        assert_eq!(reused.completed_at, 2);
        assert_eq!(reused.roots, finished.roots);
        let renewed = slice(&cache, home.path(), 3 + TTL_MS, 1, deadline());
        assert!(renewed.scanning);
        assert!(
            !renewed.discovering("claude"),
            "a renewal keeps the last read's coverage state"
        );
        assert_eq!(renewed.roots, finished.roots);
    }
}
