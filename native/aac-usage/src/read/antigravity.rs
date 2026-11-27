//! Antigravity conversation databases (1.4 and 1.5): one SQLite per conversation; usage is protobuf
//! in `gen_metadata.data` and `steps.metadata`, apart from the conversation text. Only model ids and
//! names, token counts, record ids and timestamps are taken. A record counts once across databases
//! (T3's alias merge). The Python helper's port of T3 Code's `antigravityUsageReader.ts`.

use crate::collector::Collector;
use crate::discover::{AGY_DIRS, Env};
use crate::fsx::{self, is_link, link_inside, same_or_inside};
use crate::util::{self, abspath, path_text, realpath};
use rusqlite::{Connection, OpenFlags, types::ValueRef as SqlRef};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const AGY_MAX_DBS: usize = 2000;
const AGY_MAX_CANDIDATES: usize = 200_000;
const AGY_MAX_DEPTH: usize = 32;
const AGY_MAX_DB_BYTES: u64 = 1024 * 1024 * 1024;
const AGY_SAFE_INTEGER: u64 = (1 << 53) - 1;
const AGY_UNKNOWN: &str = "antigravity-unknown";
/// String.prototype.trim()'s whitespace, so a name or id reads as T3 reads it.
const AGY_TRIM: &[char] = &[
    '\t', '\n', '\u{0b}', '\u{0c}', '\r', ' ', '\u{a0}', '\u{1680}', '\u{2000}', '\u{2001}', '\u{2002}', '\u{2003}',
    '\u{2004}', '\u{2005}', '\u{2006}', '\u{2007}', '\u{2008}', '\u{2009}', '\u{200a}', '\u{2028}', '\u{2029}',
    '\u{202f}', '\u{205f}', '\u{3000}', '\u{feff}',
];

fn model_by_id(id: u64) -> Option<&'static str> {
    Some(match id {
        246 => "gemini-2.5-pro",
        312 => "gemini-2.5-flash",
        313 | 329 => "gemini-2.5-flash-thinking",
        330 => "gemini-2.5-flash-lite",
        281 | 282 => "claude-sonnet-4",
        290 | 291 => "claude-opus-4",
        333 | 334 => "claude-sonnet-4-5",
        340 | 341 => "claude-haiku-4-5",
        1026 => "claude-opus-4-6",
        1035 => "claude-sonnet-4-6",
        1016 | 1036 | 1037 => "gemini-3.1-pro",
        1018 | 1084 | 1047 => "gemini-3-flash-preview",
        _ => return None,
    })
}

#[derive(Debug, PartialEq)]
enum AgyError {
    /// T3 refuses the database as a whole: it contributes nothing.
    Invalid,
    /// A valid SQLite database without Antigravity's usage tables: skipped quietly.
    NotConversation,
    /// A writer opened a cleanly closed database during an immutable read: read it next scan.
    Retry,
    /// The deadline passed in the middle of one database.
    Deadline,
    Sql,
}

impl From<rusqlite::Error> for AgyError {
    fn from(_: rusqlite::Error) -> AgyError {
        AgyError::Sql
    }
}

#[derive(Clone, Copy)]
enum Field<'a> {
    Int(u64),
    /// A varint past 2^53 - 1: T3 holds it as a bigint, which no read takes as a number.
    Big,
    Bytes(&'a [u8]),
}

fn varint(data: &[u8], mut offset: usize) -> Result<(Field<'static>, usize), AgyError> {
    let mut value: u64 = 0;
    let mut shift = 0u32;
    while shift < 70 {
        let Some(&byte) = data.get(offset) else {
            return Err(AgyError::Invalid);
        };
        offset += 1;
        if shift == 63 && byte > 1 {
            return Err(AgyError::Invalid);
        }
        value |= ((byte & 127) as u64) << shift;
        if byte < 128 {
            return Ok((
                if value > AGY_SAFE_INTEGER {
                    Field::Big
                } else {
                    Field::Int(value)
                },
                offset,
            ));
        }
        shift += 7;
    }
    Err(AgyError::Invalid)
}

type Fields<'a> = BTreeMap<u64, Vec<Field<'a>>>;

/// Field number to values of one message; fixed32/64 fields are skipped, as in T3.
fn fields(data: &[u8]) -> Result<Fields<'_>, AgyError> {
    let mut result: Fields = BTreeMap::new();
    let mut offset = 0;
    while offset < data.len() {
        let (tag, next) = varint(data, offset)?;
        offset = next;
        let Field::Int(tag) = tag else {
            return Err(AgyError::Invalid);
        };
        if tag / 8 == 0 {
            return Err(AgyError::Invalid);
        }
        let (number, wire) = (tag / 8, tag % 8);
        let value = match wire {
            0 => {
                let (value, next) = varint(data, offset)?;
                offset = next;
                value
            }
            1 | 2 | 5 => {
                let length = if wire == 2 {
                    let (length, next) = varint(data, offset)?;
                    offset = next;
                    match length {
                        Field::Int(length) => length as usize,
                        _ => return Err(AgyError::Invalid),
                    }
                } else if wire == 1 {
                    8
                } else {
                    4
                };
                if length > data.len() - offset {
                    return Err(AgyError::Invalid);
                }
                let slice = &data[offset..offset + length];
                offset += length;
                if wire != 2 {
                    continue;
                }
                Field::Bytes(slice)
            }
            _ => return Err(AgyError::Invalid),
        };
        result.entry(number).or_default().push(value);
    }
    Ok(result)
}

fn first<'a>(f: &Fields<'a>, key: u64) -> Option<Field<'a>> {
    f.get(&key).and_then(|v| v.first().copied())
}

fn number(f: &Fields, key: u64) -> u64 {
    match first(f, key) {
        Some(Field::Int(v)) => v,
        _ => 0,
    }
}

fn bytes<'a>(f: &Fields<'a>, key: u64) -> Option<&'a [u8]> {
    match first(f, key) {
        Some(Field::Bytes(b)) => Some(b),
        _ => None,
    }
}

fn nested<'a>(f: &Fields<'a>, key: u64) -> Result<Fields<'a>, AgyError> {
    match bytes(f, key) {
        None => Ok(BTreeMap::new()),
        Some(data) => fields(data),
    }
}

fn text(f: &Fields, key: u64) -> Result<String, AgyError> {
    text_of(bytes(f, key))
}

fn text_of(data: Option<&[u8]>) -> Result<String, AgyError> {
    match data {
        None => Ok(String::new()),
        Some(data) => {
            let text = std::str::from_utf8(data).map_err(|_| AgyError::Invalid)?;
            Ok(text.trim_matches(AGY_TRIM).to_string())
        }
    }
}

fn timestamp(f: &Fields) -> Option<f64> {
    let seconds = number(f, 1);
    if seconds > 0 {
        Some((seconds * 1000 + number(f, 2) / 1_000_000) as f64)
    } else {
        None
    }
}

/// `[TRIM]*\([^)]*\)[TRIM]*\Z` removed once (the last parenthesized group and its spacing).
fn strip_suffix(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut end = chars.len();
    while end > 0 && AGY_TRIM.contains(&chars[end - 1]) {
        end -= 1;
    }
    if end == 0 || chars[end - 1] != ')' {
        return text.to_string();
    }
    let Some(open) = chars[..end - 1].iter().rposition(|c| *c == '(') else {
        return text.to_string();
    };
    if chars[open + 1..end - 1].contains(&')') {
        return text.to_string();
    }
    let mut start = open;
    while start > 0 && AGY_TRIM.contains(&chars[start - 1]) {
        start -= 1;
    }
    chars[..start].iter().collect()
}

fn model_name(name: &str, model_id: u64) -> String {
    if !name.is_empty() {
        let normalized = strip_suffix(&name.to_lowercase()).replace(' ', "-");
        if let Some(rest) = normalized.strip_prefix("claude-") {
            // ^claude-(4(?:\.[0-9]+)?)-(sonnet|opus|haiku) -> claude-\2-\1, then dots to dashes.
            let mut version_end = 0;
            if rest.starts_with('4') {
                version_end = 1;
                let bytes = rest.as_bytes();
                if bytes.get(1) == Some(&b'.') && bytes.get(2).is_some_and(u8::is_ascii_digit) {
                    version_end = 2;
                    while bytes.get(version_end).is_some_and(u8::is_ascii_digit) {
                        version_end += 1;
                    }
                }
            }
            let mut out = normalized.clone();
            if version_end > 0 && rest.as_bytes().get(version_end) == Some(&b'-') {
                let after = &rest[version_end + 1..];
                for family in ["sonnet", "opus", "haiku"] {
                    if let Some(tail) = after.strip_prefix(family) {
                        out = format!("claude-{family}-{}{tail}", &rest[..version_end]);
                        break;
                    }
                }
            }
            return out.replace('.', "-");
        }
        return normalized;
    }
    if let Some(name) = model_by_id(model_id) {
        return name.to_string();
    }
    if model_id > 0 {
        format!("antigravity-model-{model_id}")
    } else {
        String::new()
    }
}

/// The fields of one usage record the candidates read: numbers 1-5, 9, 10 and ids 11, 12, 7, owned,
/// so the row's blob is dropped as soon as it is parsed (R4).
#[derive(Clone, Debug, PartialEq)]
struct Usage {
    numbers: [u64; 7],
    ids: [Option<Box<[u8]>>; 3],
}

const USAGE_NUMBERS: [u64; 7] = [1, 2, 3, 4, 5, 9, 10];
const USAGE_IDS: [u64; 3] = [11, 12, 7];

impl Usage {
    fn of(f: &Fields) -> Usage {
        Usage {
            numbers: USAGE_NUMBERS.map(|key| number(f, key)),
            ids: USAGE_IDS.map(|key| bytes(f, key).map(Box::from)),
        }
    }

    fn number(&self, key: u64) -> u64 {
        USAGE_NUMBERS
            .iter()
            .position(|k| *k == key)
            .map_or(0, |i| self.numbers[i])
    }

    fn text(&self, key: u64) -> Result<String, AgyError> {
        let id = USAGE_IDS
            .iter()
            .position(|k| *k == key)
            .and_then(|i| self.ids[i].as_deref());
        text_of(id)
    }
}

struct Meta {
    model: String,
    model_id: u64,
    named: bool,
    stamp: Option<f64>,
    usages: Vec<Usage>,
}

fn metadata(blob: &[u8], step: bool) -> Result<Meta, AgyError> {
    let root = fields(blob)?;
    if !step && bytes(&root, 1).is_none() {
        return Err(AgyError::Invalid);
    }
    let data = if step { root } else { nested(&root, 1)? };
    let model = if step { nested(&data, 24)? } else { data.clone() };
    let mut usages = Vec::new();
    if let Some(usage) = bytes(&data, if step { 9 } else { 4 }) {
        usages.push(Usage::of(&fields(usage)?));
    }
    for retry in data
        .get(&(if step { 28 } else { 17 }))
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let Field::Bytes(retry) = retry else {
            return Err(AgyError::Invalid);
        };
        let retry = fields(retry)?;
        if let Some(retry_usage) = bytes(&retry, 2) {
            usages.push(Usage::of(&fields(retry_usage)?));
        }
    }
    let primary = text(&model, if step { 12 } else { 19 })?;
    let name = if primary.is_empty() {
        text(&model, if step { 8 } else { 21 })?
    } else {
        primary
    };
    let model_id = number(&model, if step { 1 } else { 3 });
    let stamp = if step {
        timestamp(&nested(&data, 8)?).or(timestamp(&nested(&data, 1)?))
    } else {
        timestamp(&nested(&nested(&data, 9)?, 4)?)
    };
    Ok(Meta {
        model: model_name(&name, model_id),
        model_id,
        named: !name.is_empty(),
        stamp,
        usages,
    })
}

fn uri(path: &Path, immutable: bool) -> String {
    super::sqlite::uri(path, immutable)
}

/// Size, mtime and file identity: a writer opening a database changes one of them.
type DbIdentity = (u64, i128, Option<(u64, u64)>);

fn identity(path: &Path) -> Option<DbIdentity> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.len(), util::mtime_ns(&meta), fsx::identity(path)))
}

fn wal_exists(path: &Path) -> bool {
    // An inaccessible WAL is not proof of a cleanly closed database: use the live read path.
    !matches!(fsx::metadata(Path::new(&format!("{}-wal", path_text(path)))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound)
}

/// A WAL-mode database (header bytes 18-19 are 2) closed cleanly (no -wal): an immutable open reads
/// the same rows and creates no -wal or -shm file.
fn closed_wal(path: &Path) -> bool {
    if wal_exists(path) {
        return false;
    }
    // The 20-byte header only, never the database (FIX-ROUND-BIG 10).
    let mut data = [0u8; 20];
    let Ok(mut file) = fsx::open_regular(path) else {
        return false;
    };
    if std::io::Read::read_exact(&mut file, &mut data).is_err() {
        return false;
    }
    &data[..16] == b"SQLite format 3\x00" && data[18..20] == [2, 2]
}

#[cfg_attr(test, derive(Debug, PartialEq))]
struct Candidate {
    keys: Vec<String>,
    stamp: f64,
    quality: u8,
    tokens: [u64; 4],
    t3_model: String,
    static_model: String,
    ids: (u64, u64),
}

/// Usage candidates and the (model id, name) pairs the database names.
type DbRead = (Vec<Candidate>, Vec<(u64, String)>);

/// The first failure met while parsing rows, in read order. The reads go on (every SQL error
/// still surfaces first, and a moved database still answers Retry), as when every row was read
/// before any was parsed.
type Parsed = Option<AgyError>;

/// One table's `(idx, blob)` rows, each parsed as SQLite steps to it and its blob dropped (R4:
/// the reader used to hold every blob of the database before parsing the first).
fn read_entries(
    connection: &Connection,
    sql: &str,
    step: bool,
    deadline: Instant,
    failed: &mut Parsed,
) -> Result<Vec<(f64, Meta)>, AgyError> {
    let mut found = Vec::new();
    let mut statement = connection.prepare(sql)?;
    let mut rows = statement.query([])?;
    let mut index = 0usize;
    while let Some(row) = rows.next()? {
        let (idx, data) = (row.get_ref(0)?, row.get_ref(1)?);
        if failed.is_some() {
            continue;
        }
        let entry = if index % 256 == 255 && Instant::now() >= deadline {
            Err(AgyError::Deadline)
        } else {
            entry(idx, data, step)
        };
        index += 1;
        match entry {
            Ok(entry) => found.push(entry),
            Err(error) => *failed = Some(error),
        }
    }
    Ok(found)
}

fn entry(idx: SqlRef<'_>, data: SqlRef<'_>, step: bool) -> Result<(f64, Meta), AgyError> {
    let idx = match idx {
        SqlRef::Integer(v) => v as f64,
        SqlRef::Real(v) => v,
        _ => return Err(AgyError::Invalid),
    };
    let SqlRef::Blob(data) = data else {
        return Err(AgyError::Invalid);
    };
    Ok((idx, metadata(data, step)?))
}

/// T3's `readDatabase`: one short read transaction with a 100 ms busy wait; never writes.
fn read_db(path: &Path, fallback_ms: f64, deadline: Instant) -> Result<DbRead, AgyError> {
    let closed = closed_wal(path);
    let before = if closed { identity(path) } else { None };
    let mut failed: Parsed = None;
    let (generations, trajectory, steps) = {
        let connection = Connection::open_with_flags(
            uri(path, closed),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(Duration::from_millis(100))?;
        connection.progress_handler(20_000, Some(move || Instant::now() >= deadline));
        connection.execute_batch("BEGIN")?;
        let tables: HashSet<String> = {
            let mut statement = connection.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<_, _>>()?
        };
        if !tables.contains("gen_metadata") && !tables.contains("steps") {
            return Err(AgyError::NotConversation);
        }
        let generations = if tables.contains("gen_metadata") {
            read_entries(
                &connection,
                "SELECT idx, data FROM gen_metadata ORDER BY idx",
                false,
                deadline,
                &mut failed,
            )?
        } else {
            Vec::new()
        };
        // The first row with a timestamp dates the conversation; a missing blob before it refuses it.
        let mut trajectory = None;
        if tables.contains("trajectory_metadata_blob") {
            let mut statement = connection.prepare("SELECT data FROM trajectory_metadata_blob")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let data = row.get_ref(0)?;
                if failed.is_some() || trajectory.is_some() {
                    continue;
                }
                let SqlRef::Blob(data) = data else {
                    failed = Some(AgyError::Invalid);
                    continue;
                };
                match fields(data).and_then(|f| nested(&f, 2).map(|n| timestamp(&n))) {
                    Ok(stamp) => trajectory = stamp,
                    Err(error) => failed = Some(error),
                }
            }
        }
        let steps = if tables.contains("steps") {
            read_entries(
                &connection,
                "SELECT idx, metadata FROM steps WHERE metadata IS NOT NULL ORDER BY idx",
                true,
                deadline,
                &mut failed,
            )?
        } else {
            Vec::new()
        };
        (generations, trajectory, steps)
    };
    if closed && (wal_exists(path) || identity(path) != before) {
        return Err(AgyError::Retry);
    }
    if let Some(error) = failed {
        return Err(error);
    }
    let mut names = Vec::new();
    for (_, meta) in steps.iter().chain(generations.iter()) {
        if meta.named && meta.model_id > 0 {
            names.push((meta.model_id, meta.model.clone()));
        }
    }
    let positional: HashMap<u64, String> = generations
        .iter()
        .map(|(idx, m)| (idx.to_bits(), m.model.clone()))
        .collect();
    let mut candidates = Vec::new();
    for (step, listed) in [(true, &steps), (false, &generations)] {
        for (idx, meta) in listed {
            for usage in &meta.usages {
                let output = usage.number(3).max(usage.number(9) + usage.number(10));
                let tokens = [usage.number(2), usage.number(5), usage.number(4), output];
                if tokens.iter().sum::<u64>() == 0 {
                    continue;
                }
                let mut keys = Vec::new();
                for key in [11u64, 12, 7] {
                    let value = usage.text(key)?;
                    if !value.is_empty() {
                        keys.push(format!("antigravity:{key}:{value}"));
                    }
                }
                let usage_id = usage.number(1);
                let positional_model = if step {
                    positional.get(&idx.to_bits()).cloned().unwrap_or_default()
                } else {
                    String::new()
                };
                let mut t3_model = model_by_id(usage_id).map(str::to_string).unwrap_or_default();
                for fallback in [
                    meta.model.clone(),
                    positional_model,
                    model_name("", usage_id),
                    AGY_UNKNOWN.to_string(),
                ] {
                    if !t3_model.is_empty() {
                        break;
                    }
                    t3_model = fallback;
                }
                let static_model = model_by_id(usage_id)
                    .map(str::to_string)
                    .unwrap_or_else(|| if meta.named { meta.model.clone() } else { String::new() });
                candidates.push(Candidate {
                    keys,
                    stamp: meta.stamp.or(trajectory).unwrap_or(fallback_ms),
                    quality: if meta.stamp.is_some() {
                        2
                    } else if trajectory.is_some() {
                        1
                    } else {
                        0
                    },
                    tokens,
                    t3_model,
                    static_model,
                    ids: (usage_id, meta.model_id),
                });
            }
        }
    }
    Ok((candidates, names))
}

fn pick_model(candidate: &Candidate, learned: &HashMap<u64, String>) -> String {
    if !candidate.static_model.is_empty() {
        return candidate.static_model.clone();
    }
    let (usage_id, entry_id) = candidate.ids;
    if usage_id > 0
        && let Some(model) = learned.get(&usage_id)
    {
        return model.clone();
    }
    if let Some(model) = model_by_id(entry_id) {
        return model.to_string();
    }
    if entry_id > 0
        && let Some(model) = learned.get(&entry_id)
    {
        return model.clone();
    }
    candidate.t3_model.clone()
}

/// A network mount, a UNC path or ~/PM-Experiments: never entered.
fn excluded(path: &Path, home: &Path) -> bool {
    let text = path_text(&abspath(path));
    if text.starts_with("\\\\") || text.starts_with("//") {
        return true;
    }
    let roots: Vec<PathBuf> = ["/mnt", "/media", "/Volumes", "/net"]
        .iter()
        .map(PathBuf::from)
        .chain([home.join("PM-Experiments")])
        .collect();
    same_or_inside(&abspath(path), &roots)
}

/// A root's real path, resolved one component at a time: each link's target is checked as written
/// before it is followed, so a link into a network mount or ~/PM-Experiments is never touched.
fn real_dir(path: &Path, home: &Path) -> Option<PathBuf> {
    let absolute = abspath(path);
    let mut components: Vec<String> = Vec::new();
    let mut current = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::Prefix(_) | std::path::Component::RootDir => current.push(component.as_os_str()),
            other => components.push(other.as_os_str().to_string_lossy().into_owned()),
        }
    }
    let mut pieces: std::collections::VecDeque<String> = components.into();
    let mut hops = 0;
    while let Some(piece) = pieces.pop_front() {
        if piece == "." {
            continue;
        }
        if piece == ".." {
            current.pop();
            continue;
        }
        let candidate = current.join(&piece);
        if excluded(&candidate, home) {
            return None;
        }
        if !is_link(&candidate) {
            current = candidate;
            continue;
        }
        hops += 1;
        let written = path_text(&fs::read_link(&candidate).ok()?);
        if hops > 40 || written.starts_with("\\\\?\\UNC\\") {
            return None;
        }
        let written = written.strip_prefix("\\\\?\\").unwrap_or(&written).to_string();
        let target = util::normpath(&current.join(written));
        if excluded(&target, home) {
            return None;
        }
        let mut rebuilt = PathBuf::new();
        let mut rest: Vec<String> = Vec::new();
        for component in target.components() {
            match component {
                std::path::Component::Prefix(_) | std::path::Component::RootDir => rebuilt.push(component.as_os_str()),
                other => rest.push(other.as_os_str().to_string_lossy().into_owned()),
            }
        }
        for piece in rest.into_iter().rev() {
            pieces.push_front(piece);
        }
        current = rebuilt;
    }
    if excluded(&current, home) { None } else { Some(current) }
}

/// The Antigravity roots, each with its origin: `$ANTIGRAVITY_DATA_DIR` (comma-separated), else the
/// ~/.gemini stores and ~/.config/antigravity; then every T3 Code instance's
/// `<stateDir>/providers/antigravity/<sha256(id)>/antigravity-acp`. A root with a `conversations`
/// folder is read there only. Each real folder once. Returns (dirs, present).
pub fn roots(home: &Path, env: &Env, collector: &mut Collector) -> (Vec<PathBuf>, bool) {
    let mut listed: Vec<(PathBuf, &str)> = env
        .get("ANTIGRAVITY_DATA_DIR")
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
        .into_iter()
        .map(|p| {
            if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
                home.join(rest)
            } else {
                PathBuf::from(p)
            }
        })
        .filter(|p| fsx::absolute_text(&path_text(p)))
        .map(|p| (p, "env"))
        .collect();
    if listed.is_empty() {
        listed = AGY_DIRS
            .iter()
            .map(|name| (home.join(".gemini").join(name), "default"))
            .collect();
        listed.push((home.join(".config").join("antigravity"), "default"));
    }
    let instances = home.join(".t3").join("userdata").join("providers").join("antigravity");
    let (instance_dirs, unreadable) = fsx::child_dirs_checked(&instances);
    if unreadable {
        collector.unreadable_folder("antigravity", &instances);
    }
    listed.extend(
        instance_dirs
            .into_iter()
            .map(|p| (p.join("antigravity-acp"), "t3_instance")),
    );
    for (path, origin) in &listed {
        collector.note_root("antigravity", path, origin);
    }
    // Each root is probed first (FIX-ROUND-BIG 9): one that is there but cannot be read, or does
    // not answer, is named on its root; the rest are read as before.
    let paths: Vec<PathBuf> = listed.iter().map(|(p, _)| p.clone()).collect();
    let answered: HashSet<PathBuf> = collector.gate_dirs("antigravity", &paths).into_iter().collect();
    let mut dirs = Vec::new();
    let mut present = false;
    for (root, _) in listed {
        if !answered.contains(&root) {
            continue;
        }
        let Some(real) = real_dir(&root, home) else {
            continue;
        };
        if !fsx::discovery_metadata(collector, "antigravity", &real).is_some_and(|meta| meta.is_dir()) {
            continue;
        }
        present = true;
        let nested = real_dir(&real.join("conversations"), home)
            .filter(|n| fsx::discovery_metadata(collector, "antigravity", n).is_some_and(|meta| meta.is_dir()));
        let chosen = nested.unwrap_or(real);
        if !dirs.contains(&chosen) {
            dirs.push(chosen);
        }
    }
    (dirs, present)
}

struct WalkState {
    dirs: HashSet<(u64, u64)>,
    seen: HashSet<PathBuf>,
    files: Vec<(PathBuf, String)>,
    unreadable: usize,
}

fn walk(collector: &mut Collector, directory: &Path, start: &Path, depth: usize, state: &mut WalkState) -> bool {
    // Python: a folder gone since the listing is skipped; any other stat or list failure counts.
    let Some(id) = fsx::identity(directory) else {
        if !matches!(fsx::metadata(directory), Err(error) if error.kind() == std::io::ErrorKind::NotFound) {
            state.unreadable += 1;
            collector.unreadable_folder("antigravity", directory);
        }
        return true;
    };
    if !state.dirs.insert(id) {
        return true;
    }
    let names = match fsx::list_dir(directory) {
        Ok(names) => names,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
        Err(_) => {
            state.unreadable += 1;
            collector.unreadable_folder("antigravity", directory);
            return true;
        }
    };
    for name in names {
        if collector.expired() || state.dirs.len() > crate::fsx::WALK_MAX_DIRS {
            collector.truncated = true;
            return false;
        }
        let path = directory.join(&name);
        let real = if is_link(&path) {
            link_inside(&path, start)
        } else {
            Some(path.clone())
        };
        let Some(real) = real else {
            continue;
        };
        let Some(meta) = fsx::discovery_metadata(collector, "antigravity", &real) else {
            // A stat failure already made the kind partial; keep this walk's verdict honest too.
            if !matches!(fsx::metadata(&real), Err(error) if error.kind() == std::io::ErrorKind::NotFound) {
                state.unreadable += 1;
            }
            continue;
        };
        if meta.is_dir() {
            if name == "secrets" {
                continue;
            }
            if depth >= AGY_MAX_DEPTH {
                collector.truncated = true;
                continue;
            }
            if !walk(collector, &real, start, depth + 1, state) {
                return false;
            }
        } else if name.ends_with(".db") && meta.is_file() {
            let canonical = realpath(&real);
            if !state.seen.insert(canonical) {
                continue;
            }
            if state.files.len() >= AGY_MAX_DBS {
                collector.truncated = true;
                return false;
            }
            let session = if name.len() > 3 {
                name.get(..name.len() - 3).unwrap_or(&name).to_string()
            } else {
                name.clone()
            };
            state.files.push((real, session));
        }
    }
    true
}

/// One fingerprint over every database and write-ahead log: a record counts once across databases.
fn store_print(files: &[(PathBuf, String)]) -> util::Print {
    let mut digest_input = String::new();
    let (mut size, mut newest) = (0u64, 0i64);
    for (path, _) in files {
        let Ok(meta) = fs::metadata(path) else {
            continue;
        };
        let (mut wal_size, mut wal_ns) = (0u64, 0i128);
        if let Ok(wal) = fs::metadata(format!("{}-wal", path_text(path))) {
            wal_size = wal.len();
            wal_ns = util::mtime_ns(&wal);
        }
        size += meta.len();
        newest = newest.max(util::mtime_ms(&meta)).max((wal_ns / 1_000_000) as i64);
        digest_input.push_str(&format!(
            "{}\0{}\0{}\0{}\0{}\n",
            util::filekey("antigravity", path),
            meta.len(),
            util::mtime_ns(&meta),
            wal_size,
            wal_ns
        ));
    }
    let head = util::sha256_hex(digest_input.as_bytes());
    util::Print::plain(size, newest, head.clone(), head)
}

struct Group {
    parent: usize,
    size: usize,
    owner: usize,
    session: String,
    model: String,
    stamp: f64,
    quality: u8,
    tokens: [u64; 4],
}

/// Python's `_collect_antigravity`.
pub fn collect(collector: &mut Collector, home: &Path, env: &Env) -> &'static str {
    let (dirs, present) = roots(home, env, collector);
    if !present {
        return "not_installed";
    }
    let mut state = WalkState {
        dirs: HashSet::new(),
        seen: HashSet::new(),
        files: Vec::new(),
        unreadable: 0,
    };
    for directory in &dirs {
        let start = state.files.len();
        let walked = walk(collector, directory, directory, 0, &mut state);
        let found: Vec<PathBuf> = state.files[start..].iter().map(|(p, _)| p.clone()).collect();
        // A root with a `conversations` folder is read there: count it under the root.
        let owner = if directory.file_name().is_some_and(|n| n == "conversations") {
            directory
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| directory.clone())
        } else {
            directory.clone()
        };
        collector.count_root("antigravity", &realpath(&owner), &found);
        if !walked {
            return "ok";
        }
    }
    if state.files.is_empty() {
        return if state.unreadable > 0 { "error" } else { "ok" };
    }
    let storekey = util::filekey("antigravity", home);
    let current = store_print(&state.files);
    let unchanged = util::same_prior_print(collector.prior_print("antigravity", &storekey), &current);
    collector
        .pending
        .entry("antigravity".into())
        .or_default()
        .insert(storekey.clone(), current);
    if unchanged {
        collector.confirm_file("antigravity", &storekey);
        return "ok";
    }
    let mut read: Vec<(String, Vec<Candidate>)> = Vec::new();
    let mut named: HashMap<(u64, String), usize> = HashMap::new();
    let mut total = 0usize;
    let deadline = collector.deadline();
    for (path, session) in &state.files {
        if collector.expired() {
            collector.truncated = true;
            return "ok";
        }
        let meta = match fsx::metadata(path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                state.unreadable += 1;
                collector.unreadable_folder("antigravity", path);
                continue;
            }
        };
        if meta.len() > AGY_MAX_DB_BYTES {
            state.unreadable += 1;
            continue;
        }
        match read_db(path, util::mtime_seconds(&meta) * 1000.0, deadline) {
            Ok((candidates, names)) => {
                total += candidates.len();
                if total > AGY_MAX_CANDIDATES {
                    collector.truncated = true;
                    return "ok";
                }
                for name in names {
                    *named.entry(name).or_default() += 1;
                }
                read.push((util::prefix_chars(session, util::SESSION_MAX_LEN), candidates));
            }
            Err(AgyError::NotConversation) => continue,
            Err(_) => {
                if collector.expired() {
                    collector.truncated = true;
                    return "ok";
                }
                if !matches!(fsx::metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound) {
                    state.unreadable += 1;
                }
            }
        }
    }
    let mut ordered: Vec<((u64, String), usize)> = named.into_iter().collect();
    ordered.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut learned: HashMap<u64, String> = HashMap::new();
    for ((id, model), _) in ordered {
        learned.entry(id).or_insert(model);
    }
    // T3's alias merge: union-find over every record id, across databases.
    let mut groups: Vec<Group> = Vec::new();
    let mut identities: HashMap<String, usize> = HashMap::new();
    fn find(groups: &mut [Group], mut index: usize) -> usize {
        let mut root = index;
        while groups[root].parent != root {
            root = groups[root].parent;
        }
        while index != root {
            let parent = groups[index].parent;
            groups[index].parent = root;
            index = parent;
        }
        root
    }
    fn merge(groups: &mut [Group], left: usize, right: usize) {
        let (mut a, mut b) = (find(groups, left), find(groups, right));
        if a == b {
            return;
        }
        if groups[a].size < groups[b].size {
            std::mem::swap(&mut a, &mut b);
        }
        let (target_owner, source_owner) = (groups[a].owner, groups[b].owner);
        let first_is_target = target_owner < source_owner;
        let source_better = groups[b].quality > groups[a].quality
            || (groups[b].quality == groups[a].quality && groups[b].stamp < groups[a].stamp);
        let (first_model, first_session, first_owner) = if first_is_target {
            (groups[a].model.clone(), groups[a].session.clone(), groups[a].owner)
        } else {
            (groups[b].model.clone(), groups[b].session.clone(), groups[b].owner)
        };
        let model = if first_model == AGY_UNKNOWN {
            if first_is_target {
                groups[b].model.clone()
            } else {
                groups[a].model.clone()
            }
        } else {
            first_model
        };
        let (best_stamp, best_quality) = if source_better {
            (groups[b].stamp, groups[b].quality)
        } else {
            (groups[a].stamp, groups[a].quality)
        };
        let tokens: [u64; 4] = std::array::from_fn(|i| groups[a].tokens[i].max(groups[b].tokens[i]));
        let source_size = groups[b].size;
        let target = &mut groups[a];
        target.tokens = tokens;
        target.model = model;
        target.session = first_session;
        target.stamp = best_stamp;
        target.quality = best_quality;
        target.owner = first_owner;
        target.size += source_size;
        groups[b].parent = a;
    }
    for (session, candidates) in &read {
        for candidate in candidates {
            let index = groups.len();
            groups.push(Group {
                parent: index,
                size: 1,
                owner: index,
                session: session.clone(),
                model: pick_model(candidate, &learned),
                stamp: candidate.stamp,
                quality: candidate.quality,
                tokens: candidate.tokens,
            });
            for key in &candidate.keys {
                if let Some(existing) = identities.get(key).copied() {
                    merge(&mut groups, index, existing);
                }
                identities.insert(key.clone(), index);
            }
        }
    }
    for (index, group) in groups.iter().enumerate() {
        if group.parent != index || group.stamp < collector.min_date_ms as f64 {
            continue;
        }
        let epoch_ms = group.stamp as i64;
        let Some(hour) = util::hour_label(epoch_ms, collector.now_ms) else {
            continue;
        };
        let model = util::clean_model(Some(&serde_json::Value::String(group.model.clone())))
            .unwrap_or_else(|| AGY_UNKNOWN.to_string());
        let [uncached, cached, written, output] = group.tokens;
        let tokens = [uncached, output, cached, written];
        collector.add("antigravity", &storekey, &model, None, &hour, tokens, 0.0);
        let session_key = util::session_key("antigravity", Some(&group.session));
        // A step with no timestamp and no trajectory date is stamped with the database's file
        // time. The number is still counted, but a file time never becomes a usage date: the
        // session row keeps it for parity and the measured freshness clock stays without it.
        if group.quality == 0 {
            collector.sadd_estimated(
                "antigravity",
                &storekey,
                session_key.as_deref(),
                &model,
                None,
                epoch_ms,
                tokens,
                0.0,
            );
        } else {
            collector.sadd(
                "antigravity",
                &storekey,
                session_key.as_deref(),
                &model,
                None,
                epoch_ms,
                tokens,
                0.0,
            );
        }
        collector.note_session("antigravity", &group.session, None);
    }
    collector.agy_unreadable = state.unreadable;
    if read.is_empty() {
        return "error";
    }
    if state.unreadable == 0 {
        collector.confirm_file("antigravity", &storekey);
    }
    "ok"
}

#[cfg(test)]
#[path = "antigravity_oracle.rs"]
mod oracle;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_names_read_as_t3_reads_them() {
        assert_eq!(model_name("Claude Sonnet 4.5 (Thinking)", 0), "claude-sonnet-4-5");
        assert_eq!(model_name("Gemini 3.8 Flash", 0), "gemini-3.8-flash");
        assert_eq!(model_name("", 246), "gemini-2.5-pro");
        assert_eq!(model_name("", 9999), "antigravity-model-9999");
        assert_eq!(model_name("", 0), "");
        assert_eq!(strip_suffix("a (b) (c)"), "a (b)");
    }

    #[test]
    fn varints_past_two_to_the_53_are_no_number() {
        let big = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f];
        assert!(matches!(varint(&big, 0).unwrap().0, Field::Big));
        assert!(matches!(varint(&[0x96, 0x01], 0).unwrap().0, Field::Int(150)));
        assert!(varint(&[0x80], 0).is_err());
    }

    fn varint_bytes(mut value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return out;
            }
            out.push(byte | 0x80);
        }
    }

    fn int(field: u64, value: u64) -> Vec<u8> {
        [varint_bytes(field * 8), varint_bytes(value)].concat()
    }

    fn blob(field: u64, value: &[u8]) -> Vec<u8> {
        [
            varint_bytes(field * 8 + 2),
            varint_bytes(value.len() as u64),
            value.to_vec(),
        ]
        .concat()
    }

    /// A small deterministic generator, so every case is the same on every run.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    fn usage(rng: &mut Rng, damaged: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for key in [1u64, 2, 3, 4, 5, 9, 10] {
            if rng.below(4) != 0 {
                let value = match rng.below(10) {
                    0 => 0,
                    1 => (1 << 53) + 5, // past 2^53: no number
                    _ => rng.below(5000),
                };
                out.extend(int(key, value));
            }
        }
        for key in [11u64, 12, 7] {
            match rng.below(6) {
                0 if damaged => out.extend(blob(key, &[0xff, 0xfe])), // not UTF-8
                1 => out.extend(blob(key, " \u{a0}padded id\t".as_bytes())),
                2 | 3 => out.extend(blob(key, format!("id-{}", rng.below(40)).as_bytes())),
                _ => {}
            }
        }
        out
    }

    fn stamp(rng: &mut Rng) -> Vec<u8> {
        [
            int(1, 1_790_000_000 + rng.below(1_000_000)),
            int(2, rng.below(999_999_999)),
        ]
        .concat()
    }

    /// One `steps.metadata` (step) or `gen_metadata.data` blob; sometimes damaged.
    fn record(rng: &mut Rng, step: bool, damaged: bool) -> Vec<u8> {
        if damaged && rng.below(40) == 0 {
            return vec![0x80]; // a varint cut short
        }
        let mut data = Vec::new();
        if rng.below(5) != 0 {
            data.extend(blob(if step { 9 } else { 4 }, &usage(rng, damaged)));
        }
        for _ in 0..rng.below(3) {
            data.extend(blob(if step { 28 } else { 17 }, &blob(2, &usage(rng, damaged))));
        }
        let names = ["Claude Sonnet 4.5 (Thinking)", "Gemini 3.1 Pro", "", "GPT 6 (High)"];
        let name = names[rng.below(4) as usize];
        let mut model = Vec::new();
        if step {
            if !name.is_empty() {
                model.extend(blob(if rng.below(2) == 0 { 12 } else { 8 }, name.as_bytes()));
            }
            model.extend(int(1, [0, 246, 1035, 9999][rng.below(4) as usize]));
            data.extend(blob(24, &model));
            match rng.below(3) {
                0 => data.extend(blob(8, &stamp(rng))),
                1 => data.extend(blob(1, &stamp(rng))),
                _ => {}
            }
            data
        } else {
            if !name.is_empty() {
                data.extend(blob(if rng.below(2) == 0 { 19 } else { 21 }, name.as_bytes()));
            }
            data.extend(int(3, [0, 246, 1016, 777][rng.below(4) as usize]));
            if rng.below(2) == 0 {
                data.extend(blob(9, &blob(4, &stamp(rng))));
            }
            if damaged && rng.below(30) == 0 {
                return data; // no field 1: refused
            }
            blob(1, &data)
        }
    }

    fn database(path: &Path, rng: &mut Rng) {
        let connection = Connection::open(path).unwrap();
        connection.execute_batch("BEGIN").unwrap();
        let shape = rng.below(4);
        let damaged = rng.below(3) == 0;
        if shape != 1 {
            connection
                .execute_batch("CREATE TABLE gen_metadata (idx, data)")
                .unwrap();
            for i in 0..rng.below(300) {
                let idx: rusqlite::types::Value = match if damaged { rng.below(60) } else { 99 } {
                    0 => rusqlite::types::Value::Text("x".into()),
                    1 => rusqlite::types::Value::Real(i as f64 + 0.5),
                    _ => rusqlite::types::Value::Integer(i as i64),
                };
                let data: rusqlite::types::Value = match if damaged { rng.below(80) } else { 99 } {
                    0 => rusqlite::types::Value::Null,
                    1 => rusqlite::types::Value::Text("text".into()),
                    _ => rusqlite::types::Value::Blob(record(rng, false, damaged)),
                };
                connection
                    .execute("INSERT INTO gen_metadata VALUES (?1, ?2)", (idx, data))
                    .unwrap();
            }
        }
        if shape != 2 {
            connection
                .execute_batch("CREATE TABLE steps (idx, metadata, content)")
                .unwrap();
            for i in 0..rng.below(600) {
                let metadata: rusqlite::types::Value = match if damaged { rng.below(70) } else { 99 } {
                    0 => rusqlite::types::Value::Null,
                    1 => rusqlite::types::Value::Text("text".into()),
                    _ => rusqlite::types::Value::Blob(record(rng, true, damaged)),
                };
                connection
                    .execute(
                        "INSERT INTO steps VALUES (?1, ?2, ?3)",
                        (i as i64, metadata, "conversation text"),
                    )
                    .unwrap();
            }
        }
        if shape == 3 || rng.below(2) == 0 {
            connection
                .execute_batch("CREATE TABLE trajectory_metadata_blob (data)")
                .unwrap();
            for _ in 0..rng.below(4) {
                let data: rusqlite::types::Value = match if damaged { rng.below(5) } else { 2 + rng.below(3) } {
                    0 => rusqlite::types::Value::Null,
                    1 => rusqlite::types::Value::Blob(vec![0x80]),
                    2 => rusqlite::types::Value::Blob(Vec::new()),
                    _ => rusqlite::types::Value::Blob(blob(2, &stamp(rng))),
                };
                connection
                    .execute("INSERT INTO trajectory_metadata_blob VALUES (?1)", [data])
                    .unwrap();
            }
        }
        if shape == 0 && rng.below(8) == 0 {
            connection
                .execute_batch("DROP TABLE gen_metadata; DROP TABLE IF EXISTS steps")
                .unwrap();
        }
        connection.execute_batch("COMMIT").unwrap();
    }

    /// R4: the streaming reader answers exactly as the reader that held every blob first: the same
    /// candidates and names in the same order, or the same refusal, over 400 generated databases
    /// (damaged blobs, missing and non-blob rows, non-UTF-8 ids on counted and uncounted usages,
    /// timestamps in either place, a trajectory row before or after a damaged one, no usage tables).
    #[test]
    fn the_streaming_reader_answers_as_the_reader_that_held_every_blob() {
        let dir = tempfile::tempdir().unwrap();
        let deadline = Instant::now() + Duration::from_secs(600);
        let (mut ok, mut refused) = (0, 0);
        for case in 0..400u64 {
            let mut rng = Rng(case.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
            let path = dir.path().join(format!("case-{case}.db"));
            database(&path, &mut rng);
            let streamed = read_db(&path, 1_790_000_000_000.0, deadline);
            let held = oracle::read_db(&path, 1_790_000_000_000.0, deadline);
            match (&streamed, &held) {
                (Ok(a), Ok(b)) => {
                    assert_eq!(a.0, b.0, "case {case}: candidates");
                    assert_eq!(a.1, b.1, "case {case}: names");
                    ok += 1;
                }
                (Err(a), Err(b)) => {
                    assert_eq!(a, b, "case {case}: refusal");
                    refused += 1;
                }
                _ => panic!(
                    "case {case}: streamed {streamed:?} but held {held:?}",
                    streamed = streamed.as_ref().map(|r| r.0.len()),
                    held = held.as_ref().map(|r| r.0.len())
                ),
            }
        }
        // Both outcomes are exercised.
        assert!(ok > 100 && refused > 50, "ok {ok}, refused {refused}");
    }
}
