//! W3: ZCode desktop and T3's ACP adapters (`muse-acp`, `zcode-acp-server`), the port of
//! `app_update_zcode.py`.
//!
//! ZCode comes only from its official release page and the CDN manifest of that exact
//! release; the adapters only from npm's latest tags. Nothing here stops a T3 session: a row
//! that would replace files a session uses reports `action_required` / `in_use` and leaves
//! them as they were; an adapter that is free still updates on its own.
use super::{
    AppRow, AppUpdater, BoxFuture, CommandSpec, Context, Detection, FailureCause, Installed,
    Platform, Process, ProcessTable, Shared, Status, UpdateFailure,
    common::{EnvMode, version_tuple},
    processes, shared,
};
use crate::apps::Host;
use serde_json::{Value, json};
use sha2::Digest as _;
use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    io::Read as _,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex, Weak, atomic::Ordering},
    time::Duration,
};
use tokio::time::Instant;

const PAGE: &str = "https://zcode.z.ai/en";
const RELEASES: &str = "https://cdn-zcode.z.ai/zcode/electron/releases/";
const MAC_NAME: &str = "ZCode.app";
const MAC_ID: &str = "dev.zcode.app";
const MAC_TEAM: &str = "8A5X4JJ39T";
/// The signer's CN and O are Chinese; its registration number is the stable ASCII part.
const WINDOWS_SIGNER: &str = "SERIALNUMBER=91110108MA01KP2T5U";
const MAX_PACKAGE: u64 = 800 * 1024 * 1024;
const PAGE_LIMIT: u64 = 4 * 1024 * 1024;
const PAGE_ATTEMPTS: u32 = 3;
/// The asar header lists ZCode's whole node_modules (about 7 MB in 3.14.5).
const ASAR_HEADER_LIMIT: u64 = 16 * 1024 * 1024;
const UNIT_SECONDS: u64 = 600;
const NPM_SECONDS: u64 = 300;
const SETTINGS_LIMIT: u64 = 4 * 1024 * 1024;
const COPY_LIMIT: usize = 4;

/// The adapters in `PACKAGES` order: each one's name and npm package.
const PACKAGES: [(&str, &str); PARTS] = [
    ("muse-acp", "@brokkai/muse-acp"),
    ("zcode-acp-server", "zcode-acp-server"),
];
/// Python `BINS`: the commands that start each adapter.
const MUSE_BINS: &[&str] = &["muse-acp"];
const ACP_BINS: &[&str] = &["zcode-acp-server", "zcode-acp"];
const BINS: [&[&str]; PARTS] = [MUSE_BINS, ACP_BINS];
const REGISTRY: [&str; PARTS] = [
    "https://registry.npmjs.org/@brokkai%2fmuse-acp/latest",
    "https://registry.npmjs.org/zcode-acp-server/latest",
];
const PARTS: usize = 2;
/// A T3 ZCode session: the adapter, or ZCode's bundled CLI started by path.
const SESSION_MARKERS: [&str; 2] = ["zcode-acp-server/dist/cli.js", "resources/glm/zcode.cjs"];
/// The adapter names t3-acp-update's own deferral looks for.
const ADAPTER_NAMES: [&str; 2] = ["muse-acp", "muse-acp.cjs"];
/// Rows that change nothing and tell the user what to do.
const ACTIONS: [&str; 2] = ["quit_first", "in_use"];
/// The two `Info.plist` fields this module reads.
const PLIST_FIELDS: [&str; 2] = ["CFBundleIdentifier", "CFBundleShortVersionString"];

/// One adapter, as an index into [`PACKAGES`].
type Adapter = usize;
/// One copy an install step is bound to: the adapter and its owner's index.
type Plan = (Adapter, usize);
type Copies = Vec<(PathBuf, Option<String>)>;
type Versions = Vec<Option<String>>;
type Parts = [Option<String>; PARTS];
/// Which adapter packages are busy or held.
type Busy = [bool; PARTS];

static EXACT: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\A[0-9]+\.[0-9]+\.[0-9]+\z").expect("exact version"));
static PART: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\A[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?\z").expect("part version")
});
/// npm's words for a file a running program holds: Windows refuses to replace or remove it.
static LOCKED: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?-u:\b)(?:EPERM|EACCES|EBUSY)(?-u:\b)").expect("locked words")
});
/// A syscall name is not lock evidence: ACLs can deny rename/unlink even when the parent is writable.
static DENIED: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?-u:\b)(?:EPERM|EACCES)(?-u:\b)").expect("denied words"));

/// Python `UBUNTU_RUN["code"]`: `None` until this job's one unit run finished; then its
/// failure code, or `None` for a run that succeeded.
type UnitCode = Option<Option<&'static str>>;

/// Per-job state of Ubuntu's one `t3-acp-update.service` run: Python `UBUNTU_RUN` plus the
/// pre-unit detection snapshots Python captured at job start, so the second row of a shared
/// unit run still compares against pre-unit versions.
#[derive(Default)]
struct UnitRun {
    code: UnitCode,
    adapters_before: Option<Layout>,
    zcode_before: Option<Option<String>>,
}

/// Python `UBUNTU_RUN`: the one t3-acp-update run of a job, shared by the ZCode and adapter
/// rows. The helper had one process per job; here an entry lives as long as the job's
/// [`Shared`] does, so a later job never inherits it.
static UNIT_RUNS: Mutex<Vec<(Weak<Shared>, UnitRun)>> = Mutex::new(Vec::new());

/// The process environment this module reads besides [`Host`] (Python `os.environ`).
/// Production reads the real one; fixtures answer from their own table.
trait Vars: Sync {
    fn get(&self, name: &str) -> Option<String>;
}

struct Ambient;

impl Vars for Ambient {
    fn get(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|value| !value.is_empty())
    }
}

/// The host operations that exist only on their own OS (Python `mac_arguments`,
/// `ProcessIdToSessionId`, `terminate_desktops`, `restart_desktops`) and the pause between
/// two page lookups. [`NativeServices`] is the real host; fixtures script the same four
/// the Python tests mock.
trait Services: Send + Sync {
    fn arguments<'a>(&'a self, cx: &'a Context, pid: u32) -> BoxFuture<'a, Vec<String>>;
    /// This process's Windows desktop session; `None` elsewhere or when unreadable.
    fn session(&self) -> Option<u32>;
    /// The count forced.
    fn terminate<'a>(
        &'a self,
        cx: &'a Context,
        install: &'a Installed,
        contexts: &'a [Process],
    ) -> BoxFuture<'a, Result<u32, UpdateFailure>>;
    /// The count reopened.
    fn restart<'a>(
        &'a self,
        cx: &'a Context,
        install: &'a Installed,
        contexts: &'a [Process],
    ) -> BoxFuture<'a, Result<u32, UpdateFailure>>;
    fn pause<'a>(&'a self, seconds: Duration) -> BoxFuture<'a, ()> {
        Box::pin(tokio::time::sleep(seconds))
    }
}

struct NativeServices;

impl Services for NativeServices {
    fn arguments<'a>(&'a self, cx: &'a Context, pid: u32) -> BoxFuture<'a, Vec<String>> {
        Box::pin(processes::mac_arguments(cx, pid))
    }

    fn session(&self) -> Option<u32> {
        processes::own_session()
    }

    fn terminate<'a>(
        &'a self,
        cx: &'a Context,
        install: &'a Installed,
        contexts: &'a [Process],
    ) -> BoxFuture<'a, Result<u32, UpdateFailure>> {
        Box::pin(processes::terminate_desktops(
            cx,
            install,
            contexts,
            Duration::from_secs(15),
            || Ok(()),
        ))
    }

    fn restart<'a>(
        &'a self,
        cx: &'a Context,
        install: &'a Installed,
        contexts: &'a [Process],
    ) -> BoxFuture<'a, Result<u32, UpdateFailure>> {
        Box::pin(processes::restart_desktops(cx, install, contexts))
    }
}

/// Python `state` of one row: whether an install step started, so the row reports the
/// bits actually on disk.
#[derive(Default)]
struct Attempt {
    attempted: bool,
}

/// One global package folder (Python `Owner`): node_modules itself, the prefix an
/// installer is bound to and its manager.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Owner {
    root: PathBuf,
    prefix: PathBuf,
    manager: &'static str,
    source: &'static str,
    /// T3's own node, when its config names one.
    node: Option<PathBuf>,
    /// pnpm only: the folder its global bin links go to (PNPM_HOME).
    bin_dir: Option<PathBuf>,
}

/// One copy T3's enabled instances run: its node_modules folder, T3's node and why it
/// cannot be installed into (`None` when it can).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Consumer {
    root: PathBuf,
    node: Option<PathBuf>,
    refusal: Option<&'static str>,
}

/// What detection found (Python the extra fields of `AdaptersInstall`): the owners of
/// each adapter, every copy and the owners' versions when they were read.
#[derive(Clone, Debug, Default)]
struct Layout {
    owners: [Vec<Owner>; PARTS],
    copies: [Copies; PARTS],
    before: [Versions; PARTS],
}

/// A small insertion-ordered map of adapter to failure code (Python's `failed` dict,
/// whose first insertion decides the row's code).
#[derive(Default)]
struct Codes {
    items: Vec<(Adapter, &'static str)>,
}

impl Codes {
    fn has(&self, adapter: Adapter) -> bool {
        self.items.iter().any(|(found, _)| *found == adapter)
    }

    fn get(&self, adapter: Adapter) -> Option<&'static str> {
        self.items
            .iter()
            .find(|(found, _)| *found == adapter)
            .map(|(_, code)| *code)
    }

    fn set(&mut self, adapter: Adapter, code: &'static str) {
        match self.items.iter_mut().find(|(found, _)| *found == adapter) {
            Some(item) => item.1 = code,
            None => self.items.push((adapter, code)),
        }
    }

    fn values(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.items.iter().map(|(_, code)| *code)
    }
}

// ---------------------------------------------------------------------------
// Versions and installed bits
// ---------------------------------------------------------------------------

fn exact_version(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| value.len() <= 32 && EXACT.is_match(value))
        .map(str::to_owned)
}

fn part_version(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| value.len() <= 64 && PART.is_match(value))
        .map(str::to_owned)
}

fn key(value: &str) -> Result<shared::NpmKey, UpdateFailure> {
    shared::npm_version_key(value)
}

/// Order by [`key`]; two values that are not versions compare equal.
fn order(left: &str, right: &str) -> std::cmp::Ordering {
    match (key(left), key(right)) {
        (Ok(left), Ok(right)) => left.cmp(&right),
        _ => std::cmp::Ordering::Equal,
    }
}

/// Python `lowest`: the lowest of several copies' versions; `None` when any copy's is unreadable.
fn lowest(versions: &[Option<String>]) -> Option<String> {
    let _ = versions.first()?.as_deref()?;
    if versions.iter().any(Option::is_none) {
        return None;
    }
    versions
        .iter()
        .filter_map(|version| version.as_deref())
        .reduce(|kept, next| {
            if order(next, kept).is_lt() {
                next
            } else {
                kept
            }
        })
        .map(str::to_owned)
}

/// Python `path_text`: one spelling for Windows and POSIX paths, so a marker matches either.
fn path_text(value: impl AsRef<str>) -> String {
    value.as_ref().replace('\\', "/").to_lowercase()
}

/// Python `PurePosixPath(text).name`.
fn posix_name(text: &str) -> &str {
    text.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
}

fn real(path: &Path) -> PathBuf {
    crate::apps::real(path)
}

/// Python `_lexical`: the path as written, made absolute, without following links.
fn lexical(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| path.to_path_buf())
            .join(path)
    };
    aac_files::path::python::normpath(&absolute)
}

fn component(path: &Path, name: &str) -> bool {
    path.components()
        .any(|part| part.as_os_str() == OsStr::new(name))
}

/// A small file read whole through links (Python `open`), `None` past `cap`.
fn read_bytes(path: &Path, cap: u64) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= cap).then_some(bytes)
}

fn read_json(path: &Path, cap: u64) -> Option<Value> {
    serde_json::from_slice(&read_bytes(path, cap)?).ok()
}

/// Python `Path.expanduser`: a leading `~` is the home folder.
fn expand_user(path: &Path, home: &Path) -> PathBuf {
    let mut parts = path.components();
    if parts.next().is_some_and(|part| part.as_os_str() == "~") {
        return parts.fold(home.to_path_buf(), |mut expanded, part| {
            expanded.push(part.as_os_str());
            expanded
        });
    }
    path.to_path_buf()
}

/// Python `os.path.expandvars` for `$NAME` and `${NAME}`; an unset name stays verbatim.
fn expand_vars(text: &str, vars: &dyn Vars) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let (raw, name, tail) = match after.strip_prefix('{') {
            Some(inner) => match inner.find('}') {
                Some(end) => (
                    &rest[at..at + 2 + end + 1],
                    &inner[..end],
                    &inner[end + 1..],
                ),
                None => {
                    out.push_str(&rest[at..]);
                    return out;
                }
            },
            None => {
                let end = after
                    .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
                    .unwrap_or(after.len());
                (&rest[at..at + 1 + end], &after[..end], &after[end..])
            }
        };
        if name.is_empty() {
            out.push('$');
            rest = after;
            continue;
        }
        match vars.get(name) {
            Some(value) => out.push_str(&value),
            None => out.push_str(raw),
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// The top-level dict's string fields of an `Info.plist`, XML or binary (what
/// `plistlib.load` gives this module's two `get` calls). Bounded, and `None` for
/// anything it cannot read.
fn plist_strings(bytes: &[u8]) -> Option<Vec<(String, String)>> {
    if bytes.starts_with(b"bplist") {
        binary_plist(bytes)
    } else {
        xml_plist(bytes)
    }
}

fn decode16(bytes: &[u8], big: bool) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            if big {
                u16::from_be_bytes(*pair)
            } else {
                u16::from_le_bytes(*pair)
            }
        })
        .collect();
    char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .ok()
}

/// Index of the `>` that closes the tag opened at `open`, quotes respected.
fn tag_end(text: &str, open: usize) -> Option<usize> {
    let mut quote = None;
    for (index, ch) in text[open + 1..].char_indices() {
        match (quote, ch) {
            (Some(closing), _) if closing == ch => quote = None,
            (None, '"' | '\'') => quote = Some(ch),
            (None, '>') => return Some(open + 1 + index + 1),
            _ => {}
        }
    }
    None
}

fn decode_entities(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let end = rest[at..].find(';')? + at;
        let name = &rest[at + 1..end];
        let decoded = match name {
            "amp" => "&".to_owned(),
            "lt" => "<".to_owned(),
            "gt" => ">".to_owned(),
            "quot" => "\"".to_owned(),
            "apos" => "'".to_owned(),
            hex if hex.starts_with("#x") || hex.starts_with("#X") => {
                char::from_u32(u32::from_str_radix(&name[2..], 16).ok()?)?.to_string()
            }
            decimal if decimal.starts_with('#') => {
                char::from_u32(name[1..].parse().ok()?)?.to_string()
            }
            _ => return None,
        };
        out.push_str(&decoded);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// The text of the element just opened at `at`, up to its closing tag.
fn element_text(text: &str, at: usize, name: &str) -> Option<(String, usize)> {
    let mut content = String::new();
    let mut rest = at;
    loop {
        let open = rest + text[rest..].find('<')?;
        content.push_str(&text[rest..open]);
        if text[open..].starts_with("<![CDATA[") {
            let start = open + "<![CDATA[".len();
            let end = start + text[start..].find("]]>")?;
            content.push_str(&text[start..end]);
            rest = end + "]]>".len();
            continue;
        }
        let closing = format!("</{name}>");
        break text[open..]
            .starts_with(&closing)
            .then(|| (content, open + closing.len()));
    }
}

fn xml_plist(bytes: &[u8]) -> Option<Vec<(String, String)>> {
    let text = match bytes {
        [0xff, 0xfe, rest @ ..] => decode16(rest, false)?,
        [0xfe, 0xff, rest @ ..] => decode16(rest, true)?,
        _ => String::from_utf8(bytes.to_vec()).ok()?,
    };
    let mut at = 0;
    let mut stack: Vec<&str> = Vec::new();
    let mut pending: Option<String> = None;
    let mut pairs = Vec::new();
    let mut tags = 0_u32;
    loop {
        at += text[at..].find('<')?;
        tags += 1;
        if tags > 200_000 || stack.len() > 256 {
            return None;
        }
        if text[at..].starts_with("<!--") {
            at += 4 + text[at + 4..].find("-->")? + 3;
            continue;
        }
        if text[at..].starts_with("<![CDATA[") {
            at += "<![CDATA[".len() + text[at + 9..].find("]]>")? + 3;
            continue;
        }
        if text[at..].starts_with("<?") {
            at += 2 + text[at + 2..].find("?>")? + 2;
            continue;
        }
        if text[at..].starts_with("<!") {
            let mut end = tag_end(&text, at)?;
            let inner = &text[at + 2..end - 1];
            if inner.contains('[') && !inner.contains(']') {
                end += text[end..].find("]>")? + 2;
            }
            at = end;
            continue;
        }
        let end = tag_end(&text, at)?;
        let inner = &text[at + 1..end - 1];
        at = end;
        if let Some(closing) = inner.strip_prefix('/') {
            if stack.pop() != Some(closing) {
                return None;
            }
            if stack.is_empty() {
                break;
            }
            continue;
        }
        let split = inner
            .find(|ch: char| ch.is_whitespace() || ch == '/')
            .unwrap_or(inner.len());
        let name = &inner[..split];
        let parent = stack.last().copied();
        let depth = stack.len();
        stack.push(name);
        if inner.ends_with('/') {
            stack.pop();
            if depth == 2 && parent == Some("dict") {
                pending = None;
            }
            if stack.is_empty() {
                break;
            }
            continue;
        }
        if depth == 2 && parent == Some("dict") {
            if matches!(name, "key" | "string") {
                let (content, next) = element_text(&text, at, name)?;
                at = next;
                stack.pop();
                if name == "key" {
                    pending = Some(decode_entities(&content)?);
                } else if let Some(field) = pending.take()
                    && PLIST_FIELDS.contains(&field.as_str())
                {
                    pairs.push((field, decode_entities(&content)?));
                }
            } else {
                pending = None;
            }
        }
    }
    Some(pairs)
}

/// Reads one length: the low nibble, or `0xF` followed by an integer object.
fn plist_length(bytes: &[u8], at: &mut usize, low: usize) -> Option<usize> {
    if low != 0x0F {
        return Some(low);
    }
    let marker = *bytes.get(*at)?;
    if marker >> 4 != 1 {
        return None;
    }
    let size = 1_usize << (marker & 0x0F);
    let start = *at + 1;
    let data = bytes.get(start..start.checked_add(size)?)?;
    *at = start + size;
    let value = if size <= 8 {
        big_endian(data)
    } else {
        big_endian(&data[size - 8..])
    };
    usize::try_from(value).ok()
}

fn big_endian(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0, |value, byte| (value << 8) | u64::from(*byte))
}

fn binary_plist(bytes: &[u8]) -> Option<Vec<(String, String)>> {
    if !bytes.starts_with(b"bplist00") || bytes.len() < 8 + 32 + 1 {
        return None;
    }
    let trailer = &bytes[bytes.len() - 32..];
    let (offset_size, ref_size) = (trailer[6] as usize, trailer[7] as usize);
    if !(1..=8).contains(&offset_size) || !(1..=8).contains(&ref_size) {
        return None;
    }
    let count = big_endian(&trailer[8..16]);
    let top = big_endian(&trailer[16..24]);
    let table = usize::try_from(big_endian(&trailer[24..32])).ok()?;
    if count == 0
        || count > 2_000_000
        || table.checked_add(count as usize * offset_size)? > bytes.len()
    {
        return None;
    }
    let offset = |index: u64| -> Option<usize> {
        if index >= count {
            return None;
        }
        let at = table + index as usize * offset_size;
        Some(usize::try_from(big_endian(&bytes[at..at + offset_size])).ok()?)
            .filter(|at| *at < bytes.len())
    };
    let string = |index: u64| -> Option<String> {
        let mut at = offset(index)?;
        let marker = *bytes.get(at)?;
        let kind = marker >> 4;
        if kind != 5 && kind != 6 {
            return None;
        }
        at += 1;
        let length = plist_length(bytes, &mut at, usize::from(marker & 0x0F))?;
        let width = if kind == 6 { 2 } else { 1 };
        let data = bytes.get(at..at.checked_add(length.checked_mul(width)?)?)?;
        if kind == 6 {
            decode16(data, true)
        } else if data.is_ascii() {
            Some(String::from_utf8_lossy(data).into_owned())
        } else {
            None
        }
    };
    let mut at = offset(top)?;
    let marker = bytes[at];
    if marker >> 4 != 0xD {
        return None;
    }
    at += 1;
    let entries = plist_length(bytes, &mut at, usize::from(marker & 0x0F))?;
    if entries > 1_000_000 {
        return None;
    }
    let keys = at;
    let values = keys + entries * ref_size;
    let pairs = (0..entries)
        .filter_map(|index| {
            let reference = |from: usize| -> Option<u64> {
                let span = from + index * ref_size..from + (index + 1) * ref_size;
                Some(big_endian(bytes.get(span)?))
            };
            Some((string(reference(keys)?)?, string(reference(values)?)?))
        })
        .collect();
    Some(pairs)
}

fn asar_version(resources: &Path) -> Option<String> {
    let package = shared::asar_package(&resources.join("app.asar"), ASAR_HEADER_LIMIT)?;
    (package.get("name").and_then(Value::as_str) == Some("@zcode/desktop"))
        .then(|| exact_version(package.get("version").and_then(Value::as_str)))
        .flatten()
}

fn mac_version(bundle: &Path) -> Option<String> {
    let fields = plist_strings(&read_bytes(
        &bundle.join("Contents/Info.plist"),
        1024 * 1024,
    )?)?;
    let field = |name: &str| {
        fields
            .iter()
            .find(|(found, _)| found == name)
            .map(|(_, value)| value.as_str())
    };
    (field("CFBundleIdentifier") == Some(MAC_ID))
        .then(|| exact_version(field("CFBundleShortVersionString")))
        .flatten()
}

/// The version of the installed bits, read without running anything.
fn installed_version(platform: Platform, path: &Path) -> Option<String> {
    match platform {
        Platform::Mac => mac_version(path),
        Platform::Windows => asar_version(&path.parent()?.join("resources")),
        // The asar wins; t3-acp-update's stamp beside the extracted AppImage is the fallback.
        Platform::Ubuntu => asar_version(&path.join("resources")).or_else(|| {
            let mut stamp = Vec::new();
            std::fs::File::open(path.parent()?.join(".installed-version"))
                .ok()?
                .take(64)
                .read_to_end(&mut stamp)
                .ok()?;
            let stamp = std::str::from_utf8(&stamp).ok()?;
            stamp
                .is_ascii()
                .then(|| exact_version(Some(stamp.trim())))
                .flatten()
        }),
    }
}

fn detect_zcode(cx: &Context) -> Option<Installed> {
    let host = &cx.host;
    let mut install = match cx.platform {
        Platform::Mac => {
            let path = host.system("/Applications").join(MAC_NAME);
            let mut install = Installed::new(
                "zcode",
                Platform::Mac,
                path.clone(),
                None,
                "official-download",
            );
            install.identity = Some(MAC_ID.into());
            install.publisher = Some(MAC_TEAM.into());
            install.package_root = Some(path);
            install
        }
        Platform::Windows => {
            let path = host.local.join("Programs/ZCode/ZCode.exe");
            let mut install = Installed::new(
                "zcode",
                Platform::Windows,
                path.clone(),
                None,
                "official-download",
            );
            install.package_root = path.parent().map(Path::to_path_buf);
            install
        }
        Platform::Ubuntu => {
            let path = host.home.join(".local/opt/zcode/app");
            let mut install = Installed::new(
                "zcode",
                Platform::Ubuntu,
                path.clone(),
                None,
                "official-download",
            );
            install.package_root = Some(path);
            install
        }
    };
    let present = if cx.platform == Platform::Windows {
        install.path.is_file()
    } else {
        install.path.is_dir()
    };
    if !present {
        return None;
    }
    let path = install.path.clone();
    install.version = if cx.platform == Platform::Ubuntu {
        unit_zcode_before(&cx.shared).unwrap_or_else(|| installed_version(cx.platform, &path))
    } else {
        installed_version(cx.platform, &path)
    };
    Some(install)
}

// ---------------------------------------------------------------------------
// Which copy of each adapter T3 actually runs, and which installer owns it.
// A successful `npm install -g` can land in a prefix nothing runs, so the owner is
// found from what consumes the package, the install is bound to that owner's prefix,
// and verification reads that copy.
// ---------------------------------------------------------------------------

fn adapter_root(host: &Host) -> PathBuf {
    match host.platform {
        Platform::Windows => host.roaming.join("npm/node_modules"),
        Platform::Mac => host.system("/opt/homebrew/lib/node_modules"),
        Platform::Ubuntu => host.home.join(".local/lib/node_modules"),
    }
}

fn manifest(root: &Path, adapter: Adapter) -> PathBuf {
    root.join(PACKAGES[adapter].1).join("package.json")
}

fn suffixes(platform: Platform) -> &'static [&'static str] {
    if platform == Platform::Windows {
        &[".cmd", ".ps1", ""]
    } else {
        &[""]
    }
}

/// The installer that owns one node_modules folder, by its layout. On Linux and the Mac
/// npm's global folder is `<prefix>/lib/node_modules`; any other plain folder has no
/// installer AAC can bind.
fn owner_of_modules(
    modules: &Path,
    source: &'static str,
    node: Option<PathBuf>,
    platform: Platform,
) -> Owner {
    let modules = real(modules);
    let text = path_text(modules.to_string_lossy());
    let parent = modules.parent().map(Path::to_path_buf);
    let named = |name: &str| {
        parent
            .as_ref()
            .and_then(|parent| parent.file_name())
            .is_some_and(|found| found == name)
    };
    let digits = |value: &Option<PathBuf>| {
        value
            .as_ref()
            .and_then(|value| value.file_name())
            .and_then(OsStr::to_str)
            .is_some_and(|found| {
                !found.is_empty() && found.bytes().all(|byte| byte.is_ascii_digit())
            })
    };
    let (prefix, manager, bin_dir) = if format!("{text}/").contains("/.pnpm/") {
        // A path inside pnpm's store is never an install target (package_in maps it back first).
        (parent.clone(), "other", None)
    } else if text.contains("/pnpm/global/") || text.ends_with("/.pnpm-global/node_modules") {
        let global = if digits(&parent) {
            parent
                .as_ref()
                .and_then(|parent| parent.parent())
                .map(Path::to_path_buf)
        } else {
            parent.clone()
        };
        (
            global.clone(),
            "pnpm",
            global
                .as_ref()
                .and_then(|global| global.parent())
                .map(Path::to_path_buf),
        )
    } else if text.ends_with("/install/global/node_modules") && text.contains("/.bun/") {
        (parent.clone(), "bun", None)
    } else if text.ends_with("/yarn/global/node_modules") {
        (parent.clone(), "yarn", None)
    } else if named("lib") {
        (
            parent
                .as_ref()
                .and_then(|parent| parent.parent())
                .map(Path::to_path_buf),
            "npm",
            None,
        )
    } else {
        // Windows npm: <prefix>\node_modules, with the bin shims in <prefix> itself.
        (
            parent.clone(),
            if platform == Platform::Windows {
                "npm"
            } else {
                "other"
            },
            None,
        )
    };
    Owner {
        root: modules,
        prefix: prefix.unwrap_or_default(),
        manager,
        source,
        node,
        bin_dir,
    }
}

/// Python `_modules_of`: (node_modules folder, adapter) for a path inside one adapter package.
fn modules_of(path: &Path) -> Option<(PathBuf, Adapter)> {
    let parts: Vec<&OsStr> = path.components().map(|part| part.as_os_str()).collect();
    let folder = |until: usize| {
        parts[..until]
            .iter()
            .fold(PathBuf::new(), |mut folder, part| {
                folder.push(part);
                folder
            })
    };
    if let Some(index) = parts.iter().position(|part| *part == OsStr::new(".pnpm")) {
        // pnpm's store: the install belongs to the node_modules folder that holds .pnpm.
        if index == 0 || parts[index - 1] != "node_modules" {
            return None;
        }
        let tail = parts[index + 1..]
            .iter()
            .map(|part| part.to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let tail = format!("/{tail}");
        return (0..PARTS)
            .find(|adapter| tail.contains(&format!("/node_modules/{}", PACKAGES[*adapter].1)))
            .map(|adapter| (folder(index), adapter));
    }
    for index in (0..parts.len()).rev() {
        if parts[index] != "node_modules" {
            continue;
        }
        let rest = &parts[index + 1..parts.len().min(index + 3)];
        for (adapter, (_, package)) in PACKAGES.iter().enumerate() {
            let depth = package.split('/').count();
            if rest.len() >= depth
                && rest[..depth]
                    .iter()
                    .map(|part| part.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/")
                    == *package
            {
                return Some((folder(index + 1), adapter));
            }
        }
    }
    None
}

/// (node_modules folder, adapter) for a file inside one adapter package, else `None`.
///
/// The path as written is read first: pnpm's global node_modules/<pkg> is a link into its
/// store, and following it would name the store as the install. Links are followed only
/// when the written path names no package (a bin link such as
/// /opt/homebrew/bin/zcode-acp-server).
fn package_in(path: &Path) -> Option<(PathBuf, Adapter)> {
    modules_of(&lexical(path)).or_else(|| modules_of(&real(path)))
}

/// A Windows npm shim (`<prefix>\zcode-acp-server.cmd`) names a package in
/// `<prefix>\node_modules`.
fn shim_package(path: &Path) -> Option<(PathBuf, Adapter)> {
    let suffix = path
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_lowercase();
    if !matches!(suffix.as_str(), "cmd" | "ps1" | "exe" | "") {
        return None;
    }
    let stem = path.file_stem()?.to_string_lossy().to_lowercase();
    let modules = path.parent()?.join("node_modules");
    (0..PARTS)
        .find(|adapter| {
            BINS[*adapter].contains(&stem.as_str()) && manifest(&modules, *adapter).is_file()
        })
        .map(|adapter| (modules, adapter))
}

/// `{adapter: node_modules}` for the adapter commands found on PATH, first match wins.
fn path_consumers(host: &Host) -> [Option<PathBuf>; PARTS] {
    let mut found: [Option<PathBuf>; PARTS] = [None, None];
    for folder in host.effective_path() {
        for adapter in 0..PARTS {
            if found[adapter].is_some() {
                continue;
            }
            found[adapter] = BINS[adapter]
                .iter()
                .flat_map(|binary| {
                    suffixes(host.platform)
                        .iter()
                        .map(move |suffix| (binary, suffix))
                })
                .find_map(|(binary, suffix)| {
                    let candidate = folder.join(format!("{binary}{suffix}"));
                    candidate
                        .is_file()
                        .then(|| package_in(&candidate).or_else(|| shim_package(&candidate)))
                        .flatten()
                        .map(|(root, _)| root)
                });
        }
    }
    found
}

/// The copy a bare command names in the usual adapter prefix's bin folder. T3 resolves a
/// bare name on its own PATH; the dashboard or its ssh session may not carry that folder.
fn default_consumer(host: &Host, adapter: Adapter) -> Option<PathBuf> {
    let root = adapter_root(host);
    let folder = if host.platform == Platform::Windows {
        root.parent().map(Path::to_path_buf)
    } else {
        root.parent()
            .and_then(Path::parent)
            .map(|prefix| prefix.join("bin"))
    }?;
    BINS[adapter]
        .iter()
        .flat_map(|binary| {
            suffixes(host.platform)
                .iter()
                .map(move |suffix| (binary, suffix))
        })
        .find_map(|(binary, suffix)| {
            let candidate = folder.join(format!("{binary}{suffix}"));
            let hit = candidate
                .is_file()
                .then(|| package_in(&candidate).or_else(|| shim_package(&candidate)))
                .flatten();
            match hit {
                Some((root, found)) if found == adapter => Some(root),
                _ => None,
            }
        })
}

/// A `prefix=` line in the user's ~/.npmrc, or npm_config_prefix: where plain npm -g installs.
fn npmrc_prefix(host: &Host, vars: &dyn Vars) -> Option<PathBuf> {
    if let Some(prefix) = host.npm_prefix.clone().or_else(|| {
        vars.get("npm_config_prefix")
            .or_else(|| vars.get("NPM_CONFIG_PREFIX"))
            .map(PathBuf::from)
    }) {
        return Some(expand_user(&prefix, &host.home));
    }
    let text = String::from_utf8(read_bytes(&host.home.join(".npmrc"), 1024 * 1024)?).ok()?;
    text.lines().take(200).find_map(|line| {
        let (name, rest) = line.split_once('=')?;
        (name.trim() == "prefix" && !rest.trim().is_empty())
            .then(|| expand_user(&PathBuf::from(expand_vars(rest.trim(), vars)), &host.home))
    })
}

/// Every global package folder an adapter copy could live in, most usual first.
fn candidate_roots(host: &Host, vars: &dyn Vars) -> Vec<PathBuf> {
    let home = &host.home;
    let windows = host.platform == Platform::Windows;
    let suffix = if windows {
        "node_modules"
    } else {
        "lib/node_modules"
    };
    let mut roots = vec![adapter_root(host)];
    if let Some(prefix) = npmrc_prefix(host, vars) {
        roots.push(prefix.join(suffix));
    }
    if let Some(located) = host.which("node") {
        let node = real(&located);
        let node_home = if windows {
            node.parent().map(Path::to_path_buf)
        } else {
            node.parent().and_then(Path::parent).map(Path::to_path_buf)
        };
        if let Some(node_home) = node_home {
            roots.push(node_home.join(suffix));
        }
    }
    if windows {
        roots.push(host.program_files.join("nodejs/node_modules"));
    } else {
        roots.extend([
            home.join(".local/lib/node_modules"),
            home.join(".npm-global/lib/node_modules"),
            host.system("/opt/homebrew/lib/node_modules"),
            host.system("/usr/local/lib/node_modules"),
            host.system("/usr/lib/node_modules"),
        ]);
    }
    roots.extend([
        home.join(".bun/install/global/node_modules"),
        home.join(".local/share/pnpm/global/5/node_modules"),
        home.join("Library/pnpm/global/5/node_modules"),
        home.join(".config/yarn/global/node_modules"),
    ]);
    if windows {
        roots.push(host.local.join("pnpm/global/5/node_modules"));
    }
    let mut seen = HashSet::new();
    roots
        .into_iter()
        .filter(|root| seen.insert(real(root).to_string_lossy().into_owned()))
        .collect()
}

fn package_version(root: &Path, adapter: Adapter) -> Option<String> {
    let info = read_json(&manifest(root, adapter), 1024 * 1024)?;
    (info["name"] == PACKAGES[adapter].1)
        .then(|| part_version(info["version"].as_str()))
        .flatten()
}

/// `[(node_modules, version)]` of every copy of one adapter, deduplicated by real folder.
fn copies_of(roots: &[PathBuf], adapter: Adapter) -> Copies {
    roots
        .iter()
        .filter(|root| manifest(root, adapter).is_file())
        .map(|root| (root.clone(), package_version(root, adapter)))
        .collect()
}

/// `{adapter: [(node_modules, node, refusal)]}`: copies T3's enabled instances run.
///
/// Only config.commandPath and config.commandArgs are read: they are paths and flags.
/// Environment values, which can be secrets, are never read. A missing or unreadable
/// settings file names nothing.
fn t3_consumers(host: &Host) -> [Vec<Consumer>; PARTS] {
    let mut found: [Vec<Consumer>; PARTS] = Default::default();
    let settings = host.home.join(".t3/userdata/settings.json");
    let Some(settings) = read_json(&settings, SETTINGS_LIMIT) else {
        return found;
    };
    let Some(instances) = settings["providerInstances"].as_object() else {
        return found;
    };
    let on_path = path_consumers(host);
    let stem = |command: &str| {
        Path::new(command)
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_lowercase())
    };
    for instance in instances.values() {
        if instance.get("enabled") == Some(&Value::Bool(false)) {
            continue;
        }
        let config = instance.get("config").filter(|config| config.is_object());
        let command_path = config.and_then(|config| config["commandPath"].as_str());
        let args: Vec<&str> = config
            .and_then(|config| config["commandArgs"].as_array())
            .map(|args| args.iter().take(16).filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let node = command_path
            .filter(|command| stem(command).as_deref() == Some("node"))
            .map(PathBuf::from);
        if let Some(command) = command_path.filter(|command| !command.contains(['/', '\\'])) {
            for (adapter, bins) in BINS.iter().enumerate() {
                if stem(command).is_some_and(|stem| bins.contains(&stem.as_str())) {
                    let root = on_path[adapter]
                        .clone()
                        .or_else(|| default_consumer(host, adapter));
                    found[adapter].push(Consumer {
                        root: root.clone().unwrap_or_else(|| adapter_root(host)),
                        node: node.clone(),
                        refusal: root.is_none().then_some("mixed_ownership"),
                    });
                }
            }
        }
        for candidate in command_path.into_iter().chain(args) {
            if candidate.len() > 1024 || !candidate.contains(['/', '\\']) {
                continue;
            }
            let path = Path::new(candidate);
            if let Some((root, adapter)) = package_in(path).or_else(|| shim_package(path)) {
                let written = lexical(path);
                let pinned_store = component(&written, ".pnpm")
                    || (component(&real(path), ".pnpm") && modules_of(&written).is_none());
                let refusal = if pinned_store {
                    Some("pnpm_store_copy")
                } else if !path.exists() {
                    Some("mixed_ownership")
                } else {
                    None
                };
                found[adapter].push(Consumer {
                    root,
                    node: node.clone(),
                    refusal,
                });
            }
        }
    }
    found
}

fn owner_versions(layout: &Layout, adapter: Adapter) -> Versions {
    layout.owners[adapter]
        .iter()
        .map(|owner| package_version(&owner.root, adapter))
        .collect()
}

fn owned_parts(layout: &Layout) -> Parts {
    std::array::from_fn(|adapter| {
        if layout.owners[adapter].is_empty() {
            None
        } else {
            lowest(&layout.before[adapter])
        }
    })
}

/// `{adapter: [Owner]}` for each installed adapter and `{adapter: [(root, version)]}` of
/// every copy.
///
/// Precedence: every distinct copy T3's enabled instances run (each is updated and
/// verified), else the command on PATH, else the usual folder for this OS, else the first
/// other copy found. Each owner records its source, so a row says why it updated (and
/// verified) that copy.
fn discover_owners(host: &Host, vars: &dyn Vars) -> Layout {
    let roots = candidate_roots(host, vars);
    let consumers = t3_consumers(host);
    let on_path = path_consumers(host);
    let mut layout = Layout::default();
    for adapter in 0..PARTS {
        layout.copies[adapter] = copies_of(&roots, adapter);
        let first_copy = layout.copies[adapter].first().cloned();
        if !consumers[adapter].is_empty() {
            for consumer in &consumers[adapter] {
                let owner = owner_of_modules(
                    &consumer.root,
                    consumer.refusal.unwrap_or("t3-config"),
                    consumer.node.clone(),
                    host.platform,
                );
                let repeated = layout.owners[adapter]
                    .iter()
                    .any(|other| real(&other.root) == real(&consumer.root));
                if consumer.refusal.is_some() || !repeated {
                    layout.owners[adapter].push(owner);
                }
            }
        } else if let Some(root) = &on_path[adapter] {
            layout.owners[adapter] = vec![owner_of_modules(root, "path", None, host.platform)];
        } else if manifest(&adapter_root(host), adapter).is_file() {
            let root = adapter_root(host);
            layout.owners[adapter] = vec![owner_of_modules(&root, "default", None, host.platform)];
        } else if let Some((root, _)) = first_copy {
            layout.owners[adapter] = vec![owner_of_modules(&root, "found", None, host.platform)];
        }
        for owner in layout.owners[adapter].clone() {
            let root = real(&owner.root);
            if !layout.copies[adapter]
                .iter()
                .any(|(copy, _)| real(copy) == root)
            {
                layout.copies[adapter].insert(
                    0,
                    (owner.root.clone(), package_version(&owner.root, adapter)),
                );
            }
        }
    }
    layout.before = std::array::from_fn(|adapter| owner_versions(&layout, adapter));
    layout
}

/// The other copies of one adapter as they are now (a run can create one), with their versions.
fn elsewhere(roots: &[PathBuf], layout: &Layout, adapter: Adapter) -> Copies {
    let mut skip: HashSet<PathBuf> = layout.owners[adapter]
        .iter()
        .map(|owner| real(&owner.root))
        .collect();
    let mut found = Vec::new();
    for (root, _) in copies_of(roots, adapter).into_iter().chain(
        layout.copies[adapter]
            .iter()
            .map(|(root, _)| (root.clone(), None)),
    ) {
        if skip.insert(real(&root)) {
            let version = package_version(&root, adapter);
            found.push((root, version));
        }
    }
    found.truncate(COPY_LIMIT);
    found
}

/// `diagnostics.owner` for an adapters row: every owner of each package and any other copies.
fn adapters_owner(host: &Host, vars: &dyn Vars, layout: &Layout, after: Option<&Parts>) -> Value {
    let now: [Versions; PARTS] = if after.is_some() {
        std::array::from_fn(|adapter| owner_versions(layout, adapter))
    } else {
        layout.before.clone()
    };
    let roots = candidate_roots(host, vars);
    let mut packages = Vec::new();
    let mut others = Vec::new();
    for adapter in 0..PARTS {
        for (index, owner) in layout.owners[adapter].iter().enumerate() {
            packages.push(json!({
                "package": PACKAGES[adapter].0, "manager": owner.manager,
                "prefix": shared::display_path(host, &owner.prefix), "source": owner.source,
                "version": now[adapter].get(index).cloned().flatten(),
            }));
        }
        for (root, version) in elsewhere(&roots, layout, adapter) {
            let prefix = owner_of_modules(&root, "default", None, Platform::Ubuntu).prefix;
            others.push(json!({
                "package": PACKAGES[adapter].0, "prefix": shared::display_path(host, &prefix), "version": version,
            }));
        }
    }
    packages.truncate(4);
    others.truncate(COPY_LIMIT * 2);
    let mut value = json!({ "packages": packages });
    if !others.is_empty()
        && let Value::Object(fields) = &mut value
    {
        fields.insert("elsewhere".into(), Value::Array(others));
    }
    value
}

fn detect_adapters(cx: &Context, vars: &dyn Vars) -> Option<Installed> {
    let layout = if cx.platform == Platform::Ubuntu {
        unit_adapters_before(&cx.shared).unwrap_or_else(|| discover_owners(&cx.host, vars))
    } else {
        discover_owners(&cx.host, vars)
    };
    let first = (0..PARTS).find(|adapter| !layout.owners[*adapter].is_empty())?;
    let root = layout.owners[first][0].root.clone();
    let mut install = Installed::new("t3-acp-adapters", cx.platform, root.clone(), None, "npm");
    install.package_root = Some(root);
    install.owner = Some(adapters_owner(&cx.host, vars, &layout, None));
    Some(install)
}

// ---------------------------------------------------------------------------
// Feeds
// ---------------------------------------------------------------------------

/// This computer's folder in the ZCode release tree, e.g. linux-x64.
fn feed_key(host: &Host, vars: &dyn Vars) -> Result<String, UpdateFailure> {
    if host.platform == Platform::Windows {
        let arm = vars
            .get("PROCESSOR_ARCHITECTURE")
            .is_some_and(|value| value.to_uppercase() == "ARM64");
        return Ok(if arm { "windows-arm64" } else { "windows-x64" }.into());
    }
    let architecture = match host.arch {
        "arm64" | "aarch64" => "arm64",
        "x86_64" | "amd64" => "x64",
        _ => return Err(UpdateFailure::new("unsupported")),
    };
    Ok(format!(
        "{}-{architecture}",
        if host.platform == Platform::Mac {
            "macos"
        } else {
            "linux"
        }
    ))
}

/// A fixed publisher lookup, with a hard deadline: HTTP 200 at a final URL starting with
/// `prefix`, at most `limit` bytes, 20 seconds at most.
async fn read_bounded(
    cx: &Context,
    url: &str,
    prefix: &str,
    limit: u64,
    deadline: Option<Instant>,
) -> Result<Vec<u8>, UpdateFailure> {
    // Python `min(deadline if deadline is not None else inf, monotonic() + 20)`.
    let cap = Instant::now() + Duration::from_secs(20);
    let end = deadline.map_or(cap, |deadline| deadline.min(cap));
    if end.saturating_duration_since(Instant::now()).is_zero() {
        return Err(UpdateFailure::new("timeout"));
    }
    let remaining = end.saturating_duration_since(Instant::now());
    let body = cx.system.fetch(url, prefix, limit, remaining).await?;
    if Instant::now() >= end {
        return Err(UpdateFailure::new("timeout"));
    }
    Ok(body)
}

/// The newest version the official page lists for this platform, compared numerically.
/// A page/connection failure is retried; an old installed version is never called
/// current when every lookup fails.
async fn latest_zcode(
    cx: &Context,
    services: &dyn Services,
    key: &str,
    deadline: Instant,
) -> Result<String, UpdateFailure> {
    let end = deadline.min(Instant::now() + Duration::from_secs(60));
    for attempt in 0..PAGE_ATTEMPTS {
        let listed =
            match read_bounded(cx, PAGE, "https://zcode.z.ai/", PAGE_LIMIT, Some(end)).await {
                Err(error) => Err(error),
                Ok(body) => {
                    let pattern = regex::bytes::Regex::new(&format!(
                        r"releases/([0-9]{{1,6}}\.[0-9]{{1,6}}\.[0-9]{{1,6}})/{}/latest\.yml",
                        regex::escape(key)
                    ))
                    .map_err(|_| UpdateFailure::failed())?;
                    let mut best: Option<String> = None;
                    for found in pattern.captures_iter(&body) {
                        let item = String::from_utf8_lossy(&found[1]).into_owned();
                        if best
                            .as_deref()
                            .is_none_or(|kept| version_tuple(&item) > version_tuple(kept))
                        {
                            best = Some(item);
                        }
                    }
                    best.ok_or_else(UpdateFailure::failed)
                }
            };
        match listed {
            Ok(newest) => return Ok(newest),
            Err(error) if attempt + 1 == PAGE_ATTEMPTS => {
                return Err(UpdateFailure::new(if error.code == "timeout" {
                    "timeout"
                } else {
                    "zcode_release_unavailable"
                }));
            }
            Err(_) => {
                let remaining = end.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(UpdateFailure::new("timeout"));
                }
                services
                    .pause(Duration::from_secs(u64::from(attempt) + 1).min(remaining))
                    .await;
            }
        }
    }
    Err(UpdateFailure::new("zcode_release_unavailable"))
}

/// npm's latest version of one adapter, or `None` when the registry does not answer clearly.
async fn registry_version(cx: &Context, adapter: Adapter) -> Option<String> {
    let body = read_bounded(
        cx,
        REGISTRY[adapter],
        "https://registry.npmjs.org/",
        1024 * 1024,
        None,
    )
    .await
    .ok()?;
    let value: Value = serde_json::from_slice(&body).ok()?;
    (value["name"] == PACKAGES[adapter].1)
        .then(|| part_version(value["version"].as_str()))
        .flatten()
}

/// The exact release asset, checked against the size and sha512 in that release's own latest.yml.
async fn verified_package(
    cx: &Context,
    key: &str,
    version: &str,
    folder: &Path,
) -> Result<PathBuf, UpdateFailure> {
    let (system, architecture) = key.split_once('-').ok_or_else(UpdateFailure::failed)?;
    let asset = format!(
        "ZCode-{version}{}",
        if system == "macos" {
            format!("-mac-{architecture}.zip")
        } else {
            format!("-win-{architecture}.exe")
        }
    );
    let base = format!("{RELEASES}{version}/{key}/");
    let (metadata, package) = (folder.join("latest.yml"), folder.join(&asset));
    cx.system
        .download(
            &format!("{base}latest.yml"),
            &metadata,
            65_536,
            Duration::from_secs(30),
        )
        .await?;
    let text = read_bytes(&metadata, 65_536).ok_or_else(UpdateFailure::failed)?;
    let text = String::from_utf8(text).map_err(|_| UpdateFailure::failed())?;
    let (digest, size) = shared::manifest_asset(&text, version, &asset)?;
    cx.system
        .download(
            &format!("{base}{asset}"),
            &package,
            size.unwrap_or(MAX_PACKAGE).min(MAX_PACKAGE),
            Duration::from_secs(shared::budget(cx, 600)?),
        )
        .await?;
    let sha = |path: &Path| -> Option<Vec<u8>> {
        let mut file = std::fs::File::open(path).ok()?;
        let mut hash = sha2::Sha512::new();
        let mut buffer = [0_u8; 256 * 1024];
        loop {
            let read = file.read(&mut buffer).ok()?;
            if read == 0 {
                return Some(hash.finalize().to_vec());
            }
            hash.update(&buffer[..read]);
        }
    };
    let actual = sha(&package).ok_or_else(|| UpdateFailure::new("signature_failed"))?;
    let wrong_size = size
        .is_some_and(|size| std::fs::metadata(&package).map_or(true, |info| info.len() != size));
    if actual != digest || wrong_size {
        return Err(UpdateFailure::new("signature_failed"));
    }
    Ok(package)
}

// ---------------------------------------------------------------------------
// Processes
// ---------------------------------------------------------------------------

async fn table(cx: &Context) -> Result<ProcessTable, UpdateFailure> {
    processes::scan(cx).await
}

/// The Mac process table carries no argv: read it for node and ZCode processes only.
async fn with_arguments(cx: &Context, services: &dyn Services, table: &mut ProcessTable) {
    if cx.platform != Platform::Mac {
        return;
    }
    for item in &mut table.items {
        let name = Path::new(&item.exe)
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or_default();
        let session_or_app =
            matches!(name, "node" | "zcode-cli") || path_text(&item.exe).contains("/zcode.app/");
        if item.args.is_empty() && session_or_app {
            item.args = services.arguments(cx, item.pid).await;
        }
    }
}

/// T3's ZCode sessions: the zcode-acp-server adapter, its zcode-cli child and ZCode
/// started as a CLI.
fn t3_sessions(items: &[Process]) -> Vec<Process> {
    items
        .iter()
        .filter(|item| {
            let args: Vec<String> = item.args.iter().take(16).map(path_text).collect();
            if args.is_empty() {
                return false;
            }
            // zcode-cli rewrites its argv: only that name shows, no path names the app.
            let head = posix_name(args[0].split(' ').next().unwrap_or_default());
            matches!(head, "zcode-cli" | "zcode-cli.exe")
                || args
                    .iter()
                    .any(|arg| SESSION_MARKERS.iter().any(|marker| arg.ends_with(marker)))
        })
        .cloned()
        .collect()
}

/// The adapter packages one process belongs to: its executable or early arguments inside a
/// package folder (any letter case, native subfolders such as native/x86_64-pc-windows-msvc
/// too), or a name or session marker.
fn adapter_owners(item: &Process, roots: &[Vec<PathBuf>; PARTS]) -> Busy {
    let texts: Vec<String> = std::iter::once(&item.exe)
        .chain(item.args.iter().take(16))
        .map(path_text)
        .collect();
    let mut owners = [false; PARTS];
    for adapter in 0..PARTS {
        owners[adapter] = roots[adapter].iter().any(|folder| {
            let package = format!(
                "{}/",
                path_text(folder.join(PACKAGES[adapter].1).to_string_lossy())
            );
            texts.iter().any(|text| text.starts_with(&package))
        });
    }
    let arguments = &texts[1..];
    if arguments
        .iter()
        .any(|text| ADAPTER_NAMES.contains(&posix_name(text)))
    {
        owners[0] = true;
    }
    if arguments
        .iter()
        .any(|text| text.ends_with(SESSION_MARKERS[0]))
    {
        owners[1] = true;
    }
    owners
}

/// The adapter packages running processes use; npm cannot replace their files while they do.
fn busy_packages(items: &[Process], owners: &[Vec<Owner>; PARTS]) -> Busy {
    let roots: [Vec<PathBuf>; PARTS] = std::array::from_fn(|adapter| {
        owners[adapter]
            .iter()
            .map(|owner| owner.root.clone())
            .collect()
    });
    let mut busy = [false; PARTS];
    for item in items {
        let found = adapter_owners(item, &roots);
        for adapter in 0..PARTS {
            busy[adapter] |= found[adapter];
        }
    }
    busy
}

async fn busy_now(cx: &Context, layout: &Layout) -> Result<Busy, UpdateFailure> {
    Ok(busy_packages(&table(cx).await?.items, &layout.owners))
}

/// Whether npm failed on files this package holds open: npm's error lines, not its
/// warnings, carry EPERM or EBUSY and name the package folder. A rollback that could not
/// remove a folder is only a warning.
fn locked_here(output: &str, adapter: Adapter) -> bool {
    let text = output.replace("\\\\", "\\").replace('\\', "/");
    let errors = text
        .lines()
        .filter(|line| line.starts_with("npm error"))
        .collect::<Vec<_>>()
        .join("\n");
    if !LOCKED.is_match(&errors) {
        return false;
    }
    let needle = format!("node_modules/{}", PACKAGES[adapter].1);
    let lower = errors.to_lowercase();
    lower.match_indices(&needle).any(|(at, found)| {
        !lower[at + found.len()..]
            .chars()
            .next()
            .is_some_and(|next| next.is_alphanumeric() || matches!(next, '_' | '.' | '@' | '-'))
    })
}

/// Processes executing from folder or naming it, which t3-acp-update's swap waits for.
fn runs_from(items: &[Process], folder: &Path) -> Vec<Process> {
    let prefixes = [real(folder), folder.to_path_buf()]
        .map(|folder| format!("{}/", path_text(folder.to_string_lossy())));
    items
        .iter()
        .filter(|item| {
            std::iter::once(&item.exe)
                .chain(item.args.iter().take(16))
                .map(path_text)
                .any(|text| prefixes.iter().any(|prefix| text.contains(prefix)))
        })
        .cloned()
        .collect()
}

/// ZCode's own GUI: its bundle's processes that are neither a CLI session nor a session's child.
fn gui_processes(
    cx: &Context,
    install: &Installed,
    table: &ProcessTable,
    sessions: &[Process],
) -> Vec<Process> {
    let excluded = processes::descendants(
        &sessions.iter().map(|item| item.pid).collect(),
        &table.items,
    );
    processes::family(&cx.host, install, table, true)
        .into_iter()
        .filter(|item| !excluded.contains(&item.pid))
        .collect()
}

/// `quit_first` while ZCode's GUI runs (the Mac never quits it), `in_use` while T3 runs a ZCode session.
async fn mac_blocker(
    cx: &Context,
    services: &dyn Services,
    install: &Installed,
) -> Result<(), UpdateFailure> {
    let mut table = table(cx).await?;
    with_arguments(cx, services, &mut table).await;
    let sessions = t3_sessions(&table.items);
    if !gui_processes(cx, install, &table, &sessions).is_empty() {
        return Err(UpdateFailure::new("quit_first"));
    }
    if !sessions.is_empty() {
        return Err(UpdateFailure::new("in_use"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Installers
// ---------------------------------------------------------------------------

/// One package's own global install: no other package rides along in the same npm run.
/// `version` is the exact release the public registry named.
fn npm_argv(
    node: &Path,
    cli: &Path,
    prefix: &Path,
    adapter: Adapter,
    shell: Option<&Path>,
    version: &str,
) -> Vec<OsString> {
    let mut argv: Vec<OsString> = [node, cli]
        .map(|path| plain(path).into_os_string())
        .to_vec();
    argv.extend(["install", "--global", "--prefix"].map(OsString::from));
    argv.push(plain(prefix).into_os_string());
    if let Some(shell) = shell {
        argv.extend(["--script-shell".into(), plain(shell).into_os_string()]);
    }
    // npm 11 skips a package's install scripts unless the install allows them by name; the
    // adapters' postinstall is part of their install.
    argv.push(format!("--allow-scripts={}", PACKAGES[adapter].1).into());
    argv.push(format!("{}@{version}", PACKAGES[adapter].1).into());
    argv
}

/// A Windows verbatim path written the plain way, as `dunce::simplified` does: `\\?\C:\..`
/// becomes `C:\..` and `\\?\UNC\server\share\..` becomes `\\server\share\..`. npm 11
/// cannot install into a verbatim `--prefix` (it exits 1 with "Maximum call stack size
/// exceeded"). Any other path, and a verbatim one with no plain spelling (a volume or device
/// path, or one past the 260-character limit), is returned unchanged.
fn plain(path: &Path) -> PathBuf {
    const MAX_PATH: usize = 260;
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        if !rest.is_empty() && rest.len() + 2 < MAX_PATH {
            return PathBuf::from(format!(r"\\{rest}"));
        }
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        let bytes = rest.as_bytes();
        if bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'\\'
            && rest.len() < MAX_PATH
        {
            return PathBuf::from(rest);
        }
    }
    path.to_path_buf()
}

/// Git for Windows' bash.exe, the POSIX shell the adapters' postinstall scripts are
/// written for; `None` when absent. Only Git's own bin folder counts, never the WSL
/// launcher in System32.
fn git_bash(host: &Host, vars: &dyn Vars) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(|name| vars.get(name))
        .map(|folder| PathBuf::from(folder).join("Git/bin/bash.exe"))
        .collect();
    candidates.push(host.local.join("Programs/Git/bin/bash.exe"));
    if let Some(home) = host.which("git").and_then(|git| {
        Path::new(&git)
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
    }) {
        candidates.push(home.join("bin/bash.exe"));
    }
    let named = |path: &Path, name: &str| {
        path.file_name()
            .is_some_and(|found| found.to_string_lossy().to_lowercase() == name)
    };
    candidates.into_iter().find(|candidate| {
        candidate.is_file()
            && candidate
                .parent()
                .is_some_and(|parent| named(parent, "bin"))
            && candidate
                .ancestors()
                .nth(2)
                .is_some_and(|home| named(home, "git"))
    })
}

/// The pnpm entry corepack itself would run, from its cache; the version it last ran
/// from lastKnownGood.json wins over the newest cached one.
fn corepack_pnpm(host: &Host, vars: &dyn Vars) -> Option<PathBuf> {
    let base = match vars.get("COREPACK_HOME") {
        Some(home) => PathBuf::from(home),
        None => {
            let base = match vars
                .get("XDG_CACHE_HOME")
                .or_else(|| vars.get("LOCALAPPDATA"))
            {
                Some(base) => PathBuf::from(base),
                None if host.platform == Platform::Windows => host.home.join("AppData/Local"),
                None => host.home.join(".cache"),
            };
            base.join("node/corepack")
        }
    };
    let target = base.join("v1/pnpm");
    let known = read_json(&base.join("lastKnownGood.json"), 1024 * 1024);
    if let Some(version) = known.as_ref().and_then(|known| known["pnpm"].as_str()) {
        let entry = target.join(version).join("bin/pnpm.cjs");
        if entry.is_file() {
            return Some(entry);
        }
    }
    let versions = std::fs::read_dir(&target).ok()?;
    versions
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|entry| entry.is_dir() && entry.join("bin/pnpm.cjs").is_file())
        .max_by_key(|entry| {
            entry
                .file_name()
                .map(|name| version_tuple(&name.to_string_lossy()))
                .unwrap_or_default()
        })
        .map(|entry| entry.join("bin/pnpm.cjs"))
}

/// pnpm itself, never through cmd.exe: on Windows a pnpm.cmd shim is replaced by node and
/// pnpm's own pnpm.cjs, as npm runs.
fn pnpm_argv(host: &Host, vars: &dyn Vars) -> Option<Vec<String>> {
    let located = host.which("pnpm")?;
    let text = |path: &Path| path.to_string_lossy().into_owned();
    if host.platform == Platform::Windows {
        let suffix = located
            .extension()
            .and_then(OsStr::to_str)
            .unwrap_or_default()
            .to_lowercase();
        if matches!(suffix.as_str(), "cmd" | "bat" | "ps1") {
            let node = host.which("node");
            for name in [
                "node_modules/pnpm/bin/pnpm.cjs",
                "node_modules/pnpm/bin/pnpm.js",
            ] {
                let cli = located.parent()?.join(name);
                if let Some(node) = &node
                    && cli.is_file()
                {
                    return Some(vec![text(node), text(&cli)]);
                }
            }
            let bundled = node.as_ref().and_then(|_| corepack_pnpm(host, vars));
            return Some(vec![text(&node?), text(&bundled?)]);
        }
    }
    Some(vec![text(&located)])
}

/// `(node, npm-cli.js)` for an npm owner: T3's own node when its config names one, else
/// the prefix's pair.
fn npm_pair(host: &Host, owner: &Owner) -> Option<(PathBuf, PathBuf)> {
    if let Some(configured) = &owner.node {
        let node = real(configured);
        let home = if host.platform == Platform::Windows {
            node.parent().map(Path::to_path_buf)
        } else {
            node.parent().and_then(Path::parent).map(Path::to_path_buf)
        };
        if let Some(home) = home {
            let clis: Vec<PathBuf> = if host.platform == Platform::Windows {
                vec![home.join("node_modules/npm/bin/npm-cli.js")]
            } else {
                vec![
                    home.join("lib/node_modules/npm/bin/npm-cli.js"),
                    home.join("libexec/lib/node_modules/npm/bin/npm-cli.js"),
                ]
            };
            for cli in clis {
                if node.is_file() && cli.is_file() {
                    return Some((node.clone(), cli));
                }
            }
        }
    }
    shared::resolve_npm(host, &owner.prefix)
}

/// One planned install step (Python `install_command`).
struct Planned {
    argv: Vec<OsString>,
    extra: Vec<(String, String)>,
    /// Only its first word reaches a row (the tool id).
    label: &'static str,
}

/// The install bound to its owner; `None` when that manager is not here.
fn install_command(
    host: &Host,
    vars: &dyn Vars,
    owner: &Owner,
    adapter: Adapter,
    shell: Option<&Path>,
    version: &str,
) -> Option<Planned> {
    let package = format!("{}@{version}", PACKAGES[adapter].1);
    match owner.manager {
        "npm" => {
            let (node, cli) = npm_pair(host, owner)?;
            Some(Planned {
                argv: npm_argv(&node, &cli, &owner.prefix, adapter, shell, version),
                extra: Vec::new(),
                label: "npm install --global --prefix",
            })
        }
        "pnpm" => {
            let mut argv: Vec<OsString> =
                pnpm_argv(host, vars)?.into_iter().map(Into::into).collect();
            argv.extend(["add", "--global", "--global-dir"].map(OsString::from));
            argv.push(plain(&owner.prefix).into_os_string());
            argv.push(package.into());
            // pnpm refuses a global install without its bin folder (PNPM_HOME) on PATH.
            let bin_dir = plain(owner.bin_dir.as_deref().unwrap_or(&owner.prefix))
                .to_string_lossy()
                .into_owned();
            let path = format!(
                "{bin_dir}{}{}",
                if host.platform == Platform::Windows {
                    ';'
                } else {
                    ':'
                },
                vars.get("PATH").unwrap_or_default()
            );
            Some(Planned {
                argv,
                extra: vec![("PNPM_HOME".into(), bin_dir), ("PATH".into(), path)],
                label: "pnpm add --global --global-dir",
            })
        }
        "bun" => {
            let home = host.home.join(".bun/bin/bun");
            let bun = host
                .which("bun")
                .or_else(|| home.is_file().then_some(home))?;
            let mut argv: Vec<OsString> =
                vec![bun.as_os_str().to_owned(), "add".into(), "--global".into()];
            argv.push(package.into());
            Some(Planned {
                argv,
                extra: vec![(
                    "BUN_INSTALL_GLOBAL_DIR".into(),
                    plain(&owner.prefix).to_string_lossy().into_owned(),
                )],
                label: "bun add --global",
            })
        }
        _ => None,
    }
}

/// Whether this account can write the owner's folders, by creating and removing a file
/// there: os.access cannot see a Windows ACL.
fn writable(owner: &Owner, platform: Platform) -> bool {
    let mut folders = vec![owner.root.clone()];
    if platform != Platform::Windows && owner.manager == "npm" {
        folders.push(owner.prefix.join("bin"));
    }
    folders.into_iter().all(|folder| {
        if !folder.exists() {
            return true;
        }
        tempfile::Builder::new()
            .prefix(".aac-write-check-")
            .tempfile_in(folder)
            .is_ok()
    })
}

// ---------------------------------------------------------------------------
// Ubuntu's oneshot unit
// ---------------------------------------------------------------------------

/// Mutate or insert this job's entry in [`UNIT_RUNS`], dropping entries of finished jobs.
fn with_unit_run<T>(shared: &Arc<Shared>, change: impl FnOnce(&mut UnitRun) -> T) -> T {
    let mut runs = UNIT_RUNS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    runs.retain(|(job, _)| job.strong_count() > 0);
    if let Some((_, entry)) = runs
        .iter_mut()
        .find(|(job, _)| std::ptr::eq(job.as_ptr(), Arc::as_ptr(shared)))
    {
        return change(entry);
    }
    let mut entry = UnitRun::default();
    let result = change(&mut entry);
    runs.push((Arc::downgrade(shared), entry));
    result
}

/// Record this job's finished unit outcome in [`UNIT_RUNS`].
fn set_unit_run(shared: &Arc<Shared>, code: Option<&'static str>) {
    with_unit_run(shared, |entry| entry.code = Some(code));
}

/// Record pre-unit snapshots before the first row in a job starts `t3-acp-update.service`.
fn snapshot_before_unit(
    shared: &Arc<Shared>,
    adapters: Option<Layout>,
    zcode_version: Option<Option<String>>,
) {
    with_unit_run(shared, |entry| {
        if entry.adapters_before.is_none()
            && let Some(layout) = adapters
        {
            entry.adapters_before = Some(layout);
        }
        if entry.zcode_before.is_none()
            && let Some(version) = zcode_version
        {
            entry.zcode_before = Some(version);
        }
    });
}

fn unit_adapters_before(shared: &Arc<Shared>) -> Option<Layout> {
    let runs = UNIT_RUNS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    runs.iter()
        .find(|(job, _)| std::ptr::eq(job.as_ptr(), Arc::as_ptr(shared)))
        .and_then(|(_, entry)| entry.adapters_before.clone())
}

fn unit_zcode_before(shared: &Arc<Shared>) -> Option<Option<String>> {
    let runs = UNIT_RUNS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    runs.iter()
        .find(|(job, _)| std::ptr::eq(job.as_ptr(), Arc::as_ptr(shared)))
        .and_then(|(_, entry)| entry.zcode_before.clone())
}

/// Python `UBUNTU_RUN["code"]`: `None` until this job's one unit run finished; then its
/// failure code, or `None` for a run that succeeded.
fn unit_ran(shared: &Arc<Shared>) -> Option<Option<&'static str>> {
    let runs = UNIT_RUNS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    runs.iter()
        .find(|(job, _)| std::ptr::eq(job.as_ptr(), Arc::as_ptr(shared)))
        .and_then(|(_, entry)| entry.code)
}

/// Start t3-acp-update and wait for its run, once per job; its failure code or `None`.
/// At the timeout only this wait stops: the unit finishes on its own, under its own lock
/// and TimeoutStartSec, so the shared npm resources stay reserved until a terminal
/// state is verified.
async fn run_unit(cx: &Context) -> Result<Option<&'static str>, UpdateFailure> {
    if let Some(code) = unit_ran(&cx.shared) {
        return Ok(code);
    }
    let seconds = shared::budget(cx, UNIT_SECONDS)?;
    let spec = CommandSpec::new(["/usr/bin/systemctl", "--user", "start", shared::ACP_UNIT])
        .timeout(Duration::from_secs(seconds));
    let code = match cx.run(&spec).await {
        Ok(_) => None,
        Err(error) => {
            // Stopping systemctl's wait does not stop the oneshot's installer: an
            // unreadable state remains busy.
            cx.shared
                .adapter_unit_unsettled
                .store(true, Ordering::SeqCst);
            Some(error.code)
        }
    };
    set_unit_run(&cx.shared, code);
    Ok(code)
}

/// An interrupted wait never grants another installer ownership of the prefix: while a
/// timed-out wait is unsettled, npm and T3 work is `busy` until the unit finished.
pub(super) async fn adapter_unit_busy(cx: &Context) -> bool {
    if !cx.shared.adapter_unit_unsettled.load(Ordering::SeqCst) {
        return false;
    }
    let spec = CommandSpec::new([
        "/usr/bin/systemctl",
        "--user",
        "show",
        shared::ACP_UNIT,
        "--property=SubState",
        "--value",
    ])
    .timeout(Duration::from_secs(15))
    .capture();
    let settled = cx
        .run(&spec)
        .await
        .is_ok_and(|output| matches!(output.stdout.trim(), "dead" | "failed" | "exited"));
    if settled {
        cx.shared
            .adapter_unit_unsettled
            .store(false, Ordering::SeqCst);
    }
    !settled
}

// ---------------------------------------------------------------------------
// ZCode on each platform
// ---------------------------------------------------------------------------

fn current(install: &Installed, before: &str) -> AppRow {
    AppRow::of(
        install,
        Status::Current,
        Some(before),
        Some(before),
        "current",
    )
}

async fn zcode_ubuntu(
    cx: &Context,
    services: &dyn Services,
    vars: &dyn Vars,
    install: &Installed,
    before: &str,
    state: &mut Attempt,
) -> Result<AppRow, UpdateFailure> {
    let newest = latest_zcode(cx, services, &feed_key(&cx.host, vars)?, cx.deadline).await?;
    if version_tuple(&newest) <= version_tuple(before) {
        return Ok(current(install, before));
    }
    if !shared::acp_unit_present(&cx.host) {
        return Err(UpdateFailure::new("unsupported"));
    }
    if !t3_sessions(&table(cx).await?.items).is_empty() {
        return Err(UpdateFailure::new("in_use"));
    }
    // The shared unit also touches both adapters. Do not let this row lower a sibling.
    let modules = cx.host.home.join(".local/lib/node_modules");
    let siblings: Vec<(Adapter, Option<String>)> = (0..PARTS)
        .filter(|adapter| manifest(&modules, *adapter).exists())
        .map(|adapter| (adapter, package_version(&modules, adapter)))
        .collect();
    let mut offers = Vec::new();
    for (adapter, _) in &siblings {
        offers.push((*adapter, registry_version(cx, *adapter).await));
    }
    if offers.iter().any(|(_, offer)| offer.is_none()) {
        // The registry did not answer; that is not a downgrade risk, and the adapters
        // row in this job says the same word.
        return Err(UpdateFailure::new("registry_unreadable"));
    }
    let lowering = |index: usize| {
        let (_, old) = &siblings[index];
        let Some(old) = old else {
            return true;
        };
        let offer = offers[index].1.as_deref().unwrap_or_default();
        order(offer, old).is_lt()
    };
    if (0..siblings.len()).any(lowering) {
        return Ok(AppRow::of(
            install,
            Status::Skipped,
            Some(before),
            Some(before),
            "shared_updater_unsafe",
        ));
    }
    if unit_ran(&cx.shared).is_none() {
        snapshot_before_unit(
            &cx.shared,
            Some(discover_owners(&cx.host, vars)),
            Some(Some(before.to_owned())),
        );
    }
    cx.phase("updating");
    let code = run_unit(cx).await?;
    state.attempted = true;
    let after = installed_version(Platform::Ubuntu, &install.path);
    for (adapter, old) in &siblings {
        let Some(latest) = package_version(&modules, *adapter) else {
            return Err(UpdateFailure::new("version_unknown"));
        };
        if order(&latest, old.as_deref().unwrap_or_default()).is_lt() {
            return Err(UpdateFailure::new("version_went_down"));
        }
    }
    if let Some(after) = &after
        && version_tuple(after) < version_tuple(before)
    {
        return Err(UpdateFailure::new("version_went_down"));
    }
    if let Some(after) = after
        .as_ref()
        .filter(|after| version_tuple(after) > version_tuple(before))
    {
        return Ok(AppRow::of(
            install,
            Status::Updated,
            Some(before),
            Some(after),
            "updated",
        )
        .attempted(0));
    }
    if after.is_none() {
        // The stamp and the asar can no longer be read.
        return Err(UpdateFailure::new("version_unknown"));
    }
    // t3-acp-update holds its swap while anything runs from the app folder (the GUI or a session).
    if !runs_from(&table(cx).await?.items, &install.path).is_empty() {
        return Err(UpdateFailure::new("in_use"));
    }
    Err(UpdateFailure::new(code.unwrap_or("update_failed")))
}

/// Whether this account can create files in `path` (Python `os.access(path, os.W_OK)`).
/// A missing folder is not writable.
fn writable_folder(path: &Path) -> bool {
    path.is_dir()
        && tempfile::Builder::new()
            .prefix(".aac-write-check-")
            .tempfile_in(path)
            .is_ok()
}

fn rename(from: &Path, to: &Path) -> Result<(), UpdateFailure> {
    std::fs::rename(from, to).map_err(|_| UpdateFailure::failed())
}

/// Python `verify_bundle`: the signature checks and the bundle's own version.
async fn verify_bundle(cx: &Context, bundle: &Path, version: &str) -> Result<(), UpdateFailure> {
    shared::verify_mac_signature(cx, bundle, MAC_ID, MAC_TEAM).await?;
    if mac_version(bundle).as_deref() != Some(version) {
        return Err(UpdateFailure::new("signature_failed"));
    }
    Ok(())
}

async fn zcode_mac(
    cx: &Context,
    services: &dyn Services,
    vars: &dyn Vars,
    install: &Installed,
    before: &str,
    state: &mut Attempt,
) -> Result<AppRow, UpdateFailure> {
    let key = feed_key(&cx.host, vars)?;
    let newest = latest_zcode(cx, services, &key, cx.deadline).await?;
    if version_tuple(&newest) <= version_tuple(before) {
        return Ok(current(install, before));
    }
    // Answered before any download: the Mac never quits ZCode and a T3 session is never stopped.
    mac_blocker(cx, services, install).await?;
    if !install.path.parent().is_some_and(writable_folder) {
        return Err(UpdateFailure::new("unsupported"));
    }
    let temporary = shared::private_temporary(cx)?;
    cx.phase("downloading");
    let package = verified_package(cx, &key, &newest, temporary.path()).await?;
    cx.phase("updating");
    shared::check_mac_archive(&package)?;
    let extracted = temporary.path().join("extracted");
    let extract = [
        OsString::from("/usr/bin/ditto"),
        "-x".into(),
        "-k".into(),
        package.as_os_str().to_owned(),
        extracted.as_os_str().to_owned(),
    ];
    let seconds = Duration::from_secs(shared::budget(cx, 120)?);
    cx.run(&CommandSpec::new(extract).timeout(seconds)).await?;
    let staged = extracted.join(MAC_NAME);
    verify_bundle(cx, &staged, &newest).await?;
    swap_mac(cx, services, install, &staged, &newest, state).await
}

/// T3's staged adjacent copy, atomic rename and rollback; ZCode was not running and is
/// not opened.
async fn swap_mac(
    cx: &Context,
    services: &dyn Services,
    install: &Installed,
    staged: &Path,
    version: &str,
    state: &mut Attempt,
) -> Result<AppRow, UpdateFailure> {
    let backup = install.path.with_file_name(format!(
        ".aac-zcode-rollback-{}.app",
        processes::unique_suffix()
    ));
    let adjacent = install.path.with_file_name(format!(
        ".aac-zcode-stage-{}.app",
        processes::unique_suffix()
    ));
    let swap = Swap {
        staged,
        adjacent: &adjacent,
        backup: &backup,
    };
    let mut moved = false;
    let outcome = swap_steps(cx, services, install, &swap, version, state, &mut moved).await;
    let _ = std::fs::remove_dir_all(&adjacent);
    match outcome {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(&backup);
        }
        Err(error) => {
            if moved {
                // Put the previous bundle back.
                let _ = std::fs::remove_dir_all(&install.path);
                let _ = rename(&backup, &install.path);
            }
            return Err(error);
        }
    }
    let mut row = AppRow::of(
        install,
        Status::Updated,
        install.version.as_deref(),
        Some(version),
        "updated",
    );
    row.update_attempted = true;
    row.forced_stops = Some(0);
    Ok(row)
}

/// The paths one Mac swap works on.
struct Swap<'a> {
    staged: &'a Path,
    adjacent: &'a Path,
    backup: &'a Path,
}

/// The steps of [`swap_mac`] in order; `moved` is set once the installed bundle was
/// renamed aside.
async fn swap_steps(
    cx: &Context,
    services: &dyn Services,
    install: &Installed,
    swap: &Swap<'_>,
    version: &str,
    state: &mut Attempt,
    moved: &mut bool,
) -> Result<(), UpdateFailure> {
    let mut copy = vec![OsString::from("/usr/bin/ditto")];
    copy.extend([swap.staged, swap.adjacent].map(|path| path.as_os_str().to_owned()));
    let seconds = Duration::from_secs(shared::budget(cx, 120)?);
    cx.run(&CommandSpec::new(copy).timeout(seconds)).await?;
    verify_bundle(cx, swap.adjacent, version).await?;
    // It may have been opened during the download: never swap under it.
    mac_blocker(cx, services, install).await?;
    state.attempted = true;
    rename(&install.path, swap.backup)?;
    *moved = true;
    rename(swap.adjacent, &install.path)?;
    verify_bundle(cx, &install.path, version).await
}

/// Valid Authenticode on both files and the installer signed by the installed ZCode's
/// signer; prints nothing.
async fn verify_signer(
    cx: &Context,
    package: &Path,
    installed: &Path,
) -> Result<(), UpdateFailure> {
    let script = format!(
        concat!(
            "$ErrorActionPreference='Stop'; $n=Get-AuthenticodeSignature -LiteralPath {}",
            "; $o=Get-AuthenticodeSignature -LiteralPath {}",
            "; if($n.Status -ne 'Valid' -or $o.Status -ne 'Valid' -or -not $n.SignerCertificate",
            " -or -not $o.SignerCertificate -or $n.SignerCertificate.Subject -cne",
            " $o.SignerCertificate.Subject -or -not $o.SignerCertificate.Subject.Contains({})){{exit 1}}"
        ),
        shared::ps_quote(&package.to_string_lossy()),
        shared::ps_quote(&installed.to_string_lossy()),
        shared::ps_quote(WINDOWS_SIGNER)
    );
    shared::powershell(cx, &script, Duration::from_secs(60), None, false)
        .await
        .map(|_| ())
        .map_err(|_| UpdateFailure::new("signature_failed"))
}

/// ZCode's running main instances, each reopened by path later; `quit_first` when one
/// cannot be (another session).
async fn windows_instances(
    cx: &Context,
    services: &dyn Services,
    install: &Installed,
) -> Result<Vec<Process>, UpdateFailure> {
    let contexts = match processes::main_contexts(cx, install).await {
        Ok(contexts) => contexts,
        Err(error) => {
            return Err(UpdateFailure::new(if error.code == "restart_context" {
                "quit_first"
            } else {
                error.code
            }));
        }
    };
    if !contexts.is_empty() {
        let own = services.session();
        if own.is_none() || contexts.iter().any(|item| item.session != own) {
            return Err(UpdateFailure::new("quit_first"));
        }
    }
    Ok(contexts)
}

/// Python `shutil.copytree`: a new folder with the same contents; links are followed.
fn copy_tree(from: &Path, to: &Path) -> Result<(), UpdateFailure> {
    fn copy(from: &Path, to: &Path) -> std::io::Result<()> {
        std::fs::create_dir(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let target = to.join(entry.file_name());
            if entry.path().is_dir() {
                copy(&entry.path(), &target)?;
            } else {
                std::fs::copy(entry.path(), target)?;
            }
        }
        Ok(())
    }
    copy(from, to).map_err(|_| UpdateFailure::failed())
}

async fn zcode_windows(
    cx: &Context,
    services: &dyn Services,
    vars: &dyn Vars,
    install: &Installed,
    before: &str,
    state: &mut Attempt,
) -> Result<AppRow, UpdateFailure> {
    let key = feed_key(&cx.host, vars)?;
    let newest = latest_zcode(cx, services, &key, cx.deadline).await?;
    if version_tuple(&newest) <= version_tuple(before) {
        return Ok(current(install, before));
    }
    if !t3_sessions(&table(cx).await?.items).is_empty() {
        return Err(UpdateFailure::new("in_use"));
    }
    // Read-only: an instance that could not be reopened answers before any download.
    windows_instances(cx, services, install).await?;
    let temporary = shared::private_temporary(cx)?;
    cx.phase("downloading");
    let package = verified_package(cx, &key, &newest, temporary.path()).await?;
    verify_signer(cx, &package, &install.path).await?;
    cx.phase("updating");
    // Captured again right before closing: ZCode or a session may have started meanwhile.
    if !t3_sessions(&table(cx).await?.items).is_empty() {
        return Err(UpdateFailure::new("in_use"));
    }
    let contexts = windows_instances(cx, services, install).await?;
    let backup = temporary.path().join("rollback");
    let replace = Replace {
        package: &package,
        version: &newest,
        backup: &backup,
    };
    replace_windows(cx, services, install, &replace, contexts, state).await
}

/// What one Windows replace works on.
struct Replace<'a> {
    package: &'a Path,
    version: &'a str,
    /// The private copy of the installed folder this row restores on failure.
    backup: &'a Path,
}

/// Close, install and reopen like Windows Codex and Claude, with T3's private rollback
/// copy. A failed install restores the copy, and whatever closed reopens from it by path.
async fn replace_windows(
    cx: &Context,
    services: &dyn Services,
    install: &Installed,
    replace: &Replace<'_>,
    contexts: Vec<Process>,
    state: &mut Attempt,
) -> Result<AppRow, UpdateFailure> {
    let root = install
        .package_root
        .as_deref()
        .ok_or_else(UpdateFailure::failed)?;
    copy_tree(root, replace.backup)?;
    let seconds = shared::budget(cx, 180)?;
    state.attempted = true;
    let (forced, started, outcome) =
        replace_steps(cx, services, install, replace, &contexts, seconds).await;
    if let Err(error) = outcome {
        if started {
            restore_windows(cx, services, install, replace.backup).await;
        }
        // Whatever closed and is no longer running is reopened, from the restored copy.
        let live = processes::live_contexts(cx, &contexts).await;
        let gone: Vec<Process> = contexts
            .iter()
            .filter(|item| {
                !live
                    .iter()
                    .any(|other| processes::same_process(item, other))
            })
            .cloned()
            .collect();
        if !gone.is_empty() {
            let _ = services.restart(cx, install, &gone).await;
        }
        return Err(error);
    }
    let mut row = if contexts.is_empty() {
        AppRow::of(
            install,
            Status::Updated,
            install.version.as_deref(),
            Some(replace.version),
            "updated",
        )
        .attempted(0)
    } else {
        match services.restart(cx, install, &contexts).await {
            Ok(count) => {
                let mut row = AppRow::of(
                    install,
                    Status::Updated,
                    install.version.as_deref(),
                    Some(replace.version),
                    "desktop_reopened",
                )
                .attempted(count);
                row.restart_targets = Some(vec![json!({"kind": "desktop"})]);
                row
            }
            Err(_) => AppRow::of(
                install,
                Status::RestartFailed,
                install.version.as_deref(),
                Some(replace.version),
                "restart_failed",
            )
            .attempted(0),
        }
    };
    row.forced_stops = Some(forced);
    Ok(row)
}

/// The steps of [`replace_windows`]: the count forced, whether the installer was
/// started, and how they ended.
async fn replace_steps(
    cx: &Context,
    services: &dyn Services,
    install: &Installed,
    replace: &Replace<'_>,
    contexts: &[Process],
    seconds: u64,
) -> (u32, bool, Result<(), UpdateFailure>) {
    let mut forced = 0;
    let mut started = false;
    let outcome: Result<(), UpdateFailure> = async {
        if !contexts.is_empty() {
            forced = services.terminate(cx, install, contexts).await?;
        }
        started = true;
        shared::nsis_silent(cx, replace.package, seconds).await?;
        if installed_version(Platform::Windows, &install.path).as_deref() != Some(replace.version) {
            return Err(UpdateFailure::new("version_unknown"));
        }
        Ok(())
    }
    .await;
    (forced, started, outcome)
}

/// Put the previous ZCode back; a copy that cannot be restored stays private for recovery.
async fn restore_windows(
    cx: &Context,
    services: &dyn Services,
    install: &Installed,
    backup: &Path,
) {
    let restored: Result<(), UpdateFailure> = async {
        let contexts = processes::main_contexts(cx, install).await?;
        services.terminate(cx, install, &contexts).await?;
        let root = install
            .package_root
            .as_deref()
            .ok_or_else(UpdateFailure::failed)?;
        std::fs::remove_dir_all(root).map_err(|_| UpdateFailure::failed())?;
        copy_tree(backup, root)
    }
    .await;
    if restored.is_err() {
        let recovery = cx
            .state
            .join(format!("zcode-rollback-{}", processes::unique_suffix()));
        let _ = super::common::private_dir(&cx.state);
        if std::fs::rename(backup, &recovery).is_err() && copy_tree(backup, &recovery).is_ok() {
            let _ = std::fs::remove_dir_all(backup);
        }
    }
}

/// Python `update_zcode`: the platform's flow, and the row for any failure.
async fn zcode_update(
    cx: &Context,
    install: &Installed,
    vars: &dyn Vars,
    services: &dyn Services,
) -> AppRow {
    let before = install.version.clone();
    let mut state = Attempt::default();
    let outcome = match before.as_deref() {
        None => Err(UpdateFailure::new("version_unknown")),
        Some(before) => match cx.platform {
            Platform::Ubuntu => zcode_ubuntu(cx, services, vars, install, before, &mut state).await,
            Platform::Mac => zcode_mac(cx, services, vars, install, before, &mut state).await,
            Platform::Windows => {
                zcode_windows(cx, services, vars, install, before, &mut state).await
            }
        },
    };
    let (status, after, code) = match outcome {
        Ok(row) => return row,
        Err(error) if ACTIONS.contains(&error.code) => {
            (Status::ActionRequired, before.clone(), error.code)
        }
        Err(error) => {
            // Report the bits actually installed after any rollback, never the old snapshot.
            let after = if state.attempted {
                installed_version(cx.platform, &install.path)
            } else {
                before.clone()
            };
            let mixed = error.code != "version_went_down"
                && after.is_some()
                && before.is_some()
                && after != before;
            let status = if mixed {
                Status::RestartFailed
            } else {
                Status::Failed
            };
            let code = if error.code == "download_blocked" {
                "update_failed"
            } else {
                error.code
            };
            (status, after, code)
        }
    };
    let mut row = AppRow::of(install, status, before.as_deref(), after.as_deref(), code);
    row.update_attempted = state.attempted;
    row
}

// ---------------------------------------------------------------------------
// The adapters on each platform
// ---------------------------------------------------------------------------

/// The adapters row: top-level versions stay null, each adapter reports its own in parts.
/// A part in `held` was behind and skipped because it is in use: it keeps its version and
/// carries `inUse`.
#[expect(
    clippy::too_many_arguments,
    reason = "the same inputs as Python's adapters_row"
)]
fn adapters_row(
    cx: &Context,
    vars: &dyn Vars,
    layout: &Layout,
    status: Status,
    after: Option<&Parts>,
    code: &str,
    attempted: bool,
    held: &HashSet<Adapter>,
) -> AppRow {
    let before = owned_parts(layout);
    let versions = after.unwrap_or(&before);
    let parts: Vec<Value> = (0..PARTS)
        .map(|adapter| {
            let mut part = json!({
                "name": PACKAGES[adapter].0,
                "previousVersion": before[adapter],
                "version": versions[adapter],
            });
            if held.contains(&adapter) {
                part["inUse"] = Value::Bool(true);
            }
            part
        })
        .collect();
    let mut row = AppRow::new("t3-acp-adapters", cx.platform, status);
    row.manager = Some("npm".into());
    row.message_code = Some(code.into());
    row.update_attempted = attempted;
    row.parts = Some(Value::Array(parts));
    if (0..PARTS).any(|adapter| !layout.owners[adapter].is_empty()) {
        let owner = adapters_owner(&cx.host, vars, layout, after);
        cx.record(|record| record.owner = Some(owner));
    }
    row
}

/// Whether a copy other than the owners' changed (or appeared) during this row.
fn moved_elsewhere(roots: &[PathBuf], layout: &Layout, adapter: Adapter) -> bool {
    let owners: HashSet<PathBuf> = layout.owners[adapter]
        .iter()
        .map(|owner| real(&owner.root))
        .collect();
    copies_of(roots, adapter)
        .into_iter()
        .any(|(root, version)| {
            if owners.contains(&real(&root)) || version.is_none() {
                return false;
            }
            let before = layout.copies[adapter]
                .iter()
                .rfind(|(copy, _)| real(copy) == real(&root))
                .and_then(|(_, version)| version.clone());
            before != version
        })
}

/// One install pass's inputs (Python `install_owned`'s arguments).
struct InstallJob<'a> {
    plan: &'a [Plan],
    /// A package npm found locked on Windows joins the held parts.
    held: &'a mut HashSet<Adapter>,
    targets: &'a Parts,
    busy: &'a Busy,
}

/// Each behind copy installs in its own run, bound to the owner that holds it. Returns
/// through `failed` the worst code across that adapter's copies: one failing copy never
/// skips its siblings. A package npm reports EBUSY on, or EPERM while an adapter process
/// uses it (Windows), joins `held`; EPERM or EACCES with nothing using it is an
/// unwritable folder.
async fn install_owned(
    cx: &Context,
    vars: &dyn Vars,
    layout: &Layout,
    job: &mut InstallJob<'_>,
    state: &mut Attempt,
    failed: &mut Codes,
) -> Result<(), UpdateFailure> {
    let shell = if cx.platform == Platform::Windows {
        git_bash(&cx.host, vars)
    } else {
        None
    };
    let mut failures: Vec<(Plan, &'static str)> = Vec::new();
    for &step in job.plan {
        let (adapter, index) = step;
        if job.held.contains(&adapter) || failures.iter().any(|(found, _)| *found == step) {
            continue;
        }
        let owner = &layout.owners[adapter][index];
        if !writable(owner, cx.platform) {
            failures.push((step, "prefix_not_writable"));
            cx.record(|record| {
                record.blame(FailureCause::NotWritable, Some("prefix_not_writable"))
            });
            continue;
        }
        let version = job.targets[adapter].as_deref().unwrap_or_default();
        let planned = install_command(&cx.host, vars, owner, adapter, shell.as_deref(), version);
        let Some(planned) = planned else {
            failures.push((step, "unsupported"));
            cx.record(|record| record.note("manager_missing"));
            continue;
        };
        cx.phase("updating");
        state.attempted = true;
        let mut spec = CommandSpec::new(planned.argv).errors();
        spec.timeout = Duration::from_secs(shared::budget(cx, NPM_SECONDS)?);
        spec.label = Some(planned.label);
        if !planned.extra.is_empty() {
            spec.env_mode = EnvMode::Exact;
            spec.env = shared::environment(&planned.extra)
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect();
        }
        match cx.run(&spec).await {
            Ok(_) => {}
            Err(error)
                if cx.platform == Platform::Windows && locked_here(&error.output, adapter) =>
            {
                // npm's output is read here only, never shown in full.
                if job.busy[adapter]
                    || (!DENIED.is_match(&error.output) && error.output.contains("EBUSY"))
                {
                    job.held.insert(adapter);
                } else {
                    failures.push((step, "prefix_not_writable"));
                    cx.record(|record| record.blame(FailureCause::NotWritable, Some("npm_denied")));
                }
            }
            Err(error) => {
                cx.record(|record| record.installer_output(&error.output));
                failures.push((step, error.code));
            }
        }
    }
    for (step, code) in failures {
        // A copy the user must fix never hides a copy that really failed.
        if matches!(failed.get(step.0), None | Some("prefix_not_writable")) {
            failed.set(step.0, code);
        }
    }
    Ok(())
}

/// Each adapter's version in the copies its owners name, read now.
fn parts_now(layout: &Layout) -> Parts {
    std::array::from_fn(|adapter| lowest(&owner_versions(layout, adapter)))
}

/// The row for what really changed in the owners' copies. A package is updated only when
/// every copy T3 runs reached the registry's newest version. `unit` is the one unit run's
/// failure code when the unit ran, which also lets an unchanged package still be in use;
/// a direct install never rescans.
#[expect(
    clippy::too_many_arguments,
    reason = "the same inputs as Python's adapters_settle"
)]
async fn adapters_settle(
    cx: &Context,
    vars: &dyn Vars,
    layout: &Layout,
    behind: &[Adapter],
    targets: &Parts,
    mut failed: Codes,
    held: &mut HashSet<Adapter>,
    attempted: bool,
    unit: UnitCode,
) -> Result<AppRow, UpdateFailure> {
    let after = parts_now(layout);
    let now: [Versions; PARTS] = std::array::from_fn(|adapter| owner_versions(layout, adapter));
    let mut changed: HashSet<Adapter> = HashSet::new();
    // Check all snapshotted owners before success shortcuts, including siblings outside the plan.
    for (adapter, versions) in now.iter().enumerate() {
        let originals = &layout.before[adapter];
        if layout.owners[adapter].is_empty() {
            continue;
        }
        let lowered = originals.iter().zip(versions).any(
            |(old, new)| matches!((old, new), (Some(old), Some(new)) if order(new, old).is_lt()),
        );
        if lowered {
            cx.record(|record| {
                record.blame(FailureCause::InstallerFailed, Some("version_went_down"))
            });
            return Ok(adapters_row(
                cx,
                vars,
                layout,
                Status::Failed,
                Some(&after),
                "version_went_down",
                attempted,
                held,
            ));
        }
        let unreadable = versions.len() != originals.len()
            || originals
                .iter()
                .zip(versions)
                .any(|(old, new)| old.is_some() && new.is_none());
        if unreadable {
            failed.set(adapter, "version_unknown");
        }
    }
    let roots = candidate_roots(&cx.host, vars);
    for &adapter in behind {
        let versions = &now[adapter];
        let reached: Vec<bool> = versions
            .iter()
            .map(|version| {
                version.as_deref().is_some_and(|version| {
                    order(version, targets[adapter].as_deref().unwrap_or_default()).is_ge()
                })
            })
            .collect();
        if !versions.is_empty() && reached.iter().all(|&reached| reached) {
            changed.insert(adapter);
            continue;
        }
        if reached.iter().any(|&reached| reached) {
            cx.record(|record| record.note("owners_partial"));
        }
        if held.contains(&adapter) || failed.has(adapter) {
            continue;
        }
        let moved = layout.before[adapter]
            .iter()
            .zip(versions)
            .any(|(old, new)| old != new);
        if moved {
            failed.set(adapter, "registry_mismatch");
            cx.record(|record| record.blame(FailureCause::InstallerFailed, Some("registry_other")));
        } else if moved_elsewhere(&roots, layout, adapter) {
            failed.set(adapter, "wrong_prefix");
        } else if unit.is_some() && busy_now(cx, layout).await?.iter().any(|&busy| busy) {
            held.insert(adapter);
        } else {
            // It finished, but this package's installed version did not move.
            failed.set(adapter, unit.flatten().unwrap_or("update_failed"));
        }
    }
    held.retain(|adapter| !changed.contains(adapter));
    let row = |status, code| {
        adapters_row(
            cx,
            vars,
            layout,
            status,
            Some(&after),
            code,
            attempted,
            held,
        )
    };
    if failed.values().any(|code| code == "wrong_prefix") {
        cx.record(|record| record.blame(FailureCause::WrongPrefix, Some("owner_unchanged")));
        return Ok(row(Status::Failed, "wrong_prefix"));
    }
    if failed.values().any(|code| code == "registry_mismatch") {
        return Ok(row(Status::Failed, "registry_mismatch"));
    }
    if let Some(code) = failed.values().find(|code| *code != "prefix_not_writable") {
        return Ok(row(Status::Failed, code));
    }
    if !failed.items.is_empty() {
        return Ok(row(Status::ActionRequired, "prefix_not_writable"));
    }
    if !held.is_empty() {
        return Ok(row(Status::ActionRequired, "in_use"));
    }
    Ok(row(Status::Updated, "updated"))
}

/// Whether the owner's t3-acp-update unit installs into the copies T3 runs (npm in ~/.local).
fn unit_owns(host: &Host, layout: &Layout) -> bool {
    let local = real(&host.home.join(".local/lib/node_modules"));
    (0..PARTS).all(|adapter| {
        layout.owners[adapter]
            .iter()
            .all(|owner| owner.manager == "npm" && real(&owner.root) == local)
    })
}

/// Python `adapters_ubuntu`: the owner's unit when it installs into the copies T3 runs,
/// else each behind copy installs directly, bound to its owner.
async fn adapters_ubuntu(
    cx: &Context,
    vars: &dyn Vars,
    layout: &Layout,
    state: &mut Attempt,
    plan: &[Plan],
    behind: &[Adapter],
    targets: &Parts,
) -> Result<AppRow, UpdateFailure> {
    // The unit also installs adapters: never let it lower one a newer registry names.
    let unit_safe = (0..PARTS).all(|adapter| !layout.owners[adapter].is_empty())
        && (0..PARTS).all(|adapter| {
            targets[adapter].as_deref().is_some_and(|target| {
                layout.before[adapter]
                    .iter()
                    .all(|old| order(target, old.as_deref().unwrap_or_default()).is_ge())
            })
        });
    if shared::acp_unit_present(&cx.host) && unit_owns(&cx.host, layout) && unit_safe {
        // The zcode row may already have run t3-acp-update in this job; it never runs twice.
        if unit_ran(&cx.shared).is_none() && busy_now(cx, layout).await?.iter().any(|&busy| busy) {
            return Err(UpdateFailure::new("in_use"));
        }
        let desktop = detect_zcode(cx);
        let unknown = desktop
            .as_ref()
            .is_some_and(|desktop| desktop.version.is_none());
        if unknown {
            return Ok(adapters_row(
                cx,
                vars,
                layout,
                Status::Skipped,
                None,
                "shared_updater_unsafe",
                false,
                &HashSet::new(),
            ));
        }
        if unit_ran(&cx.shared).is_none() {
            snapshot_before_unit(
                &cx.shared,
                Some(layout.clone()),
                desktop.as_ref().map(|desktop| desktop.version.clone()),
            );
        }
        cx.phase("updating");
        let code = run_unit(cx).await?;
        state.attempted = true;
        if let Some(desktop) = &desktop {
            let installed = installed_version(Platform::Ubuntu, &desktop.path);
            let lowered = installed.as_ref().is_some_and(|after| {
                version_tuple(after) < version_tuple(desktop.version.as_deref().unwrap_or_default())
            });
            if installed.is_none() || lowered {
                let after = parts_now(layout);
                let code = if lowered {
                    "version_went_down"
                } else {
                    "version_unknown"
                };
                return Ok(adapters_row(
                    cx,
                    vars,
                    layout,
                    Status::Failed,
                    Some(&after),
                    code,
                    true,
                    &HashSet::new(),
                ));
            }
        }
        let mut held = HashSet::new();
        return adapters_settle(
            cx,
            vars,
            layout,
            behind,
            targets,
            Codes::default(),
            &mut held,
            true,
            Some(code),
        )
        .await;
    }
    // No unit, or T3 runs a copy the unit does not install: install there directly.
    cx.record(|record| record.note("direct_install"));
    let busy = busy_now(cx, layout).await?;
    install_direct(cx, vars, layout, state, plan, behind, targets, &busy).await
}

/// The install pass of a platform that installs each behind copy itself, then the row.
#[expect(
    clippy::too_many_arguments,
    reason = "the same inputs as Python's adapters_ubuntu's direct branch"
)]
async fn install_direct(
    cx: &Context,
    vars: &dyn Vars,
    layout: &Layout,
    state: &mut Attempt,
    plan: &[Plan],
    behind: &[Adapter],
    targets: &Parts,
    busy: &Busy,
) -> Result<AppRow, UpdateFailure> {
    let mut held: HashSet<Adapter> = behind
        .iter()
        .copied()
        .filter(|adapter| busy[*adapter])
        .collect();
    let mut failed = Codes::default();
    let mut job = InstallJob {
        plan,
        held: &mut held,
        targets,
        busy,
    };
    install_owned(cx, vars, layout, &mut job, state, &mut failed).await?;
    adapters_settle(
        cx,
        vars,
        layout,
        behind,
        targets,
        failed,
        &mut held,
        state.attempted,
        None,
    )
    .await
}

/// macOS replaces files under running adapters safely, so nothing is held. The install is
/// bound to the prefix T3's copy lives in: the owner's own script ran plain
/// `npm install -g`, which follows whichever Node is first.
async fn adapters_mac(
    cx: &Context,
    vars: &dyn Vars,
    layout: &Layout,
    state: &mut Attempt,
    plan: &[Plan],
    behind: &[Adapter],
    targets: &Parts,
) -> Result<AppRow, UpdateFailure> {
    install_direct(
        cx,
        vars,
        layout,
        state,
        plan,
        behind,
        targets,
        &[false; PARTS],
    )
    .await
}

/// Each behind copy installs in its own npm run, so none can block or roll back another.
/// A package a running adapter uses is held (in_use), and so is one npm reports EBUSY on.
async fn adapters_windows(
    cx: &Context,
    vars: &dyn Vars,
    layout: &Layout,
    state: &mut Attempt,
    plan: &[Plan],
    behind: &[Adapter],
    targets: &Parts,
) -> Result<AppRow, UpdateFailure> {
    let busy = busy_now(cx, layout).await?;
    install_direct(cx, vars, layout, state, plan, behind, targets, &busy).await
}

/// Python `update_adapters`'s `try` block: the ownership checks, the registry answer and
/// the platform's install. `behind` receives the adapters the plan installs.
async fn adapters_steps(
    cx: &Context,
    vars: &dyn Vars,
    layout: &Layout,
    state: &mut Attempt,
    behind: &mut Vec<Adapter>,
) -> Result<AppRow, UpdateFailure> {
    for owners in &layout.owners {
        for owner in owners {
            if matches!(owner.source, "mixed_ownership" | "pnpm_store_copy") {
                cx.record(|record| record.note(owner.source));
                return Ok(adapters_row(
                    cx,
                    vars,
                    layout,
                    Status::Skipped,
                    None,
                    owner.source,
                    false,
                    &HashSet::new(),
                ));
            }
        }
    }
    let present: Vec<Adapter> = (0..PARTS)
        .filter(|adapter| !layout.owners[*adapter].is_empty())
        .collect();
    let unreadable = present.is_empty()
        || present.iter().any(|adapter| {
            layout.before[*adapter].is_empty()
                || layout.before[*adapter].iter().any(Option::is_none)
        });
    if unreadable {
        return Err(UpdateFailure::new("version_unknown")
            .with_cause(FailureCause::Unknown, Some("adapter_version_unreadable")));
    }
    let mut latest: Parts = [None, None];
    for &adapter in &present {
        latest[adapter] = registry_version(cx, adapter).await;
    }
    if present.iter().any(|adapter| latest[*adapter].is_none()) {
        // Without the registry's answer nothing can be called current, so nothing runs.
        return Err(UpdateFailure::new("update_failed")
            .with_cause(FailureCause::Unknown, Some("registry_unreadable")));
    }
    // Only a copy behind its latest is installed; one already current is never reinstalled.
    let mut plan: Vec<Plan> = Vec::new();
    for &adapter in &present {
        let target = latest[adapter].as_deref().unwrap_or_default();
        for (index, version) in layout.before[adapter].iter().enumerate() {
            if order(target, version.as_deref().unwrap_or_default()).is_gt() {
                plan.push((adapter, index));
            }
        }
    }
    behind.clear();
    for (adapter, _) in &plan {
        if !behind.contains(adapter) {
            behind.push(*adapter);
        }
    }
    if plan.is_empty() {
        return Ok(adapters_row(
            cx,
            vars,
            layout,
            Status::Current,
            None,
            "current",
            false,
            &HashSet::new(),
        ));
    }
    match cx.platform {
        Platform::Ubuntu => adapters_ubuntu(cx, vars, layout, state, &plan, behind, &latest).await,
        Platform::Mac => adapters_mac(cx, vars, layout, state, &plan, behind, &latest).await,
        Platform::Windows => {
            adapters_windows(cx, vars, layout, state, &plan, behind, &latest).await
        }
    }
}

/// Python `update_adapters`: the steps and the row for any failure.
async fn adapters_update(cx: &Context, vars: &dyn Vars) -> AppRow {
    // The contract hands `update` the generic installation only, so the copies are read
    // again here (or taken from this job's pre-unit snapshot when `zcode` already ran
    // `t3-acp-update.service`).
    let layout = if cx.platform == Platform::Ubuntu {
        unit_adapters_before(&cx.shared).unwrap_or_else(|| discover_owners(&cx.host, vars))
    } else {
        discover_owners(&cx.host, vars)
    };
    adapters_update_layout(cx, vars, &layout).await
}

async fn adapters_update_layout(cx: &Context, vars: &dyn Vars, layout: &Layout) -> AppRow {
    let mut state = Attempt::default();
    let mut behind: Vec<Adapter> = Vec::new();
    let error = match adapters_steps(cx, vars, layout, &mut state, &mut behind).await {
        Ok(row) => return row,
        Err(error) => error,
    };
    if error.cause.is_some() || error.reason.is_some() {
        let cause = error.cause.unwrap_or(FailureCause::Unknown);
        cx.record(|record| record.blame(cause, error.reason));
    }
    if ACTIONS.contains(&error.code) {
        // A held part was behind and skipped because it is in use.
        let held: HashSet<Adapter> = if error.code == "in_use" {
            behind.into_iter().collect()
        } else {
            HashSet::new()
        };
        return adapters_row(
            cx,
            vars,
            layout,
            Status::ActionRequired,
            None,
            error.code,
            state.attempted,
            &held,
        );
    }
    let after = state.attempted.then(|| parts_now(layout));
    adapters_row(
        cx,
        vars,
        layout,
        Status::Failed,
        after.as_ref(),
        error.code,
        state.attempted,
        &HashSet::new(),
    )
}

/// ZCode desktop.
pub(super) struct Zcode;

impl AppUpdater for Zcode {
    fn app_id(&self) -> &'static str {
        "zcode"
    }

    fn detect<'a>(&'a self, cx: &'a Context) -> BoxFuture<'a, Detection> {
        Box::pin(async move {
            detect_zcode(cx).map_or(Detection::Absent, |install| {
                Detection::Found(Box::new(install))
            })
        })
    }

    fn update<'a>(
        &'a self,
        cx: &'a Context,
        install: &'a Installed,
    ) -> BoxFuture<'a, Result<AppRow, UpdateFailure>> {
        Box::pin(async move { Ok(zcode_update(cx, install, &Ambient, &NativeServices).await) })
    }
}

/// T3's ACP adapters.
pub(super) struct Adapters;

impl AppUpdater for Adapters {
    fn app_id(&self) -> &'static str {
        "t3-acp-adapters"
    }

    fn detect<'a>(&'a self, cx: &'a Context) -> BoxFuture<'a, Detection> {
        Box::pin(async move {
            detect_adapters(cx, &Ambient).map_or(Detection::Absent, |install| {
                Detection::Found(Box::new(install))
            })
        })
    }

    fn update<'a>(
        &'a self,
        cx: &'a Context,
        _install: &'a Installed,
    ) -> BoxFuture<'a, Result<AppRow, UpdateFailure>> {
        Box::pin(async move { Ok(adapters_update(cx, &Ambient).await) })
    }
}

pub(super) static ZCODE: Zcode = Zcode;
pub(super) static ADAPTERS: Adapters = Adapters;

/// Ports of `app_update_zcode_test.py` and the adapter tests of the fix rounds. Offline
/// fixtures only: nothing real is downloaded, installed, stopped or started, and every
/// path is under the test's own folder.
#[cfg(test)]
mod tests {
    use super::super::{CommandOutput, Diagnostics, Downloaded, Fingerprint, System, attempt};
    use super::*;
    use base64::Engine as _;
    use std::ffi::OsString;
    use std::sync::Mutex;
    use tokio::time::Instant;

    const OLD: &str = "3.14.4";
    const NEW: &str = "3.14.5";
    const UNIT_START: [&str; 4] = [
        "/usr/bin/systemctl",
        "--user",
        "start",
        "t3-acp-update.service",
    ];

    type Answer = Box<dyn FnMut(&CommandSpec) -> Result<CommandOutput, UpdateFailure> + Send>;
    type Lookup = Box<dyn FnMut(&str) -> Result<Vec<u8>, UpdateFailure> + Send>;
    type Serving = Box<dyn FnMut(&str, &Path) -> Result<(), UpdateFailure> + Send>;
    type Table = Arc<Mutex<Vec<Process>>>;
    type RecordedRun = (Vec<OsString>, Duration, Vec<(OsString, OsString)>);

    /// A scripted host: answers commands, the process table, page lookups and downloads,
    /// and records what each lookup was asked for (URL, size cap, URL prefix).
    struct Fixture {
        runs: Mutex<Vec<RecordedRun>>,
        answer: Mutex<Answer>,
        table: Table,
        /// Optional scripted sequence of process tables; once drained, falls back to `table`.
        scans_script: Mutex<std::collections::VecDeque<Vec<Process>>>,
        lookup: Mutex<Lookup>,
        serving: Mutex<Serving>,
        asked: Mutex<Vec<(String, u64, String)>>,
        downloads: Mutex<Vec<(String, u64)>>,
        /// How long every page lookup takes.
        delay: Mutex<Duration>,
        /// How many times the process table was scanned.
        scans: std::sync::atomic::AtomicUsize,
    }

    impl Fixture {
        fn plain(table: Table) -> Arc<Self> {
            Arc::new(Self {
                runs: Mutex::new(Vec::new()),
                answer: Mutex::new(Box::new(|_| Ok(CommandOutput::default()))),
                table,
                scans_script: Mutex::new(std::collections::VecDeque::new()),
                lookup: Mutex::new(Box::new(|_| Err(UpdateFailure::failed()))),
                serving: Mutex::new(Box::new(|_, _| Err(UpdateFailure::failed()))),
                asked: Mutex::new(Vec::new()),
                downloads: Mutex::new(Vec::new()),
                delay: Mutex::new(Duration::ZERO),
                scans: std::sync::atomic::AtomicUsize::new(0),
            })
        }

        fn answering(
            self: Arc<Self>,
            answer: impl FnMut(&CommandSpec) -> Result<CommandOutput, UpdateFailure> + Send + 'static,
        ) -> Arc<Self> {
            *self.answer.lock().expect("answer") = Box::new(answer);
            self
        }

        fn lookup(
            self: Arc<Self>,
            lookup: impl FnMut(&str) -> Result<Vec<u8>, UpdateFailure> + Send + 'static,
        ) -> Arc<Self> {
            *self.lookup.lock().expect("lookup") = Box::new(lookup);
            self
        }

        fn serving(
            self: Arc<Self>,
            serving: impl FnMut(&str, &Path) -> Result<(), UpdateFailure> + Send + 'static,
        ) -> Arc<Self> {
            *self.serving.lock().expect("serving") = Box::new(serving);
            self
        }

        fn runs(&self) -> Vec<Vec<OsString>> {
            self.runs
                .lock()
                .expect("runs")
                .iter()
                .map(|(argv, _, _)| argv.clone())
                .collect()
        }

        fn run_strings(&self) -> Vec<Vec<String>> {
            self.runs()
                .into_iter()
                .map(|argv| {
                    argv.into_iter()
                        .map(|word| word.to_string_lossy().into_owned())
                        .collect()
                })
                .collect()
        }

        #[cfg_attr(not(unix), allow(dead_code))]
        fn run_envs(&self) -> Vec<Vec<(String, String)>> {
            self.runs
                .lock()
                .expect("runs")
                .iter()
                .map(|(_, _, env)| {
                    env.iter()
                        .map(|(k, v)| {
                            (
                                k.to_string_lossy().into_owned(),
                                v.to_string_lossy().into_owned(),
                            )
                        })
                        .collect()
                })
                .collect()
        }

        fn timeouts(&self) -> Vec<Duration> {
            self.runs
                .lock()
                .expect("runs")
                .iter()
                .map(|(_, timeout, _)| *timeout)
                .collect()
        }

        fn asked(&self) -> Vec<(String, u64, String)> {
            self.asked.lock().expect("asked").clone()
        }

        fn downloads(&self) -> Vec<(String, u64)> {
            self.downloads.lock().expect("downloads").clone()
        }

        fn scans(&self) -> usize {
            self.scans.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl System for Fixture {
        fn run<'a>(
            &'a self,
            spec: &'a CommandSpec,
            diagnostics: &'a Mutex<Diagnostics>,
        ) -> BoxFuture<'a, Result<CommandOutput, UpdateFailure>> {
            Box::pin(async move {
                self.runs.lock().expect("runs").push((
                    spec.argv.clone(),
                    spec.timeout,
                    spec.env.clone(),
                ));
                let answer = {
                    let mut answer = self.answer.lock().expect("answer");
                    answer(spec)
                };
                if !spec.quiet {
                    let exit = match &answer {
                        Ok(_) => Some(0),
                        Err(failure) => failure.exit_code,
                    };
                    diagnostics.lock().expect("record").ran(
                        &spec.argv,
                        spec.label,
                        exit,
                        Some(answer.is_err()),
                    );
                }
                answer
            })
        }

        fn processes(
            &self,
            _platform: Platform,
        ) -> BoxFuture<'_, Result<ProcessTable, UpdateFailure>> {
            Box::pin(async move {
                self.scans.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let scripted = self.scans_script.lock().expect("scans_script").pop_front();
                let items = scripted.unwrap_or_else(|| self.table.lock().expect("table").clone());
                Ok(ProcessTable::new(items))
            })
        }

        fn download<'a>(
            &'a self,
            url: &'a str,
            destination: &'a Path,
            maximum: u64,
            _timeout: Duration,
        ) -> BoxFuture<'a, Result<Downloaded, UpdateFailure>> {
            Box::pin(async move {
                self.downloads
                    .lock()
                    .expect("downloads")
                    .push((url.to_owned(), maximum));
                let mut serving = self.serving.lock().expect("serving");
                serving(url, destination)?;
                drop(serving);
                let bytes = std::fs::metadata(destination)
                    .map(|info| info.len())
                    .unwrap_or_default();
                Ok(Downloaded {
                    bytes,
                    url: url.to_owned(),
                })
            })
        }

        fn fetch<'a>(
            &'a self,
            url: &'a str,
            prefix: &'a str,
            maximum: u64,
            _timeout: Duration,
        ) -> BoxFuture<'a, Result<Vec<u8>, UpdateFailure>> {
            Box::pin(async move {
                self.asked.lock().expect("asked").push((
                    url.to_owned(),
                    maximum,
                    prefix.to_owned(),
                ));
                let delay = *self.delay.lock().expect("delay");
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                let mut lookup = self.lookup.lock().expect("lookup");
                lookup(url)
            })
        }

        fn fingerprint<'a>(
            &'a self,
            _url: &'a str,
            _timeout: Duration,
        ) -> BoxFuture<'a, Option<Fingerprint>> {
            Box::pin(async move { None })
        }
    }

    /// The OS-specific host operations, scripted where the Python tests mock them. This
    /// process answers session 1; a close removes the closed mains (and their children)
    /// from the process table.
    struct Scripted {
        arguments: Mutex<Vec<(u32, Vec<String>)>>,
        events: Arc<Mutex<Vec<&'static str>>>,
        closed: Mutex<Vec<Vec<u32>>>,
        reopened: Mutex<Vec<Vec<u32>>>,
        session_id: Mutex<Option<u32>>,
        /// What the reopen step raises instead of reopening.
        reopen: Mutex<Option<UpdateFailure>>,
        table: Table,
    }

    impl Services for Scripted {
        fn arguments<'a>(&'a self, _cx: &'a Context, pid: u32) -> BoxFuture<'a, Vec<String>> {
            Box::pin(async move {
                self.arguments
                    .lock()
                    .expect("arguments")
                    .iter()
                    .find(|(found, _)| *found == pid)
                    .map(|(_, arguments)| arguments.clone())
                    .unwrap_or_default()
            })
        }

        fn session(&self) -> Option<u32> {
            *self.session_id.lock().expect("session")
        }

        fn terminate<'a>(
            &'a self,
            _cx: &'a Context,
            _install: &'a Installed,
            contexts: &'a [Process],
        ) -> BoxFuture<'a, Result<u32, UpdateFailure>> {
            Box::pin(async move {
                self.events.lock().expect("events").push("close");
                self.closed
                    .lock()
                    .expect("closed")
                    .push(contexts.iter().map(|item| item.pid).collect());
                let mains: HashSet<u32> = contexts.iter().map(|item| item.pid).collect();
                self.table
                    .lock()
                    .expect("table")
                    .retain(|item| !mains.contains(&item.pid) && !mains.contains(&item.ppid));
                Ok(u32::from(!contexts.is_empty()))
            })
        }

        fn restart<'a>(
            &'a self,
            _cx: &'a Context,
            _install: &'a Installed,
            contexts: &'a [Process],
        ) -> BoxFuture<'a, Result<u32, UpdateFailure>> {
            Box::pin(async move {
                self.events.lock().expect("events").push("reopen");
                self.reopened
                    .lock()
                    .expect("reopened")
                    .push(contexts.iter().map(|item| item.pid).collect());
                match self.reopen.lock().expect("reopen").clone() {
                    Some(error) => Err(error),
                    None => Ok(u32::try_from(contexts.len()).unwrap_or(u32::MAX)),
                }
            })
        }

        fn pause<'a>(&'a self, _seconds: Duration) -> BoxFuture<'a, ()> {
            Box::pin(async move {})
        }
    }

    /// One test's folder and scripted hosts. The home folder is the test's root, as the
    /// Python tests patch `Path.home()`.
    struct Rig {
        _root: tempfile::TempDir,
        path: PathBuf,
        system: Arc<Fixture>,
        services: Arc<Scripted>,
        table: Table,
        phases: Arc<Mutex<Vec<&'static str>>>,
        /// The test computer's PATH (empty until a test adds a folder).
        search_path: Vec<PathBuf>,
        arch: &'static str,
    }

    impl Rig {
        fn start() -> Self {
            let root = tempfile::tempdir().expect("root");
            let path = real(root.path());
            let table: Table = Arc::new(Mutex::new(Vec::new()));
            let events = Arc::new(Mutex::new(Vec::new()));
            Self {
                _root: root,
                path,
                system: Fixture::plain(table.clone()),
                services: Arc::new(Scripted {
                    arguments: Mutex::new(Vec::new()),
                    events,
                    closed: Mutex::new(Vec::new()),
                    reopened: Mutex::new(Vec::new()),
                    session_id: Mutex::new(Some(1)),
                    reopen: Mutex::new(None),
                    table: table.clone(),
                }),
                table,
                phases: Arc::new(Mutex::new(Vec::new())),
                search_path: Vec::new(),
                arch: "x86_64",
            }
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn host(&self, platform: Platform) -> Arc<Host> {
            let mut host = Arc::into_inner(host(self.path(), platform)).expect("own host");
            host.path = self.search_path.clone();
            host.arch = self.arch;
            Arc::new(host)
        }

        /// A context with its own shared state (one job with one row so far).
        fn cx(&self, platform: Platform, seconds: u64) -> Context {
            self.cx_shared(platform, seconds, Arc::new(Shared::default()))
        }

        /// A context with an explicit deadline (including past deadlines).
        fn cx_deadline(
            &self,
            platform: Platform,
            deadline: Instant,
            shared: Arc<Shared>,
        ) -> Context {
            let phases = self.phases.clone();
            Context::new(
                platform,
                self.host(platform),
                self.system.clone(),
                shared,
                deadline,
                self.path().join("stage"),
                self.path().join("state"),
                None,
                || false,
                move |phase| phases.lock().expect("phases").push(phase),
            )
        }

        /// A context that shares `shared` with the job's other rows.
        fn cx_shared(&self, platform: Platform, seconds: u64, shared: Arc<Shared>) -> Context {
            self.cx_deadline(
                platform,
                Instant::now() + Duration::from_secs(seconds),
                shared,
            )
        }

        /// The ZCode row of one whole attempt (Python `update_zcode` on the detected install).
        async fn zcode(&self, platform: Platform) -> Value {
            attempt(&ZCODE, &self.cx(platform, 900)).await.to_value()
        }

        /// The adapters row of one whole attempt.
        async fn adapters(&self, platform: Platform) -> Value {
            attempt(&ADAPTERS, &self.cx(platform, 900)).await.to_value()
        }

        /// Export diagnostics onto a direct `zcode_update` / `adapters_update` row, as `attempt` does.
        fn finish(cx: &Context, mut row: AppRow) -> Value {
            let value = row.to_value();
            row.diagnostics = Some(cx.record(|record| record.export(&value)));
            row.to_value()
        }
    }

    fn host(root: &Path, platform: Platform) -> Arc<Host> {
        let root = real(root);
        Arc::new(Host {
            platform,
            home: root.clone(),
            local: root.join("local"),
            roaming: root.join("roaming"),
            program_files: root.join("Program Files"),
            path: Vec::new(),
            path_ext: vec![".CMD".into(), ".EXE".into()],
            powershell: None,
            npm_prefix: None,
            arch: "x86_64",
            system_root: Some(root.join("system")),
        })
    }

    fn process(pid: u32, ppid: u32, exe: &str, arguments: &[&str]) -> Process {
        Process {
            pid,
            ppid,
            uid: 1,
            exe: exe.into(),
            identity: pid.to_string(),
            args: arguments
                .iter()
                .map(|argument| (*argument).to_owned())
                .collect(),
            ..Process::default()
        }
    }

    fn write_package(root: &Path, package: &str, version: &str) {
        let folder = root.join(package);
        std::fs::create_dir_all(&folder).expect("package folder");
        let manifest = json!({"name": package, "version": version}).to_string();
        std::fs::write(folder.join("package.json"), manifest).expect("package.json");
    }

    /// An app.asar whose root package.json carries `version`; filler entries grow the
    /// header like ZCode's node_modules.
    fn write_asar(resources: &Path, version: &str, name: &str, filler: usize) {
        std::fs::create_dir_all(resources).expect("resources");
        let package = json!({"name": name, "productName": "ZCode", "version": version}).to_string();
        let mut files = json!({"package.json": {"size": package.len(), "offset": "0"}});
        if filler > 0 {
            let entries: serde_json::Map<String, Value> = (0..filler)
                .map(|index| (format!("f{index:06}"), json!({"size": 1, "offset": "0"})))
                .collect();
            files["node_modules"] = json!({"files": Value::Object(entries)});
        }
        let header = json!({"files": files}).to_string();
        let padded = header.len().div_ceil(4) * 4;
        let field = |value: usize| u32::try_from(value).expect("asar size").to_le_bytes();
        let mut asar = Vec::new();
        asar.extend(field(4));
        asar.extend(field(8 + padded));
        asar.extend(field(4 + padded));
        asar.extend(field(header.len()));
        asar.extend(header.as_bytes());
        asar.extend(std::iter::repeat_n(0, padded - header.len()));
        asar.extend(package.as_bytes());
        std::fs::write(resources.join("app.asar"), asar).expect("app.asar");
    }

    fn mac_bundle(bundle: &Path, version: &str, identity: &str) {
        std::fs::create_dir_all(bundle.join("Contents")).expect("Contents");
        let plist = format!(
            concat!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
                "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ",
                "\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n",
                "\t<key>CFBundleIdentifier</key>\n\t<string>{}</string>\n",
                "\t<key>CFBundleShortVersionString</key>\n\t<string>{}</string>\n</dict>\n</plist>\n"
            ),
            identity, version
        );
        std::fs::write(bundle.join("Contents/Info.plist"), plist).expect("Info.plist");
    }

    /// A `latest.yml` whose one asset is `data` (its sha512, and its size unless `size`).
    fn manifest(version: &str, asset: &str, data: &[u8], size: bool) -> String {
        let digest = base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(data));
        let size = if size {
            format!("    size: {}\n", data.len())
        } else {
            String::new()
        };
        format!(
            concat!(
                "version: {}\nfiles:\n  - url: {}\n    sha512: {}\n{}path: {}\nsha512: {}\n",
                "releaseDate: '2026-09-30T08:16:39.977Z'\nreleaseNotes: |-\n  ## Fixes\n\n  - Fixture.\n"
            ),
            version, asset, digest, size, asset, digest
        )
    }

    /// The official page listing `(version, folder)` entries.
    fn page(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut html = String::from("<html>");
        for (version, key) in entries {
            html.push_str(&format!(
                "<a href=\"{RELEASES}{version}/{key}/latest.yml\">x</a>"
            ));
        }
        html.push_str("</html>");
        html.into_bytes()
    }

    /// The page and registry answers; `None` fails that lookup.
    fn feed(
        listed: Option<Vec<u8>>,
        muse: Option<&'static str>,
        acp: Option<&'static str>,
    ) -> impl FnMut(&str) -> Result<Vec<u8>, UpdateFailure> + Send + 'static {
        move |url| {
            if url == PAGE {
                return listed.clone().ok_or_else(UpdateFailure::failed);
            }
            let body = if url == REGISTRY[0] {
                json!({"name": "@brokkai/muse-acp", "version": muse})
            } else if url == REGISTRY[1] {
                json!({"name": "zcode-acp-server", "version": acp})
            } else {
                return Err(UpdateFailure::failed());
            };
            Ok(body.to_string().into_bytes())
        }
    }

    /// The script inside a fixture `powershell` argv.
    fn script(argv: &[OsString]) -> String {
        let encoded = argv
            .last()
            .and_then(|word| word.to_str())
            .expect("encoded command");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("base64");
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        String::from_utf16(&units).expect("script")
    }

    fn unit(root: &Path) {
        let file = root.join(".config/systemd/user").join(shared::ACP_UNIT);
        std::fs::create_dir_all(file.parent().expect("unit folder")).expect("user units");
        std::fs::write(file, "[Service]\nType=oneshot\n").expect("unit");
    }

    /// ZCode's extracted AppImage folder on Ubuntu.
    fn ubuntu_app(root: &Path, version: &str) -> PathBuf {
        let app = root.join(".local/opt/zcode/app");
        write_asar(&app.join("resources"), version, "@zcode/desktop", 0);
        std::fs::write(app.join("zcode"), b"").expect("zcode");
        app
    }

    /// The usual global npm folder of one OS.
    fn modules(root: &Path, platform: Platform) -> PathBuf {
        match platform {
            Platform::Ubuntu => root.join(".local/lib/node_modules"),
            Platform::Mac => root.join("system/opt/homebrew/lib/node_modules"),
            Platform::Windows => root.join("roaming/npm/node_modules"),
        }
    }

    fn windows_app(root: &Path, version: &str) -> PathBuf {
        let executable = root.join("local/Programs/ZCode/ZCode.exe");
        let folder = executable.parent().expect("program folder");
        write_asar(&folder.join("resources"), version, "@zcode/desktop", 0);
        std::fs::write(&executable, b"").expect("ZCode.exe");
        std::fs::write(folder.join("Uninstall ZCode.exe"), b"").expect("uninstaller");
        executable
    }

    /// A fixture environment: only what a test sets.
    #[derive(Default)]
    struct Lookups(std::collections::HashMap<String, String>);

    impl Vars for Lookups {
        fn get(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
    }

    /// The bytes of a hex string.
    fn unhex(text: &str) -> Vec<u8> {
        assert!(text.len().is_multiple_of(2), "hex pairs");
        let digit = |byte: u8| match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            _ => panic!("hex digit"),
        };
        text.as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| digit(pair[0]) * 16 + digit(pair[1]))
            .collect()
    }

    /// CPython's own binary plist for the two fields (`plistlib.dumps(..., FMT_BINARY)`):
    /// the root dict first, long strings with an integer length, keys then values.
    const PLISTLIB: &str = "62706c6973743030d2010203045f1012434642756e646c654964656e7469666965725f101a434642756e646c6553686f727456657273696f6e537472696e675d6465762e7a636f64652e61707056332e31342e34080d223f4d0000000000000101000000000000000500000000000000000000000000000054";

    // ------------------------------------------------------------ detection

    /// test_detects_zcode_on_every_platform_from_its_files_without_running_anything
    #[tokio::test]
    async fn zcode_is_detected_on_every_platform_from_its_files_without_running_anything() {
        let rig = Rig::start();
        let root = rig.path();
        for platform in [Platform::Ubuntu, Platform::Mac, Platform::Windows] {
            assert!(
                detect_zcode(&rig.cx(platform, 900)).is_none(),
                "{platform:?}"
            );
        }
        let app = ubuntu_app(root, OLD);
        let cx = rig.cx(Platform::Ubuntu, 900);
        let install = detect_zcode(&cx).expect("ubuntu");
        let found = (
            install.app_id,
            install.version.as_deref(),
            install.manager,
            install.package_root.as_deref(),
        );
        assert_eq!(
            found,
            ("zcode", Some(OLD), "official-download", Some(app.as_path()))
        );
        // The asar wins; t3-acp-update's stamp is the fallback when it cannot be read.
        let stamp = app.parent().expect("opt").join(".installed-version");
        std::fs::write(&stamp, format!("{NEW}\n")).expect("stamp");
        let again = detect_zcode(&cx).expect("ubuntu");
        assert_eq!(again.version.as_deref(), Some(OLD));
        std::fs::remove_file(app.join("resources/app.asar")).expect("asar");
        assert_eq!(
            detect_zcode(&cx).expect("ubuntu").version.as_deref(),
            Some(NEW)
        );
        std::fs::remove_file(&stamp).expect("stamp");
        let executable = windows_app(root, OLD);
        let cx = rig.cx(Platform::Windows, 900);
        let install = detect_zcode(&cx).expect("windows");
        assert_eq!(
            (
                install.path.clone(),
                install.version.as_deref(),
                install.package_root.as_deref()
            ),
            (executable.clone(), Some(OLD), executable.parent())
        );
        let resources = executable.parent().expect("folder").join("resources");
        write_asar(&resources, NEW, "impostor", 0);
        assert!(detect_zcode(&cx).expect("windows").version.is_none());
        let bundle = rig
            .host(Platform::Mac)
            .system("/Applications")
            .join(MAC_NAME);
        mac_bundle(&bundle, OLD, MAC_ID);
        let cx = rig.cx(Platform::Mac, 900);
        let install = detect_zcode(&cx).expect("mac");
        assert_eq!(
            (
                install.path,
                install.version.as_deref(),
                install.identity.as_deref(),
                install.publisher.as_deref()
            ),
            (bundle.clone(), Some(OLD), Some(MAC_ID), Some(MAC_TEAM))
        );
        mac_bundle(&bundle, NEW, "com.example.other");
        assert!(detect_zcode(&cx).expect("mac").version.is_none());
        assert!(rig.system.runs().is_empty(), "detection runs nothing");
    }

    /// test_zcode_asar_header_of_several_megabytes_is_read_and_t3_keeps_its_two_megabyte_bound
    #[test]
    fn zcode_asar_header_of_several_megabytes_is_read() {
        let root = tempfile::tempdir().expect("root");
        let resources = root.path().join("resources");
        write_asar(&resources, NEW, "@zcode/desktop", 70_000);
        let asar = resources.join("app.asar");
        assert!(std::fs::metadata(&asar).expect("asar").len() > 2 * 1024 * 1024);
        assert_eq!(asar_version(&resources).as_deref(), Some(NEW));
        assert!(shared::asar_package(&asar, 2 * 1024 * 1024).is_none());
    }

    /// test_detects_adapters_with_each_part_and_null_for_a_missing_one
    #[tokio::test]
    #[cfg_attr(windows, ignore = "Ubuntu prefixes are shown with the host separator")]
    async fn adapters_are_detected_with_each_part_and_null_for_a_missing_one() {
        let rig = Rig::start();
        let root = rig.path();
        let nothing = Lookups::default();
        let found = |platform: Platform| {
            discover_owners(&rig.host(platform), &nothing)
                .owners
                .map(|owners| owners.len())
        };
        assert_eq!(found(Platform::Ubuntu), [0, 0]);
        assert!(detect_adapters(&rig.cx(Platform::Ubuntu, 900), &nothing).is_none());
        write_package(
            &modules(root, Platform::Ubuntu),
            "@brokkai/muse-acp",
            "0.10.0",
        );
        let install = detect_adapters(&rig.cx(Platform::Ubuntu, 900), &nothing).expect("adapters");
        assert_eq!(
            (install.app_id, install.version, install.manager),
            ("t3-acp-adapters", None, "npm")
        );
        let parts =
            |platform: Platform| owned_parts(&discover_owners(&rig.host(platform), &nothing));
        let ten = || Some("0.10.0".to_owned());
        assert_eq!(parts(Platform::Ubuntu), [ten(), None]);
        assert_eq!(
            install.owner.as_ref().expect("owner")["packages"],
            json!([{"package": "muse-acp", "manager": "npm", "prefix": "~/.local", "source": "default", "version": "0.10.0"}])
        );
        for (package, version) in [
            ("@brokkai/muse-acp", "0.10.0"),
            ("zcode-acp-server", "0.65.1"),
        ] {
            write_package(&modules(root, Platform::Windows), package, version);
        }
        let install = detect_adapters(&rig.cx(Platform::Windows, 900), &nothing).expect("adapters");
        assert_eq!(
            real(&install.path),
            real(&root.join("roaming/npm/node_modules"))
        );
        assert_eq!(parts(Platform::Windows), [ten(), Some("0.65.1".to_owned())]);
        assert_eq!(
            adapter_root(&rig.host(Platform::Mac)),
            root.join("system/opt/homebrew/lib/node_modules")
        );
        assert!(rig.system.runs().is_empty(), "detection runs nothing");
    }

    /// `plistlib.load` reads XML and binary plists; both give the bundle's two fields.
    #[test]
    fn info_plist_is_read_in_its_xml_and_binary_forms() {
        let root = tempfile::tempdir().expect("root");
        let bundle = root.path().join("ZCode.app");
        mac_bundle(&bundle, OLD, MAC_ID);
        let xml = std::fs::read(bundle.join("Contents/Info.plist")).expect("plist");
        let fields = vec![
            ("CFBundleIdentifier".to_owned(), MAC_ID.to_owned()),
            ("CFBundleShortVersionString".to_owned(), OLD.to_owned()),
        ];
        assert_eq!(plist_strings(&xml), Some(fields.clone()));
        assert_eq!(plist_strings(&unhex(PLISTLIB)), Some(fields));
        assert_eq!(mac_version(&bundle).as_deref(), Some(OLD));
        let nested = br#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>dev.zcode.app</string><key>CFBundleShortVersionString</key><string>3.14.5</string><key>Nested</key><dict><key>CFBundleShortVersionString</key><string>9.9.9</string><key>Flags</key><array><true/><false/><integer>7</integer><string>a &amp; b</string></array></dict><key>Data</key><data>QUJD</data></dict></plist>"#;
        let fields = vec![
            ("CFBundleIdentifier".to_owned(), "dev.zcode.app".to_owned()),
            ("CFBundleShortVersionString".to_owned(), "3.14.5".to_owned()),
        ];
        assert_eq!(plist_strings(nested), Some(fields));
        for unreadable in [
            &b"<plist><dict><key>a</key>"[..],
            &b"<plist><dict><key>a</key><string>x</string></array></dict></plist>"[..],
            &b"bplist01garbage"[..],
            &b"bplist00"[..],
            &b"<plist><dict><key>CFBundleIdentifier</key><string>&nope;</string></dict></plist>"[..],
        ] {
            assert_eq!(plist_strings(unreadable), None);
        }
        // XML entities are decoded in the fields this module reads.
        let escaped = br#"<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>dev&#46;zcode&#x2e;app</string><key>CFBundleShortVersionString</key><string>3.14.5</string></dict></plist>"#;
        assert_eq!(
            plist_strings(escaped).map(|fields| fields[0].1.clone()),
            Some("dev.zcode.app".to_owned())
        );
    }

    // ------------------------------------------------------------ feeds

    const MUSE: Adapter = 0;
    const ACP: Adapter = 1;

    /// test_feed_folder_follows_this_computer
    #[test]
    fn feed_folder_follows_this_computer() {
        let root = tempfile::tempdir().expect("root");
        let nothing = Lookups::default();
        let key = |host: &Host, vars: &dyn Vars| feed_key(host, vars).ok();
        assert_eq!(
            key(&host(root.path(), Platform::Ubuntu), &nothing).as_deref(),
            Some("linux-x64")
        );
        assert_eq!(
            key(&host(root.path(), Platform::Windows), &nothing).as_deref(),
            Some("windows-x64")
        );
        let mut arm = Arc::into_inner(host(root.path(), Platform::Mac)).expect("own host");
        arm.arch = "aarch64";
        assert_eq!(key(&arm, &nothing).as_deref(), Some("macos-arm64"));
        let mut table = std::collections::HashMap::new();
        table.insert("PROCESSOR_ARCHITECTURE".to_owned(), "arm64".to_owned());
        assert_eq!(
            key(&host(root.path(), Platform::Windows), &Lookups(table)).as_deref(),
            Some("windows-arm64")
        );
        arm.arch = "i686";
        assert_eq!(
            feed_key(&arm, &nothing).err().map(|failure| failure.code),
            Some("unsupported")
        );
    }

    /// test_newest_zcode_comes_from_the_bounded_official_page_sorted_numerically
    #[tokio::test]
    async fn newest_zcode_comes_from_the_bounded_official_page_sorted_numerically() {
        let rig = Rig::start();
        let listed = page(&[
            ("3.9.9", "linux-x64"),
            (NEW, "linux-x64"),
            ("3.10.1", "linux-x64"),
            ("9.9.9", "windows-x64"),
            ("3.14.6", "linux-arm64"),
        ]);
        rig.system.clone().lookup(feed(Some(listed), None, None));
        let cx = rig.cx(Platform::Ubuntu, 900);
        let newest = latest_zcode(&cx, &*rig.services, "linux-x64", cx.deadline).await;
        assert_eq!(newest.ok().as_deref(), Some(NEW));
        assert_eq!(
            rig.system.asked(),
            [(
                PAGE.to_owned(),
                PAGE_LIMIT,
                "https://zcode.z.ai/".to_owned()
            )]
        );
        // Another platform's listing and an unreadable page end in the same honest word,
        // after the bounded number of attempts.
        rig.system.asked.lock().expect("asked").clear();
        rig.system
            .clone()
            .lookup(feed(Some(page(&[("3.9.9", "macos-arm64")])), None, None));
        let newest = latest_zcode(&cx, &*rig.services, "linux-x64", cx.deadline).await;
        assert_eq!(
            newest.err().map(|failure| failure.code),
            Some("zcode_release_unavailable")
        );
        assert_eq!(rig.system.asked().len(), PAGE_ATTEMPTS as usize);
        assert!(rig.system.runs().is_empty(), "a lookup runs no command");
    }

    /// test_zcode_recovers_a_transient_lookup_and_never_claims_current_after_failure
    #[tokio::test]
    async fn zcode_recovers_a_transient_lookup_and_never_claims_current_after_failure() {
        let rig = Rig::start();
        let listed = page(&[("3.16.3", "linux-x64")]);
        let mut attempts = 0;
        rig.system.clone().lookup(move |_url| {
            attempts += 1;
            if attempts == 1 {
                Err(UpdateFailure::failed())
            } else {
                Ok(listed.clone())
            }
        });
        let cx = rig.cx(Platform::Ubuntu, 900);
        let newest = latest_zcode(&cx, &*rig.services, "linux-x64", cx.deadline).await;
        assert_eq!(newest.ok().as_deref(), Some("3.16.3"));
        assert_eq!(rig.system.asked().len(), 2);
        rig.system.asked.lock().expect("asked").clear();
        rig.system.clone().lookup(feed(None, None, None));
        assert!(
            latest_zcode(&cx, &*rig.services, "linux-x64", cx.deadline)
                .await
                .is_err()
        );
        assert_eq!(rig.system.asked().len(), PAGE_ATTEMPTS as usize);
    }

    /// test_zcode_does_not_start_another_attempt_after_the_total_lookup_deadline
    #[tokio::test]
    async fn zcode_does_not_start_another_attempt_after_the_total_lookup_deadline() {
        let rig = Rig::start();
        rig.system
            .clone()
            .lookup(feed(Some(page(&[("3.16.3", "linux-x64")])), None, None));
        *rig.system.delay.lock().expect("delay") = Duration::from_millis(1300);
        let cx = rig.cx(Platform::Ubuntu, 1);
        let newest = latest_zcode(&cx, &*rig.services, "linux-x64", cx.deadline).await;
        assert_eq!(newest.err().map(|failure| failure.code), Some("timeout"));
        assert_eq!(
            rig.system.asked().len(),
            1,
            "no second attempt once the total deadline passed"
        );
    }

    /// test_registry_version_requires_the_exact_package_and_registry
    #[tokio::test]
    async fn registry_version_requires_the_exact_package_and_registry() {
        let rig = Rig::start();
        let cx = rig.cx(Platform::Ubuntu, 900);
        rig.system.clone().lookup(feed(None, None, Some("0.65.2")));
        assert_eq!(registry_version(&cx, ACP).await.as_deref(), Some("0.65.2"));
        assert_eq!(
            rig.system.asked(),
            [(
                REGISTRY[ACP].to_owned(),
                1024 * 1024,
                "https://registry.npmjs.org/".to_owned()
            )]
        );
        for body in [
            json!({"name": "other", "version": "1.0.0"}).to_string(),
            "not json".to_owned(),
        ] {
            let served = body.clone();
            rig.system
                .clone()
                .lookup(move |_url| Ok(served.clone().into_bytes()));
            assert_eq!(registry_version(&cx, ACP).await, None, "{body}");
        }
        assert_eq!(
            REGISTRY[MUSE],
            "https://registry.npmjs.org/@brokkai%2fmuse-acp/latest"
        );
    }

    /// test_package_download_is_bound_to_the_manifest_size_and_sha512
    #[tokio::test]
    async fn package_download_is_bound_to_the_manifest_size_and_sha512() {
        let data = b"MZ fixture installer";
        let asset = format!("ZCode-{NEW}-win-x64.exe");
        // Same length, one byte changed: only the sha512 can tell.
        let mut tampered = data.to_vec();
        tampered[data.len() - 1] = b'!';
        for (served, accepted) in [(data.to_vec(), true), (tampered, false)] {
            let rig = Rig::start();
            let manifest = manifest(NEW, &asset, data, true);
            rig.system.clone().serving(move |url, target| {
                let body = if url.ends_with("latest.yml") {
                    manifest.clone()
                } else {
                    String::from_utf8(served.clone()).expect("fixture")
                };
                std::fs::write(target, body).expect("served");
                Ok(())
            });
            let cx = rig.cx(Platform::Windows, 900);
            let folder = rig.path().join("stage");
            std::fs::create_dir_all(&folder).expect("stage");
            let outcome = verified_package(&cx, "windows-x64", NEW, &folder).await;
            if accepted {
                let package = outcome.expect("package");
                assert_eq!(
                    package.file_name().and_then(OsStr::to_str),
                    Some(asset.as_str())
                );
                assert_eq!(std::fs::read(package).expect("bytes"), data);
            } else {
                assert_eq!(
                    outcome.err().map(|failure| failure.code),
                    Some("signature_failed")
                );
            }
            assert_eq!(
                rig.system.downloads(),
                [
                    (format!("{RELEASES}{NEW}/windows-x64/latest.yml"), 65_536),
                    (
                        format!("{RELEASES}{NEW}/windows-x64/{asset}"),
                        u64::try_from(data.len()).expect("size")
                    ),
                ]
            );
        }
    }

    // ------------------------------------------------------------ test helpers

    fn parts_of(row: &Value) -> Vec<(String, Option<String>, Option<String>)> {
        row["parts"]
            .as_array()
            .expect("parts")
            .iter()
            .map(|part| {
                (
                    part["name"].as_str().expect("name").to_owned(),
                    part["previousVersion"].as_str().map(str::to_owned),
                    part["version"].as_str().map(str::to_owned),
                )
            })
            .collect()
    }

    fn make_executable(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut perms = std::fs::metadata(path).expect("meta").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(path, perms).expect("chmod");
        }
        #[cfg(not(unix))]
        let _ = path;
    }

    #[cfg(unix)]
    fn symlink_path(target: &Path, link: &Path) {
        if link.symlink_metadata().is_ok() {
            let _ = std::fs::remove_file(link);
        }
        if let Some(parent) = link.parent() {
            std::fs::create_dir_all(parent).expect("link parent");
        }
        std::os::unix::fs::symlink(target, link).expect("symlink");
    }

    fn write_npm_pair(prefix: &Path, platform: Platform) -> (PathBuf, PathBuf) {
        if platform == Platform::Windows {
            let node = prefix.join("node.exe");
            let cli = prefix.join("node_modules/npm/bin/npm-cli.js");
            std::fs::create_dir_all(cli.parent().expect("cli")).expect("cli dir");
            std::fs::write(&node, b"node").expect("node");
            std::fs::write(&cli, b"npm").expect("cli");
            (node, cli)
        } else {
            let node = prefix.join("bin/node");
            let cli = prefix.join("lib/node_modules/npm/bin/npm-cli.js");
            std::fs::create_dir_all(node.parent().expect("bin")).expect("bin dir");
            std::fs::create_dir_all(cli.parent().expect("cli")).expect("cli dir");
            std::fs::write(&node, b"node").expect("node");
            make_executable(&node);
            std::fs::write(&cli, b"npm").expect("cli");
            (node, cli)
        }
    }

    fn write_t3_consumers(root: &Path, paths: &[PathBuf]) {
        let settings = root.join(".t3/userdata/settings.json");
        std::fs::create_dir_all(settings.parent().expect("t3")).expect("t3 dir");
        let mut instances = serde_json::Map::new();
        for (index, path) in paths.iter().enumerate() {
            instances.insert(
                format!("zcode-{index}"),
                json!({
                    "enabled": true,
                    "config": {
                        "commandPath": "node",
                        "commandArgs": [path.to_string_lossy(), "server"],
                    }
                }),
            );
        }
        std::fs::write(
            settings,
            json!({"providerInstances": instances}).to_string(),
        )
        .expect("settings");
    }

    fn write_npm_prefix_adapter(root: &Path, prefix: &str, version: &str) -> PathBuf {
        let mods = root.join(prefix).join("lib/node_modules");
        write_package(&mods, "zcode-acp-server", version);
        let cli = mods.join("zcode-acp-server/dist/cli.js");
        std::fs::create_dir_all(cli.parent().expect("dist")).expect("dist dir");
        std::fs::write(cli, b"cli").expect("cli");
        mods
    }

    fn write_stored_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut body = Vec::new();
        let mut directory = Vec::new();
        for (name, data) in entries {
            let crc = {
                let mut crc = 0xffff_ffff_u32;
                for byte in *data {
                    crc ^= u32::from(*byte);
                    for _ in 0..8 {
                        crc = if crc & 1 == 1 {
                            (crc >> 1) ^ 0xedb8_8320
                        } else {
                            crc >> 1
                        };
                    }
                }
                !crc
            };
            let offset = u32::try_from(body.len()).expect("offset");
            let size = u32::try_from(data.len()).expect("size");
            let name_len = u16::try_from(name.len()).expect("name");
            body.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04, 20, 0, 0, 8, 0, 0, 0, 0, 0, 0]);
            body.extend_from_slice(&crc.to_le_bytes());
            body.extend_from_slice(&size.to_le_bytes());
            body.extend_from_slice(&size.to_le_bytes());
            body.extend_from_slice(&name_len.to_le_bytes());
            body.extend_from_slice(&0_u16.to_le_bytes());
            body.extend_from_slice(name.as_bytes());
            body.extend_from_slice(data);
            let mode: u32 = 0o100_644;
            directory
                .extend_from_slice(&[0x50, 0x4b, 0x01, 0x02, 20, 3, 20, 0, 0, 8, 0, 0, 0, 0, 0, 0]);
            directory.extend_from_slice(&crc.to_le_bytes());
            directory.extend_from_slice(&size.to_le_bytes());
            directory.extend_from_slice(&size.to_le_bytes());
            directory.extend_from_slice(&name_len.to_le_bytes());
            directory.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
            directory.extend_from_slice(&(mode << 16).to_le_bytes());
            directory.extend_from_slice(&offset.to_le_bytes());
            directory.extend_from_slice(name.as_bytes());
        }
        let start = u32::try_from(body.len()).expect("start");
        let count = u16::try_from(entries.len()).expect("count");
        let size = u32::try_from(directory.len()).expect("size");
        body.extend_from_slice(&directory);
        body.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0]);
        body.extend_from_slice(&count.to_le_bytes());
        body.extend_from_slice(&count.to_le_bytes());
        body.extend_from_slice(&size.to_le_bytes());
        body.extend_from_slice(&start.to_le_bytes());
        body.extend_from_slice(&0_u16.to_le_bytes());
        std::fs::write(path, body).expect("zip");
    }

    fn extract_stored_zip(archive: &Path, dest: &Path) {
        let bytes = std::fs::read(archive).expect("read zip");
        let mut at = 0;
        while bytes.get(at..at + 4) == Some(&[0x50, 0x4b, 0x03, 0x04]) {
            let size =
                u32::from_le_bytes(bytes[at + 18..at + 22].try_into().expect("size")) as usize;
            let name_len =
                u16::from_le_bytes(bytes[at + 26..at + 28].try_into().expect("name")) as usize;
            let extra_len =
                u16::from_le_bytes(bytes[at + 28..at + 30].try_into().expect("extra")) as usize;
            let name = std::str::from_utf8(&bytes[at + 30..at + 30 + name_len]).expect("name");
            let data_start = at + 30 + name_len + extra_len;
            let data = &bytes[data_start..data_start + size];
            let target = dest.join(name);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).expect("extract parent");
            }
            std::fs::write(target, data).expect("extract file");
            at = data_start + size;
        }
    }

    fn plist_xml_bytes(version: &str, identity: &str) -> Vec<u8> {
        format!(
            concat!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
                "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ",
                "\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n",
                "\t<key>CFBundleIdentifier</key>\n\t<string>{}</string>\n",
                "\t<key>CFBundleShortVersionString</key>\n\t<string>{}</string>\n</dict>\n</plist>\n"
            ),
            identity, version
        )
        .into_bytes()
    }

    fn dir_entries(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(folder)
            .expect("read_dir")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    // ------------------------------------------------------------ contract / red-green

    /// Red on W1 base `9873edca7` (`Unported` reports `unknown`/`readiness_unknown`); green on W3.
    #[tokio::test]
    async fn zcode_and_adapters_updaters_are_ported_and_absent_is_not_installed() {
        let rig = Rig::start();
        for platform in [Platform::Ubuntu, Platform::Mac, Platform::Windows] {
            for app_id in ["zcode", "t3-acp-adapters"] {
                let cx = rig.cx(platform, 900);
                let row = attempt(super::super::updater(app_id).expect("updater"), &cx)
                    .await
                    .to_value();
                assert_eq!(
                    (
                        row["status"].as_str(),
                        row["messageCode"].as_str(),
                        row["diagnostics"]["cause"].as_str()
                    ),
                    (Some("not_installed"), Some("not_installed"), Some("none")),
                    "{app_id} on {platform:?}"
                );
            }
        }
    }

    /// test_manifest_binds_sha512_and_size_to_the_exact_zcode_asset
    #[test]
    fn manifest_binds_sha512_and_size_to_the_exact_zcode_asset() {
        let (zip_data, dmg_data) = (b"zip", b"dmg-longer");
        let dmg_sha =
            base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(dmg_data));
        let text = manifest(NEW, "ZCode-3.14.5-mac-arm64.zip", zip_data, true).replacen(
            "path:",
            &format!(
                "  - url: ZCode-3.14.5-mac-arm64.dmg\n    sha512: {dmg_sha}\n    size: 10\npath:"
            ),
            1,
        );
        assert_eq!(
            shared::manifest_asset(&text, NEW, "ZCode-3.14.5-mac-arm64.zip").expect("zip"),
            (sha2::Sha512::digest(zip_data).to_vec(), Some(3))
        );
        assert_eq!(
            shared::manifest_asset(&text, NEW, "ZCode-3.14.5-mac-arm64.dmg")
                .expect("dmg")
                .1,
            Some(10)
        );
        // A top-level size never stands in for the asset's own.
        let top_level_size =
            manifest(NEW, "A.exe", b"a", false).replace("releaseDate", "size: 99\nreleaseDate");
        assert_eq!(
            shared::manifest_asset(&top_level_size, NEW, "A.exe")
                .expect("top size ignored")
                .1,
            None
        );
        assert_eq!(
            shared::manifest_asset(&text, OLD, "ZCode-3.14.5-mac-arm64.zip")
                .err()
                .map(|failure| failure.code),
            Some("signature_failed")
        );
    }

    // ------------------------------------------------------------ Ubuntu

    /// test_ubuntu_current_zcode_and_adapters_run_nothing
    #[tokio::test]
    async fn ubuntu_current_zcode_and_adapters_run_nothing() {
        let rig = Rig::start();
        unit(rig.path());
        ubuntu_app(rig.path(), NEW);
        let mods = modules(rig.path(), Platform::Ubuntu);
        write_package(&mods, "@brokkai/muse-acp", "0.10.1");
        write_package(&mods, "zcode-acp-server", "0.65.1");
        rig.system.clone().lookup(feed(
            Some(page(&[(NEW, "linux-x64")])),
            Some("0.10.1"),
            Some("0.65.1"),
        ));
        let zrow = rig.zcode(Platform::Ubuntu).await;
        let arow = rig.adapters(Platform::Ubuntu).await;
        assert_eq!(
            (
                zrow["status"].as_str(),
                zrow["previousVersion"].as_str(),
                zrow["version"].as_str(),
                zrow["updateAttempted"].as_bool()
            ),
            (Some("current"), Some(NEW), Some(NEW), Some(false))
        );
        assert_eq!(
            (
                arow["status"].as_str(),
                arow["previousVersion"].as_str(),
                arow["version"].as_str(),
                arow["manager"].as_str()
            ),
            (Some("current"), None, None, Some("npm"))
        );
        assert_eq!(
            parts_of(&arow),
            vec![
                (
                    "muse-acp".into(),
                    Some("0.10.1".into()),
                    Some("0.10.1".into())
                ),
                (
                    "zcode-acp-server".into(),
                    Some("0.65.1".into()),
                    Some("0.65.1".into())
                ),
            ]
        );
        assert!(rig.system.runs().is_empty());
        assert_eq!(rig.system.scans(), 0);
    }

    /// test_ubuntu_t3_session_markers_report_in_use_without_running_the_unit
    #[tokio::test]
    async fn ubuntu_t3_session_markers_report_in_use_without_running_the_unit() {
        let rig = Rig::start();
        unit(rig.path());
        let app = ubuntu_app(rig.path(), OLD);
        let mods = modules(rig.path(), Platform::Ubuntu);
        let cli_path = mods
            .join("zcode-acp-server/dist/cli.js")
            .to_string_lossy()
            .into_owned();
        let zcode_bin = app.join("zcode").to_string_lossy().into_owned();
        let zcode_cjs = app
            .join("resources/glm/zcode.cjs")
            .to_string_lossy()
            .into_owned();
        let sessions = [
            process(10, 1, "/usr/bin/node", &["node", &cli_path, "--acp"]),
            process(11, 10, "/usr/bin/node", &["zcode-cli"]),
            process(12, 1, &zcode_bin, &[&zcode_bin, &zcode_cjs, "--version"]),
        ];
        rig.system
            .clone()
            .lookup(feed(Some(page(&[(NEW, "linux-x64")])), None, None));
        for session in sessions {
            *rig.table.lock().expect("table") = vec![session];
            let row = rig.zcode(Platform::Ubuntu).await;
            assert_eq!(
                (
                    row["status"].as_str(),
                    row["messageCode"].as_str(),
                    row["version"].as_str(),
                    row["updateAttempted"].as_bool()
                ),
                (
                    Some("action_required"),
                    Some("in_use"),
                    Some(OLD),
                    Some(false)
                )
            );
            assert!(rig.system.runs().is_empty());
        }
        let unrelated = process(
            13,
            1,
            "/usr/bin/node",
            &["node", "/srv/zcode-acp-server-notes/cli.js"],
        );
        assert!(t3_sessions(&[unrelated]).is_empty());
    }

    /// test_ubuntu_one_unit_run_serves_both_rows (+ phase sequence)
    #[tokio::test]
    async fn ubuntu_one_unit_run_serves_both_rows() {
        let rig = Rig::start();
        unit(rig.path());
        let app = ubuntu_app(rig.path(), OLD);
        let mods = modules(rig.path(), Platform::Ubuntu);
        write_package(&mods, "@brokkai/muse-acp", "0.10.0");
        write_package(&mods, "zcode-acp-server", "0.65.1");
        rig.system.clone().lookup(feed(
            Some(page(&[(NEW, "linux-x64")])),
            Some("0.10.1"),
            Some("0.65.1"),
        ));
        let app_for_run = app.clone();
        let mods_for_run = mods.clone();
        rig.system.clone().answering(move |_spec| {
            write_asar(&app_for_run.join("resources"), NEW, "@zcode/desktop", 0);
            write_package(&mods_for_run, "@brokkai/muse-acp", "0.10.1");
            Ok(CommandOutput::default())
        });
        let shared = Arc::new(Shared::default());
        let zcx = rig.cx_shared(Platform::Ubuntu, 900, shared.clone());
        let zrow = attempt(&ZCODE, &zcx).await.to_value();
        let acx = rig.cx_shared(Platform::Ubuntu, 900, shared);
        let arow = attempt(&ADAPTERS, &acx).await.to_value();
        assert_eq!(
            rig.system.run_strings(),
            vec![UNIT_START.map(str::to_owned).to_vec()]
        );
        assert!(rig.system.timeouts()[0] <= Duration::from_secs(UNIT_SECONDS));
        assert_eq!(
            (
                zrow["status"].as_str(),
                zrow["messageCode"].as_str(),
                zrow["previousVersion"].as_str(),
                zrow["version"].as_str(),
                zrow["manager"].as_str(),
                zrow["updateAttempted"].as_bool()
            ),
            (
                Some("updated"),
                Some("updated"),
                Some(OLD),
                Some(NEW),
                Some("official-download"),
                Some(true)
            )
        );
        assert_eq!(
            (
                arow["status"].as_str(),
                arow["messageCode"].as_str(),
                arow["previousVersion"].as_str(),
                arow["version"].as_str(),
                arow["updateAttempted"].as_bool()
            ),
            (Some("updated"), Some("updated"), None, None, Some(true))
        );
        assert_eq!(
            parts_of(&arow),
            vec![
                (
                    "muse-acp".into(),
                    Some("0.10.0".into()),
                    Some("0.10.1".into())
                ),
                (
                    "zcode-acp-server".into(),
                    Some("0.65.1".into()),
                    Some("0.65.1".into())
                ),
            ]
        );
        assert_eq!(
            *rig.phases.lock().expect("phases"),
            vec![
                "checking", "updating", "updating", "checking", "updating", "updating",
            ]
        );
    }

    /// test_ubuntu_without_the_unit_zcode_is_unsupported_and_adapters_install_into_their_owner
    #[tokio::test]
    async fn ubuntu_without_the_unit_zcode_is_unsupported_and_adapters_install_into_their_owner() {
        let mut rig = Rig::start();
        ubuntu_app(rig.path(), OLD);
        let mods = modules(rig.path(), Platform::Ubuntu);
        write_package(&mods, "@brokkai/muse-acp", "0.10.0");
        write_package(&mods, "zcode-acp-server", "0.65.1");
        let (node, cli) = write_npm_pair(&rig.path().join("node"), Platform::Ubuntu);
        rig.search_path
            .push(node.parent().expect("bin").to_path_buf());
        rig.system.clone().lookup(feed(
            Some(page(&[(NEW, "linux-x64")])),
            Some("0.10.1"),
            Some("0.65.1"),
        ));
        let mods_for_run = mods.clone();
        let node_str = node.to_string_lossy().into_owned();
        let cli_str = cli.to_string_lossy().into_owned();
        rig.system.clone().answering(move |spec| {
            let argv: Vec<String> = spec
                .argv
                .iter()
                .map(|word| word.to_string_lossy().into_owned())
                .collect();
            if argv.len() >= 3 && argv[0] == node_str && argv[1] == cli_str && argv[2] == "install"
            {
                write_package(&mods_for_run, "@brokkai/muse-acp", "0.10.1");
            }
            Ok(CommandOutput::default())
        });
        let zrow = rig.zcode(Platform::Ubuntu).await;
        let arow = rig.adapters(Platform::Ubuntu).await;
        assert_eq!(
            (
                zrow["status"].as_str(),
                zrow["messageCode"].as_str(),
                zrow["updateAttempted"].as_bool()
            ),
            (Some("failed"), Some("unsupported"), Some(false))
        );
        assert_eq!(
            rig.system.run_strings(),
            vec![vec![
                plain(&node).to_string_lossy().into_owned(),
                plain(&cli).to_string_lossy().into_owned(),
                "install".into(),
                "--global".into(),
                "--prefix".into(),
                plain(&rig.path().join(".local"))
                    .to_string_lossy()
                    .into_owned(),
                "--allow-scripts=@brokkai/muse-acp".into(),
                "@brokkai/muse-acp@0.10.1".into(),
            ]]
        );
        assert_eq!(
            (
                arow["status"].as_str(),
                arow["messageCode"].as_str(),
                arow["updateAttempted"].as_bool()
            ),
            (Some("updated"), Some("updated"), Some(true))
        );
        assert_eq!(
            parts_of(&arow),
            vec![
                (
                    "muse-acp".into(),
                    Some("0.10.0".into()),
                    Some("0.10.1".into())
                ),
                (
                    "zcode-acp-server".into(),
                    Some("0.65.1".into()),
                    Some("0.65.1".into())
                ),
            ]
        );
    }

    /// test_ubuntu_unchanged_after_the_run_is_in_use_while_the_app_runs_else_its_failure
    #[tokio::test]
    async fn ubuntu_unchanged_after_the_run_is_in_use_while_the_app_runs_else_its_failure() {
        let rig = Rig::start();
        unit(rig.path());
        let app = ubuntu_app(rig.path(), OLD);
        let zcode_bin = app.join("zcode").to_string_lossy().into_owned();
        let gui = process(20, 1, &zcode_bin, &[&zcode_bin]);
        rig.system
            .clone()
            .lookup(feed(Some(page(&[(NEW, "linux-x64")])), None, None));
        let cases = [
            (
                vec![gui],
                Ok(CommandOutput::default()),
                "action_required",
                "in_use",
            ),
            (
                Vec::new(),
                Ok(CommandOutput::default()),
                "failed",
                "update_failed",
            ),
            (
                Vec::new(),
                Err(UpdateFailure::new("timeout")),
                "failed",
                "timeout",
            ),
        ];
        for (running, outcome, status, code) in cases {
            rig.system.runs.lock().expect("runs").clear();
            {
                let mut script = rig.system.scans_script.lock().expect("script");
                script.clear();
                script.push_back(Vec::new());
                script.push_back(running);
            }
            rig.system.clone().answering(move |_| outcome.clone());
            let row = rig.zcode(Platform::Ubuntu).await;
            assert_eq!(
                (
                    row["status"].as_str(),
                    row["messageCode"].as_str(),
                    row["version"].as_str(),
                    row["updateAttempted"].as_bool()
                ),
                (Some(status), Some(code), Some(OLD), Some(true))
            );
            assert_eq!(rig.system.runs().len(), 1);
        }
    }

    /// test_ubuntu_unit_wait_is_bounded_by_the_host_deadline
    #[tokio::test]
    async fn ubuntu_unit_wait_is_bounded_by_the_host_deadline() {
        let rig = Rig::start();
        unit(rig.path());
        ubuntu_app(rig.path(), OLD);
        rig.system
            .clone()
            .lookup(feed(Some(page(&[(NEW, "linux-x64")])), None, None));
        for (remaining, bound) in [(100_u64, 100_u64), (3600, UNIT_SECONDS)] {
            rig.system.runs.lock().expect("runs").clear();
            let cx = rig.cx(Platform::Ubuntu, remaining);
            let install = detect_zcode(&cx).expect("zcode");
            let _ = zcode_update(&cx, &install, &Lookups::default(), &*rig.services).await;
            let timeout = rig.system.timeouts()[0];
            assert!(timeout <= Duration::from_secs(bound));
            assert!(timeout > Duration::from_secs(bound - 5));
        }
        rig.system.runs.lock().expect("runs").clear();
        let past = Instant::now() - Duration::from_secs(1);
        let cx = rig.cx_deadline(Platform::Ubuntu, past, Arc::new(Shared::default()));
        let install = detect_zcode(&cx).expect("zcode");
        let row = zcode_update(&cx, &install, &Lookups::default(), &*rig.services)
            .await
            .to_value();
        assert_eq!(
            (
                row["status"].as_str(),
                row["messageCode"].as_str(),
                row["updateAttempted"].as_bool()
            ),
            (Some("failed"), Some("timeout"), Some(false))
        );
        assert!(rig.system.runs().is_empty());
    }

    /// test_ubuntu_adapters_run_the_unit_when_the_zcode_row_did_not_unless_an_adapter_runs
    #[tokio::test]
    async fn ubuntu_adapters_run_the_unit_when_the_zcode_row_did_not_unless_an_adapter_runs() {
        let rig = Rig::start();
        unit(rig.path());
        let mods = modules(rig.path(), Platform::Ubuntu);
        write_package(&mods, "@brokkai/muse-acp", "0.10.0");
        write_package(&mods, "zcode-acp-server", "0.65.1");
        let muse_bin = rig
            .path()
            .join(".local/bin/muse-acp")
            .to_string_lossy()
            .into_owned();
        let acp_cli = mods
            .join("zcode-acp-server/dist/cli.js")
            .to_string_lossy()
            .into_owned();
        let muse = process(30, 1, "/usr/bin/node", &["node", &muse_bin]);
        let acp = process(31, 1, "/usr/bin/node", &["node", &acp_cli]);
        rig.system
            .clone()
            .lookup(feed(None, Some("0.10.1"), Some("0.65.1")));
        for running in [vec![muse.clone()], vec![acp]] {
            *rig.table.lock().expect("table") = running;
            let row = rig.adapters(Platform::Ubuntu).await;
            assert_eq!(
                (
                    row["status"].as_str(),
                    row["messageCode"].as_str(),
                    row["updateAttempted"].as_bool()
                ),
                (Some("action_required"), Some("in_use"), Some(false))
            );
            assert_eq!(
                parts_of(&row),
                vec![
                    (
                        "muse-acp".into(),
                        Some("0.10.0".into()),
                        Some("0.10.0".into())
                    ),
                    (
                        "zcode-acp-server".into(),
                        Some("0.65.1".into()),
                        Some("0.65.1".into())
                    ),
                ]
            );
            assert!(rig.system.runs().is_empty());
        }
        rig.table.lock().expect("table").clear();
        let mods_for_run = mods.clone();
        rig.system.clone().answering(move |_| {
            write_package(&mods_for_run, "@brokkai/muse-acp", "0.10.1");
            Ok(CommandOutput::default())
        });
        let row = rig.adapters(Platform::Ubuntu).await;
        assert_eq!(row["status"], "updated");
        assert_eq!(
            rig.system.run_strings(),
            vec![UNIT_START.map(str::to_owned).to_vec()]
        );
        // An adapter still running after an unchanged run is in_use; with none left it failed.
        rig.system
            .clone()
            .lookup(feed(None, Some("0.10.2"), Some("0.65.1")));
        rig.system
            .clone()
            .answering(|_| Ok(CommandOutput::default()));
        for (running, status, code) in [
            (vec![muse], "action_required", "in_use"),
            (Vec::new(), "failed", "update_failed"),
        ] {
            {
                let mut script = rig.system.scans_script.lock().expect("script");
                script.clear();
                script.push_back(Vec::new());
                script.push_back(running);
            }
            let row = rig.adapters(Platform::Ubuntu).await;
            assert_eq!(
                (
                    row["status"].as_str(),
                    row["messageCode"].as_str(),
                    row["updateAttempted"].as_bool()
                ),
                (Some(status), Some(code), Some(true))
            );
        }
    }

    /// test_unreadable_feed_or_registry_fails_without_running_anything
    #[tokio::test]
    async fn unreadable_feed_or_registry_fails_without_running_anything() {
        let rig = Rig::start();
        unit(rig.path());
        ubuntu_app(rig.path(), OLD);
        let mods = modules(rig.path(), Platform::Ubuntu);
        write_package(&mods, "@brokkai/muse-acp", "0.10.0");
        write_package(&mods, "zcode-acp-server", "0.65.1");
        rig.system.clone().lookup(feed(None, None, None));
        let cx = rig.cx(Platform::Ubuntu, 900);
        let zinstall = detect_zcode(&cx).expect("zcode");
        let zrow = zcode_update(&cx, &zinstall, &Lookups::default(), &*rig.services)
            .await
            .to_value();
        let arow = rig.adapters(Platform::Ubuntu).await;
        assert_eq!(
            (
                zrow["status"].as_str(),
                zrow["messageCode"].as_str(),
                zrow["version"].as_str(),
                zrow["updateAttempted"].as_bool()
            ),
            (
                Some("failed"),
                Some("zcode_release_unavailable"),
                Some(OLD),
                Some(false)
            )
        );
        assert_eq!(
            (
                arow["status"].as_str(),
                arow["messageCode"].as_str(),
                arow["updateAttempted"].as_bool()
            ),
            (Some("failed"), Some("update_failed"), Some(false))
        );
        assert_eq!(
            parts_of(&arow),
            vec![
                (
                    "muse-acp".into(),
                    Some("0.10.0".into()),
                    Some("0.10.0".into())
                ),
                (
                    "zcode-acp-server".into(),
                    Some("0.65.1".into()),
                    Some("0.65.1".into())
                ),
            ]
        );
        assert!(rig.system.runs().is_empty());
    }

    /// test_ubuntu_partial_run_with_an_adapter_still_running_holds_the_unchanged_part
    #[tokio::test]
    async fn ubuntu_partial_run_with_an_adapter_still_running_holds_the_unchanged_part() {
        let rig = Rig::start();
        unit(rig.path());
        let mods = modules(rig.path(), Platform::Ubuntu);
        write_package(&mods, "@brokkai/muse-acp", "0.10.0");
        write_package(&mods, "zcode-acp-server", "0.65.0");
        let acp_cli = mods
            .join("zcode-acp-server/dist/cli.js")
            .to_string_lossy()
            .into_owned();
        let acp = process(50, 1, "/usr/bin/node", &["node", &acp_cli]);
        {
            let mut script = rig.system.scans_script.lock().expect("script");
            script.push_back(Vec::new());
            script.push_back(vec![acp]);
        }
        rig.system
            .clone()
            .lookup(feed(None, Some("0.10.1"), Some("0.65.1")));
        let mods_for_run = mods.clone();
        rig.system.clone().answering(move |_| {
            write_package(&mods_for_run, "@brokkai/muse-acp", "0.10.1");
            Ok(CommandOutput::default())
        });
        let row = rig.adapters(Platform::Ubuntu).await;
        assert_eq!(rig.system.runs().len(), 1);
        assert_eq!(
            (
                row["status"].as_str(),
                row["messageCode"].as_str(),
                row["updateAttempted"].as_bool()
            ),
            (Some("action_required"), Some("in_use"), Some(true))
        );
        assert_eq!(
            row["parts"],
            json!([
                {"name": "muse-acp", "previousVersion": "0.10.0", "version": "0.10.1"},
                {"name": "zcode-acp-server", "previousVersion": "0.65.0", "version": "0.65.0", "inUse": true},
            ])
        );
    }

    /// test_adapter_rows_always_carry_both_parts_and_null_top_level_versions
    #[tokio::test]
    async fn adapter_rows_always_carry_both_parts_and_null_top_level_versions() {
        let rig = Rig::start();
        let mods = modules(rig.path(), Platform::Ubuntu);
        write_package(&mods, "@brokkai/muse-acp", "0.10.0");
        rig.system.clone().lookup(feed(None, Some("0.10.0"), None));
        let row = rig.adapters(Platform::Ubuntu).await;
        assert_eq!(
            (
                row["status"].as_str(),
                row["previousVersion"].as_str(),
                row["version"].as_str()
            ),
            (Some("current"), None, None)
        );
        assert_eq!(
            parts_of(&row),
            vec![
                (
                    "muse-acp".into(),
                    Some("0.10.0".into()),
                    Some("0.10.0".into())
                ),
                ("zcode-acp-server".into(), None, None),
            ]
        );
        assert!(rig.system.runs().is_empty());
    }

    /// test_unsettled_ubuntu_oneshot_blocks_npm_and_t3_without_stopping_the_unit
    #[tokio::test]
    async fn unsettled_ubuntu_oneshot_blocks_npm_and_t3_without_stopping_the_unit() {
        let rig = Rig::start();
        let mods = modules(rig.path(), Platform::Ubuntu);
        write_package(&mods, "@brokkai/muse-acp", "0.10.0");
        let shared = Arc::new(Shared::default());
        rig.system
            .clone()
            .answering(|_| Err(UpdateFailure::new("timeout")));
        let cx = rig.cx_shared(Platform::Ubuntu, 600, shared.clone());
        assert_eq!(run_unit(&cx).await.expect("unit"), Some("timeout"));
        assert!(shared.adapter_unit_unsettled.load(Ordering::SeqCst));
        rig.system.clone().answering(|_| {
            Ok(CommandOutput {
                stdout: "start\n".into(),
            })
        });
        assert!(adapter_unit_busy(&cx).await);
        // While unsettled, attempt() refuses Ubuntu npm work with action_required/busy.
        let blocked = attempt(
            &ADAPTERS,
            &rig.cx_shared(Platform::Ubuntu, 600, shared.clone()),
        )
        .await
        .to_value();
        assert_eq!(
            (blocked["status"].as_str(), blocked["messageCode"].as_str()),
            (Some("action_required"), Some("busy"))
        );
        rig.system.clone().answering(|_| {
            Ok(CommandOutput {
                stdout: "dead\n".into(),
            })
        });
        assert!(!adapter_unit_busy(&cx).await);
        assert!(!shared.adapter_unit_unsettled.load(Ordering::SeqCst));
    }

    /// test_partial_adapter_update_fails_only_for_unfinished_required_parts
    #[tokio::test]
    async fn partial_adapter_update_fails_only_for_unfinished_required_parts() {
        let rig = Rig::start();
        let mods = modules(rig.path(), Platform::Ubuntu);
        write_package(&mods, "@brokkai/muse-acp", "1.0.0");
        write_package(&mods, "zcode-acp-server", "1.0.0");
        let nothing = Lookups::default();
        let layout = discover_owners(&rig.host(Platform::Ubuntu), &nothing);
        write_package(&mods, "@brokkai/muse-acp", "2.0.0");
        let targets: Parts = [Some("2.0.0".into()), Some("2.0.0".into())];
        let cx = rig.cx(Platform::Ubuntu, 600);
        let row = adapters_settle(
            &cx,
            &nothing,
            &layout,
            &[MUSE, ACP],
            &targets,
            Codes::default(),
            &mut HashSet::new(),
            true,
            Some(Some("timeout")),
        )
        .await
        .expect("settle")
        .to_value();
        assert_eq!(
            (row["status"].as_str(), row["messageCode"].as_str()),
            (Some("failed"), Some("timeout"))
        );
        assert_eq!(row["parts"].as_array().expect("parts").len(), 2);
        let row = adapters_settle(
            &cx,
            &nothing,
            &layout,
            &[MUSE],
            &targets,
            Codes::default(),
            &mut HashSet::new(),
            true,
            Some(Some("timeout")),
        )
        .await
        .expect("settle")
        .to_value();
        assert_eq!(row["status"], "updated");
    }

    /// Fix round 2 C3 + Lane D R2-3: unit sibling/desktop downgrade protection and down-registry naming.
    #[tokio::test]
    async fn ubuntu_unit_safety_protects_siblings_desktop_and_names_down_registry() {
        // 1. C3: unit with a newer sibling uses exact direct install instead of the shared unit.
        {
            let mut rig = Rig::start();
            unit(rig.path());
            let mods = modules(rig.path(), Platform::Ubuntu);
            write_package(&mods, "@brokkai/muse-acp", "0.22.0");
            write_package(&mods, "zcode-acp-server", "0.84.2");
            let (node, _cli) = write_npm_pair(&rig.path().join("node"), Platform::Ubuntu);
            rig.search_path
                .push(node.parent().expect("bin").to_path_buf());
            rig.system
                .clone()
                .lookup(feed(None, Some("0.21.0"), Some("0.84.3")));
            let mods_for_run = mods.clone();
            rig.system.clone().answering(move |spec| {
                let argv: Vec<String> = spec
                    .argv
                    .iter()
                    .map(|word| word.to_string_lossy().into_owned())
                    .collect();
                assert_ne!(argv[0], "/usr/bin/systemctl", "unit must not run");
                let pkg = argv.last().expect("pkg");
                let (name, ver) = pkg.rsplit_once('@').expect("ver");
                write_package(&mods_for_run, name, ver);
                Ok(CommandOutput::default())
            });
            let row = rig.adapters(Platform::Ubuntu).await;
            assert_eq!(row["status"], "updated");
            assert_eq!(package_version(&mods, MUSE).as_deref(), Some("0.22.0"));
            assert_eq!(
                rig.system.run_strings()[0].last().map(String::as_str),
                Some("zcode-acp-server@0.84.3")
            );
        }
        // 2. C3: zcode unit cannot downgrade an adapter sibling.
        {
            let rig = Rig::start();
            unit(rig.path());
            ubuntu_app(rig.path(), OLD);
            let mods = modules(rig.path(), Platform::Ubuntu);
            write_package(&mods, "@brokkai/muse-acp", "0.22.0");
            rig.system.clone().lookup(feed(
                Some(page(&[(NEW, "linux-x64")])),
                Some("0.21.0"),
                None,
            ));
            let row = rig.zcode(Platform::Ubuntu).await;
            assert_eq!(
                (row["status"].as_str(), row["messageCode"].as_str()),
                (Some("skipped"), Some("shared_updater_unsafe"))
            );
            assert!(rig.system.runs().is_empty());
        }
        // 3. C3: adapter unit verifies the Ubuntu desktop it can touch (downgrade -> version_went_down;
        //    unreadable desktop version -> shared_updater_unsafe).
        {
            let rig = Rig::start();
            unit(rig.path());
            let app = ubuntu_app(rig.path(), NEW);
            let mods = modules(rig.path(), Platform::Ubuntu);
            write_package(&mods, "@brokkai/muse-acp", "0.21.0");
            write_package(&mods, "zcode-acp-server", "0.84.2");
            rig.system
                .clone()
                .lookup(feed(None, Some("0.21.0"), Some("0.84.3")));
            let app_for_run = app.clone();
            let mods_for_run = mods.clone();
            rig.system.clone().answering(move |_| {
                write_asar(&app_for_run.join("resources"), OLD, "@zcode/desktop", 0);
                write_package(&mods_for_run, "zcode-acp-server", "0.84.3");
                Ok(CommandOutput::default())
            });
            let row = rig.adapters(Platform::Ubuntu).await;
            assert_eq!(
                (row["status"].as_str(), row["messageCode"].as_str()),
                (Some("failed"), Some("version_went_down"))
            );
            // Unreadable desktop version refuses the shared unit before running it.
            write_asar(&app.join("resources"), NEW, "impostor", 0);
            write_package(&mods, "zcode-acp-server", "0.84.2");
            rig.system.runs.lock().expect("runs").clear();
            let row = rig.adapters(Platform::Ubuntu).await;
            assert_eq!(
                (row["status"].as_str(), row["messageCode"].as_str()),
                (Some("skipped"), Some("shared_updater_unsafe"))
            );
            assert!(rig.system.runs().is_empty());
        }
        // 4. R2-3: a down registry is named `registry_unreadable`, not blamed on shared_updater_unsafe.
        {
            let rig = Rig::start();
            unit(rig.path());
            ubuntu_app(rig.path(), OLD);
            let mods = modules(rig.path(), Platform::Ubuntu);
            write_package(&mods, "zcode-acp-server", "0.84.2");
            rig.system
                .clone()
                .lookup(feed(Some(page(&[(NEW, "linux-x64")])), None, None));
            let row = rig.zcode(Platform::Ubuntu).await;
            assert_eq!(
                (
                    row["status"].as_str(),
                    row["messageCode"].as_str(),
                    row["updateAttempted"].as_bool(),
                    row["diagnostics"]["reason"].as_str()
                ),
                (
                    Some("failed"),
                    Some("registry_unreadable"),
                    Some(false),
                    Some("registry_unreadable")
                )
            );
            assert!(rig.system.runs().is_empty());
        }
    }

    // ------------------------------------------------------------ Mac

    type FailCheck = Box<dyn FnMut(&[String]) -> bool + Send>;

    #[derive(Default)]
    struct MacOptions {
        gui: bool,
        session: bool,
        opened_later: bool,
        served: Option<Vec<u8>>,
        entries: Option<Vec<(String, Vec<u8>)>>,
        fail: Option<FailCheck>,
    }

    async fn run_mac_fixture(
        mut options: MacOptions,
    ) -> (Value, Vec<Vec<String>>, Vec<String>, PathBuf) {
        let mut rig = Rig::start();
        rig.arch = "aarch64";
        let bundle = rig
            .host(Platform::Mac)
            .system("/Applications")
            .join(MAC_NAME);
        mac_bundle(&bundle, OLD, MAC_ID);
        let archive_path = rig.path().join("fixture.zip");
        let default_entries = vec![
            (
                format!("{MAC_NAME}/Contents/Info.plist"),
                plist_xml_bytes(NEW, MAC_ID),
            ),
            (
                format!("{MAC_NAME}/Contents/MacOS/ZCode"),
                b"fixture".to_vec(),
            ),
        ];
        let entries = options.entries.take().unwrap_or(default_entries);
        let borrowed: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(name, data)| (name.as_str(), data.as_slice()))
            .collect();
        write_stored_zip(&archive_path, &borrowed);
        let archive_bytes = std::fs::read(&archive_path).expect("zip bytes");
        let asset = format!("ZCode-{NEW}-mac-arm64.zip");
        let manifest_text = manifest(NEW, &asset, &archive_bytes, true);
        let served = options.served.take().unwrap_or(archive_bytes);
        rig.system
            .clone()
            .lookup(feed(Some(page(&[(NEW, "macos-arm64")])), None, None));
        rig.system.clone().serving(move |url, target| {
            if url.ends_with("latest.yml") {
                std::fs::write(target, &manifest_text).expect("manifest");
            } else {
                std::fs::write(target, &served).expect("package");
            }
            Ok(())
        });
        let main = bundle
            .join("Contents/MacOS/ZCode")
            .to_string_lossy()
            .into_owned();
        let helper = format!("{main} Helper");
        let zcode_cjs = bundle
            .join("Contents/Resources/glm/zcode.cjs")
            .to_string_lossy()
            .into_owned();
        let mut table = Vec::new();
        if options.gui {
            table.push(process(40, 1, &main, &[]));
            table.push(process(41, 40, &helper, &[]));
        }
        if options.session {
            table.push(process(50, 1, "/opt/homebrew/bin/node", &[]));
            table.push(process(51, 50, &main, &[]));
        }
        *rig.services.arguments.lock().expect("args") = vec![
            (40, vec![main.clone()]),
            (41, vec![helper, "--type=renderer".into()]),
            (
                50,
                vec![
                    "node".into(),
                    "/opt/homebrew/lib/node_modules/zcode-acp-server/dist/cli.js".into(),
                ],
            ),
            (51, vec![main.clone(), zcode_cjs]),
            (60, vec![main.clone()]),
        ];
        if options.opened_later {
            let mut later = table.clone();
            later.push(process(60, 1, &main, &[]));
            let mut script = rig.system.scans_script.lock().expect("script");
            script.push_back(table);
            script.push_back(later);
        } else {
            *rig.table.lock().expect("table") = table;
        }
        let mut fail = options.fail.take();
        rig.system.clone().answering(move |spec| {
            let argv: Vec<String> = spec
                .argv
                .iter()
                .map(|word| word.to_string_lossy().into_owned())
                .collect();
            if let Some(check) = fail.as_mut()
                && check(&argv)
            {
                return Err(UpdateFailure::failed());
            }
            if argv.len() >= 5 && argv[0] == "/usr/bin/ditto" && argv[1] == "-x" {
                extract_stored_zip(Path::new(&argv[3]), Path::new(&argv[4]));
            } else if argv.len() >= 3 && argv[0] == "/usr/bin/ditto" {
                copy_tree(Path::new(&argv[1]), Path::new(&argv[2]))?;
            }
            Ok(CommandOutput::default())
        });
        let cx = rig.cx(Platform::Mac, 900);
        let install = detect_zcode(&cx).expect("mac zcode");
        let row = Rig::finish(
            &cx,
            zcode_update(&cx, &install, &Lookups::default(), &*rig.services).await,
        );
        let calls = rig.system.run_strings();
        let downloads: Vec<String> = rig
            .system
            .downloads()
            .into_iter()
            .map(|(url, _)| url)
            .collect();
        let version_after = mac_version(&bundle);
        let entries_after = dir_entries(bundle.parent().expect("Applications"));
        // Verify on-disk state before TempDir drops via the returned bundle check tuple.
        let mut out = row;
        out["_versionAfter"] = json!(version_after);
        out["_appsAfter"] = json!(entries_after);
        (out, calls, downloads, bundle)
    }

    /// test_mac_running_gui_reports_quit_first_before_any_download
    #[tokio::test]
    async fn mac_running_gui_reports_quit_first_before_any_download() {
        let (row, calls, downloads, _) = run_mac_fixture(MacOptions {
            gui: true,
            session: true,
            ..MacOptions::default()
        })
        .await;
        assert_eq!(
            (
                row["status"].as_str(),
                row["messageCode"].as_str(),
                row["version"].as_str(),
                row["updateAttempted"].as_bool()
            ),
            (
                Some("action_required"),
                Some("quit_first"),
                Some(OLD),
                Some(false)
            )
        );
        assert!(calls.is_empty());
        assert!(downloads.is_empty());
    }

    /// test_mac_t3_session_alone_is_in_use_not_quit_first
    #[tokio::test]
    async fn mac_t3_session_alone_is_in_use_not_quit_first() {
        let (row, calls, downloads, _) = run_mac_fixture(MacOptions {
            session: true,
            ..MacOptions::default()
        })
        .await;
        assert_eq!(
            (
                row["status"].as_str(),
                row["messageCode"].as_str(),
                row["updateAttempted"].as_bool()
            ),
            (Some("action_required"), Some("in_use"), Some(false))
        );
        assert!(calls.is_empty());
        assert!(downloads.is_empty());
    }

    /// test_mac_installs_the_verified_bundle_with_an_inline_requirement_and_never_opens_it
    #[tokio::test]
    async fn mac_installs_the_verified_bundle_with_an_inline_requirement_and_never_opens_it() {
        let (row, calls, downloads, bundle) = run_mac_fixture(MacOptions::default()).await;
        assert_eq!(
            (
                row["status"].as_str(),
                row["messageCode"].as_str(),
                row["previousVersion"].as_str(),
                row["version"].as_str(),
                row["updateAttempted"].as_bool(),
                row["forcedStops"].as_u64()
            ),
            (
                Some("updated"),
                Some("updated"),
                Some(OLD),
                Some(NEW),
                Some(true),
                Some(0)
            )
        );
        assert_eq!(row["_versionAfter"], NEW);
        assert_eq!(
            downloads,
            vec![
                format!("{RELEASES}{NEW}/macos-arm64/latest.yml"),
                format!("{RELEASES}{NEW}/macos-arm64/ZCode-{NEW}-mac-arm64.zip"),
            ]
        );
        let requirement = "-R=identifier \"dev.zcode.app\" and anchor apple generic and certificate leaf[subject.OU] = \"8A5X4JJ39T\"";
        assert!(calls.contains(&vec![
            "/usr/bin/codesign".into(),
            "--verify".into(),
            requirement.into(),
            bundle.to_string_lossy().into_owned(),
        ]));
        assert!(calls.contains(&vec![
            "/usr/sbin/spctl".into(),
            "--assess".into(),
            "--type".into(),
            "execute".into(),
            bundle.to_string_lossy().into_owned(),
        ]));
        assert!(!calls.iter().any(|call| call[0] == "/usr/bin/open"));
        assert_eq!(row["_appsAfter"], json!([MAC_NAME]));
    }

    /// test_mac_verification_failures_never_swap
    #[tokio::test]
    async fn mac_verification_failures_never_swap() {
        let older = vec![(
            format!("{MAC_NAME}/Contents/Info.plist"),
            plist_xml_bytes("3.14.3", MAC_ID),
        )];
        let cases: [(&str, MacOptions); 5] = [
            (
                "sha512",
                MacOptions {
                    served: Some(b"tampered".to_vec()),
                    ..MacOptions::default()
                },
            ),
            (
                "version",
                MacOptions {
                    entries: Some(older),
                    ..MacOptions::default()
                },
            ),
            (
                "codesign",
                MacOptions {
                    fail: Some(Box::new(|argv| {
                        argv.len() >= 3 && argv[..3] == ["/usr/bin/codesign", "--verify", "--deep"]
                    })),
                    ..MacOptions::default()
                },
            ),
            (
                "team",
                MacOptions {
                    fail: Some(Box::new(|argv| {
                        argv.len() >= 3
                            && argv[0] == "/usr/bin/codesign"
                            && argv[2].starts_with("-R=")
                    })),
                    ..MacOptions::default()
                },
            ),
            (
                "spctl",
                MacOptions {
                    fail: Some(Box::new(|argv| argv[0] == "/usr/sbin/spctl")),
                    ..MacOptions::default()
                },
            ),
        ];
        for (name, options) in cases {
            let (row, calls, _, _) = run_mac_fixture(options).await;
            assert_eq!(
                (
                    row["status"].as_str(),
                    row["messageCode"].as_str(),
                    row["version"].as_str()
                ),
                (Some("failed"), Some("signature_failed"), Some(OLD)),
                "{name}"
            );
            assert_eq!(row["_versionAfter"], OLD, "{name}");
            assert_eq!(row["_appsAfter"], json!([MAC_NAME]), "{name}");
            if name == "sha512" {
                assert!(
                    calls.is_empty(),
                    "nothing extracted from unverified package"
                );
            }
        }
    }

    /// test_mac_archive_escape_is_rejected_before_extraction
    #[tokio::test]
    async fn mac_archive_escape_is_rejected_before_extraction() {
        let (row, calls, _, _) = run_mac_fixture(MacOptions {
            entries: Some(vec![("../escape".into(), b"fixture".to_vec())]),
            ..MacOptions::default()
        })
        .await;
        assert_eq!(
            (row["status"].as_str(), row["messageCode"].as_str()),
            (Some("failed"), Some("signature_failed"))
        );
        assert!(
            !calls
                .iter()
                .any(|call| call.len() >= 2 && call[..2] == ["/usr/bin/ditto", "-x"])
        );
    }

    /// test_mac_final_check_failure_rolls_back_the_swap
    #[tokio::test]
    async fn mac_final_check_failure_rolls_back_the_swap() {
        let mut checks = 0;
        let (row, _, _, _) = run_mac_fixture(MacOptions {
            fail: Some(Box::new(move |argv| {
                if argv.len() >= 3 && argv[..3] == ["/usr/bin/codesign", "--verify", "--deep"] {
                    checks += 1;
                    return checks == 3;
                }
                false
            })),
            ..MacOptions::default()
        })
        .await;
        assert_eq!(
            (
                row["status"].as_str(),
                row["messageCode"].as_str(),
                row["version"].as_str(),
                row["updateAttempted"].as_bool()
            ),
            (
                Some("failed"),
                Some("signature_failed"),
                Some(OLD),
                Some(true)
            )
        );
        assert_eq!(row["_versionAfter"], OLD);
        assert_eq!(row["_appsAfter"], json!([MAC_NAME]));
    }

    /// test_mac_app_opened_during_the_download_is_never_swapped
    #[tokio::test]
    async fn mac_app_opened_during_the_download_is_never_swapped() {
        let (row, _, downloads, _) = run_mac_fixture(MacOptions {
            opened_later: true,
            ..MacOptions::default()
        })
        .await;
        assert_eq!(
            (
                row["status"].as_str(),
                row["messageCode"].as_str(),
                row["version"].as_str()
            ),
            (Some("action_required"), Some("quit_first"), Some(OLD))
        );
        assert_eq!(downloads.len(), 2);
        assert_eq!(row["_versionAfter"], OLD);
        assert_eq!(row["_appsAfter"], json!([MAC_NAME]));
    }

    /// Unwritable /Applications fails `unsupported` before any download.
    #[cfg(unix)]
    #[tokio::test]
    async fn mac_unwritable_applications_folder_is_unsupported_before_download() {
        use std::os::unix::fs::PermissionsExt as _;
        if std::fs::metadata("/proc/self").is_ok_and(|info| {
            use std::os::unix::fs::MetadataExt as _;
            info.uid() == 0
        }) {
            return;
        }
        let mut rig = Rig::start();
        rig.arch = "aarch64";
        let apps = rig.host(Platform::Mac).system("/Applications");
        let bundle = apps.join(MAC_NAME);
        mac_bundle(&bundle, OLD, MAC_ID);
        rig.system
            .clone()
            .lookup(feed(Some(page(&[(NEW, "macos-arm64")])), None, None));
        std::fs::set_permissions(&apps, std::fs::Permissions::from_mode(0o555)).expect("chmod");
        let cx = rig.cx(Platform::Mac, 900);
        let install = detect_zcode(&cx).expect("mac zcode");
        let row = zcode_update(&cx, &install, &Lookups::default(), &*rig.services)
            .await
            .to_value();
        std::fs::set_permissions(&apps, std::fs::Permissions::from_mode(0o755)).expect("restore");
        assert_eq!(
            (row["status"].as_str(), row["messageCode"].as_str()),
            (Some("failed"), Some("unsupported"))
        );
        assert!(rig.system.downloads().is_empty());
    }

    /// test_mac_adapters_install_bound_to_the_owner_prefix_and_never_wait_for_running_adapters
    #[tokio::test]
    async fn mac_adapters_install_bound_to_the_owner_prefix_and_never_wait_for_running_adapters() {
        let mut rig = Rig::start();
        let mods = modules(rig.path(), Platform::Mac);
        let prefix = mods
            .parent()
            .and_then(Path::parent)
            .expect("homebrew")
            .to_path_buf();
        write_package(&mods, "@brokkai/muse-acp", "0.10.0");
        write_package(&mods, "zcode-acp-server", "0.65.1");
        let (node, cli) = write_npm_pair(&rig.path().join("node"), Platform::Mac);
        rig.search_path
            .push(node.parent().expect("bin").to_path_buf());
        rig.system
            .clone()
            .lookup(feed(None, Some("0.10.1"), Some("0.65.1")));
        let layout = discover_owners(&rig.host(Platform::Mac), &Lookups::default());
        assert_eq!(
            layout.owners[MUSE]
                .iter()
                .map(|owner| (owner.prefix.clone(), owner.source))
                .collect::<Vec<_>>(),
            vec![(prefix.clone(), "default")]
        );
        let outcomes: [(Option<UpdateFailure>, bool, &str, &str); 3] = [
            (None, true, "updated", "updated"),
            (None, false, "failed", "update_failed"),
            (
                Some(UpdateFailure::new("timeout")),
                false,
                "failed",
                "timeout",
            ),
        ];
        let mut last_row = Value::Null;
        for (err, mutate, status, code) in outcomes {
            write_package(&mods, "@brokkai/muse-acp", "0.10.0");
            rig.system.runs.lock().expect("runs").clear();
            let mods_for_run = mods.clone();
            rig.system.clone().answering(move |_| {
                if let Some(err) = &err {
                    return Err(err.clone());
                }
                if mutate {
                    write_package(&mods_for_run, "@brokkai/muse-acp", "0.10.1");
                }
                Ok(CommandOutput::default())
            });
            let row = rig.adapters(Platform::Mac).await;
            assert_eq!(
                (
                    row["status"].as_str(),
                    row["messageCode"].as_str(),
                    row["updateAttempted"].as_bool()
                ),
                (Some(status), Some(code), Some(true))
            );
            assert_eq!(
                rig.system.run_strings(),
                vec![vec![
                    plain(&node).to_string_lossy().into_owned(),
                    plain(&cli).to_string_lossy().into_owned(),
                    "install".into(),
                    "--global".into(),
                    "--prefix".into(),
                    plain(&prefix).to_string_lossy().into_owned(),
                    "--allow-scripts=@brokkai/muse-acp".into(),
                    "@brokkai/muse-acp@0.10.1".into(),
                ]]
            );
            assert!(rig.system.timeouts()[0] <= Duration::from_secs(NPM_SECONDS));
            assert_eq!(
                rig.system.scans(),
                0,
                "macOS never scans for running adapters"
            );
            last_row = row;
        }
        assert_eq!(
            parts_of(&last_row),
            vec![
                (
                    "muse-acp".into(),
                    Some("0.10.0".into()),
                    Some("0.10.0".into())
                ),
                (
                    "zcode-acp-server".into(),
                    Some("0.65.1".into()),
                    Some("0.65.1".into())
                ),
            ]
        );
    }

    // ------------------------------------------------------------ Windows ZCode

    struct WindowsOptions {
        running: Vec<u32>,
        session: u32,
        signer_err: Option<UpdateFailure>,
        installer_err: Option<UpdateFailure>,
        reopen_err: Option<UpdateFailure>,
        extra: Vec<Process>,
    }

    impl Default for WindowsOptions {
        fn default() -> Self {
            Self {
                running: Vec::new(),
                session: 1,
                signer_err: None,
                installer_err: None,
                reopen_err: None,
                extra: Vec::new(),
            }
        }
    }

    struct WindowsOutcome {
        row: Value,
        events: Vec<&'static str>,
        closed: Vec<Vec<u32>>,
        reopened: Vec<Vec<u32>>,
        version_after: Option<String>,
        new_file_exists: bool,
    }

    async fn run_windows_zcode(options: WindowsOptions) -> WindowsOutcome {
        let rig = Rig::start();
        let executable = windows_app(rig.path(), OLD);
        let data = b"MZ fixture installer".to_vec();
        let asset = format!("ZCode-{NEW}-win-x64.exe");
        let manifest_text = manifest(NEW, &asset, &data, true);
        let mut table = options.extra;
        let exe_str = executable.to_string_lossy().into_owned();
        for pid in options.running {
            let mut main = process(pid, 4, &exe_str, &[&exe_str]);
            main.session = Some(options.session);
            let mut child = process(pid + 1, pid, &exe_str, &[&exe_str, "--type=renderer"]);
            child.session = Some(options.session);
            table.push(main);
            table.push(child);
        }
        *rig.table.lock().expect("table") = table;
        *rig.services.reopen.lock().expect("reopen") = options.reopen_err;
        rig.system
            .clone()
            .lookup(feed(Some(page(&[(NEW, "windows-x64")])), None, None));
        let events = rig.services.events.clone();
        let events_dl = events.clone();
        rig.system.clone().serving(move |url, target| {
            events_dl.lock().expect("events").push("download");
            if url.ends_with("latest.yml") {
                std::fs::write(target, &manifest_text).expect("manifest");
            } else {
                std::fs::write(target, &data).expect("installer");
            }
            Ok(())
        });
        let events_cmd = events.clone();
        let folder = executable.parent().expect("folder").to_path_buf();
        let signer_err = options.signer_err;
        let installer_err = options.installer_err;
        rig.system.clone().answering(move |spec| {
            let text = script(&spec.argv);
            if text.contains("Get-AuthenticodeSignature") {
                events_cmd.lock().expect("events").push("verify");
                if let Some(err) = &signer_err {
                    return Err(err.clone());
                }
                return Ok(CommandOutput::default());
            }
            if text.contains("Start-Process") {
                events_cmd.lock().expect("events").push("install");
                assert!(text.contains(&asset), "installer asset in script: {text}");
                write_asar(&folder.join("resources"), NEW, "@zcode/desktop", 0);
                std::fs::write(folder.join("new-file.dll"), b"").expect("new-file");
                if let Some(err) = &installer_err {
                    return Err(err.clone());
                }
                return Ok(CommandOutput::default());
            }
            Ok(CommandOutput::default())
        });
        let cx = rig.cx(Platform::Windows, 900);
        let install = detect_zcode(&cx).expect("windows zcode");
        let row = Rig::finish(
            &cx,
            zcode_update(&cx, &install, &Lookups::default(), &*rig.services).await,
        );
        WindowsOutcome {
            row,
            events: events.lock().expect("events").clone(),
            closed: rig.services.closed.lock().expect("closed").clone(),
            reopened: rig.services.reopened.lock().expect("reopened").clone(),
            version_after: installed_version(Platform::Windows, &executable),
            new_file_exists: executable
                .parent()
                .expect("folder")
                .join("new-file.dll")
                .exists(),
        }
    }

    /// test_windows_running_zcode_downloads_first_then_closes_installs_and_reopens
    #[tokio::test]
    async fn windows_running_zcode_downloads_first_then_closes_installs_and_reopens() {
        let out = run_windows_zcode(WindowsOptions {
            running: vec![100],
            ..WindowsOptions::default()
        })
        .await;
        assert_eq!(
            out.events,
            vec![
                "download", "download", "verify", "close", "install", "reopen"
            ]
        );
        assert_eq!(out.closed, vec![vec![100]]);
        assert_eq!(out.reopened, vec![vec![100]]);
        assert_eq!(
            (
                out.row["status"].as_str(),
                out.row["messageCode"].as_str(),
                out.row["previousVersion"].as_str(),
                out.row["version"].as_str(),
                out.row["manager"].as_str()
            ),
            (
                Some("updated"),
                Some("desktop_reopened"),
                Some(OLD),
                Some(NEW),
                Some("official-download")
            )
        );
        assert_eq!(
            (
                out.row["restartedProcesses"].as_u64(),
                out.row["forcedStops"].as_u64(),
                out.row["updateAttempted"].as_bool(),
                out.row["restartTargets"].clone()
            ),
            (Some(1), Some(1), Some(true), json!([{"kind": "desktop"}]))
        );
    }

    /// test_windows_not_running_installs_without_closing_or_reopening
    #[tokio::test]
    async fn windows_not_running_installs_without_closing_or_reopening() {
        let out = run_windows_zcode(WindowsOptions::default()).await;
        assert_eq!(
            out.events,
            vec!["download", "download", "verify", "install"]
        );
        assert_eq!(
            (
                out.row["status"].as_str(),
                out.row["messageCode"].as_str(),
                out.row["version"].as_str(),
                out.row["restartedProcesses"].as_u64()
            ),
            (Some("updated"), Some("updated"), Some(NEW), Some(0))
        );
        assert!(out.row.get("restartTargets").is_none());
        assert!(out.closed.is_empty());
        assert!(out.reopened.is_empty());
    }

    /// test_windows_signer_mismatch_is_signature_failed_and_nothing_closes
    #[tokio::test]
    async fn windows_signer_mismatch_is_signature_failed_and_nothing_closes() {
        let out = run_windows_zcode(WindowsOptions {
            running: vec![100],
            signer_err: Some(UpdateFailure::failed()),
            ..WindowsOptions::default()
        })
        .await;
        assert_eq!(
            (
                out.row["status"].as_str(),
                out.row["messageCode"].as_str(),
                out.row["version"].as_str(),
                out.row["updateAttempted"].as_bool()
            ),
            (
                Some("failed"),
                Some("signature_failed"),
                Some(OLD),
                Some(false)
            )
        );
        assert_eq!(out.events, vec!["download", "download", "verify"]);
        assert!(out.closed.is_empty());
        // Verify the Authenticode PowerShell script directly.
        let rig = Rig::start();
        let installed = windows_app(rig.path(), OLD);
        let package = rig.path().join("ZCode-3.14.5-win-x64.exe");
        let cx = rig.cx(Platform::Windows, 900);
        verify_signer(&cx, &package, &installed)
            .await
            .expect("verify");
        let ps = script(&rig.system.runs()[0]);
        for part in [
            format!(
                "Get-AuthenticodeSignature -LiteralPath '{}'",
                package.display()
            ),
            format!(
                "Get-AuthenticodeSignature -LiteralPath '{}'",
                installed.display()
            ),
            "$n.Status -ne 'Valid'".to_owned(),
            "$o.Status -ne 'Valid'".to_owned(),
            "$n.SignerCertificate.Subject -cne $o.SignerCertificate.Subject".to_owned(),
            "Contains('SERIALNUMBER=91110108MA01KP2T5U')".to_owned(),
        ] {
            assert!(ps.contains(&part), "missing {part} in {ps}");
        }
        assert!(!ps.contains("Write-Output"));
    }

    /// test_windows_installer_failure_restores_the_rollback_copy_and_reopens_the_old_zcode
    #[tokio::test]
    async fn windows_installer_failure_restores_the_rollback_copy_and_reopens_the_old_zcode() {
        let out = run_windows_zcode(WindowsOptions {
            running: vec![100],
            installer_err: Some(UpdateFailure::failed()),
            ..WindowsOptions::default()
        })
        .await;
        assert_eq!(
            (
                out.row["status"].as_str(),
                out.row["messageCode"].as_str(),
                out.row["previousVersion"].as_str(),
                out.row["version"].as_str(),
                out.row["updateAttempted"].as_bool()
            ),
            (
                Some("failed"),
                Some("update_failed"),
                Some(OLD),
                Some(OLD),
                Some(true)
            )
        );
        assert_eq!(out.version_after.as_deref(), Some(OLD));
        assert!(!out.new_file_exists);
        assert_eq!(
            &out.events[out.events.len() - 3..],
            ["install", "close", "reopen"]
        );
        assert_eq!(out.reopened, vec![vec![100]]);
        let timed = run_windows_zcode(WindowsOptions {
            running: vec![100],
            installer_err: Some(UpdateFailure::new("timeout")),
            ..WindowsOptions::default()
        })
        .await;
        assert_eq!(
            (
                timed.row["status"].as_str(),
                timed.row["messageCode"].as_str(),
                timed.row["version"].as_str()
            ),
            (Some("failed"), Some("timeout"), Some(OLD))
        );
    }

    /// test_windows_reopen_failure_is_restart_failed_with_the_new_version
    #[tokio::test]
    async fn windows_reopen_failure_is_restart_failed_with_the_new_version() {
        let out = run_windows_zcode(WindowsOptions {
            running: vec![100],
            reopen_err: Some(UpdateFailure::new("restart_failed")),
            ..WindowsOptions::default()
        })
        .await;
        assert_eq!(
            (
                out.row["status"].as_str(),
                out.row["messageCode"].as_str(),
                out.row["previousVersion"].as_str(),
                out.row["version"].as_str(),
                out.row["forcedStops"].as_u64()
            ),
            (
                Some("restart_failed"),
                Some("restart_failed"),
                Some(OLD),
                Some(NEW),
                Some(1)
            )
        );
        assert!(
            out.row.get("restartTargets").is_none(),
            "restart_failed omits restartTargets"
        );
        assert_eq!(out.version_after.as_deref(), Some(NEW));
    }

    /// test_windows_t3_session_is_in_use_and_another_session_is_quit_first_before_any_download
    #[tokio::test]
    async fn windows_t3_session_is_in_use_and_another_session_is_quit_first_before_any_download() {
        let mut node = process(
            70,
            4,
            "C:/Program Files/nodejs/node.exe",
            &[
                "node.exe",
                "C:/Users/user/AppData/Roaming/npm/node_modules/zcode-acp-server/dist/cli.js",
            ],
        );
        node.session = Some(1);
        let out = run_windows_zcode(WindowsOptions {
            extra: vec![node],
            ..WindowsOptions::default()
        })
        .await;
        assert_eq!(
            (
                out.row["status"].as_str(),
                out.row["messageCode"].as_str(),
                out.row["updateAttempted"].as_bool()
            ),
            (Some("action_required"), Some("in_use"), Some(false))
        );
        assert!(out.events.is_empty());
        assert!(out.closed.is_empty());

        let other = run_windows_zcode(WindowsOptions {
            running: vec![100],
            session: 2,
            ..WindowsOptions::default()
        })
        .await;
        assert_eq!(
            (
                other.row["status"].as_str(),
                other.row["messageCode"].as_str(),
                other.row["version"].as_str()
            ),
            (Some("action_required"), Some("quit_first"), Some(OLD))
        );
        assert!(other.events.is_empty());
        assert!(other.closed.is_empty());
    }

    // ------------------------------------------------------------ Windows adapters

    #[derive(Clone)]
    enum NpmOutcome {
        Updated,
        Unchanged,
        Fail(UpdateFailure),
    }

    async fn run_windows_npm(
        installed: [(&'static str, &'static str); 2],
        latest: [&'static str; 2],
        running: Vec<Process>,
        outcomes: [NpmOutcome; 2],
        with_git_bash: bool,
    ) -> (
        Value,
        Vec<(Vec<String>, Duration)>,
        PathBuf,
        PathBuf,
        PathBuf,
        PathBuf,
    ) {
        let mut rig = Rig::start();
        let mods = modules(rig.path(), Platform::Windows);
        for (package, version) in installed {
            write_package(&mods, package, version);
        }
        let prefix = rig.path().join("roaming/npm");
        let (node, cli) =
            write_npm_pair(&rig.path().join("Program Files/nodejs"), Platform::Windows);
        rig.search_path
            .push(node.parent().expect("nodejs").to_path_buf());
        let bash = rig.path().join("Program Files/Git/bin/bash.exe");
        let mut env = std::collections::HashMap::new();
        if with_git_bash {
            std::fs::create_dir_all(bash.parent().expect("bin")).expect("git bin");
            std::fs::write(&bash, b"").expect("bash");
            env.insert(
                "ProgramFiles".to_owned(),
                rig.path()
                    .join("Program Files")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        *rig.table.lock().expect("table") = running;
        rig.system
            .clone()
            .lookup(feed(None, Some(latest[MUSE]), Some(latest[ACP])));
        let mods_for_run = mods.clone();
        rig.system.clone().answering(move |spec| {
            let pkg = spec.argv.last().expect("pkg").to_string_lossy();
            let adapter = (0..PARTS)
                .find(|adapter| pkg.starts_with(&format!("{}@", PACKAGES[*adapter].1)))
                .expect("adapter");
            match &outcomes[adapter] {
                NpmOutcome::Fail(err) => Err(err.clone()),
                NpmOutcome::Unchanged => Ok(CommandOutput::default()),
                NpmOutcome::Updated => {
                    write_package(&mods_for_run, PACKAGES[adapter].1, latest[adapter]);
                    Ok(CommandOutput::default())
                }
            }
        });
        let vars = Lookups(env);
        let cx = rig.cx(Platform::Windows, 900);
        let row = Rig::finish(&cx, adapters_update(&cx, &vars).await);
        let runs: Vec<(Vec<String>, Duration)> = rig
            .system
            .run_strings()
            .into_iter()
            .zip(rig.system.timeouts())
            .collect();
        (row, runs, prefix, node, cli, bash)
    }

    fn muse_running_under(root: &Path) -> Process {
        let exe = root
            .join("roaming/npm/node_modules/@brokkai/muse-acp/native/x86_64-pc-windows-msvc/muse-acp.exe")
            .to_string_lossy()
            .into_owned();
        let mut p = process(80, 4, &exe, &[&exe]);
        p.session = Some(1);
        p
    }

    /// test_windows_adapters_each_install_in_its_own_npm_run_with_git_bash_as_script_shell
    #[tokio::test]
    async fn windows_adapters_each_install_in_its_own_npm_run_with_git_bash_as_script_shell() {
        let (row, runs, prefix, node, cli, bash) = run_windows_npm(
            [
                ("@brokkai/muse-acp", "0.10.0"),
                ("zcode-acp-server", "0.65.0"),
            ],
            ["0.10.1", "0.65.1"],
            Vec::new(),
            [NpmOutcome::Updated, NpmOutcome::Updated],
            true,
        )
        .await;
        let argvs: Vec<Vec<String>> = runs.iter().map(|(argv, _)| argv.clone()).collect();
        assert_eq!(
            argvs,
            vec![
                vec![
                    plain(&node).to_string_lossy().into_owned(),
                    plain(&cli).to_string_lossy().into_owned(),
                    "install".into(),
                    "--global".into(),
                    "--prefix".into(),
                    plain(&prefix).to_string_lossy().into_owned(),
                    "--script-shell".into(),
                    plain(&bash).to_string_lossy().into_owned(),
                    "--allow-scripts=@brokkai/muse-acp".into(),
                    "@brokkai/muse-acp@0.10.1".into(),
                ],
                vec![
                    plain(&node).to_string_lossy().into_owned(),
                    plain(&cli).to_string_lossy().into_owned(),
                    "install".into(),
                    "--global".into(),
                    "--prefix".into(),
                    plain(&prefix).to_string_lossy().into_owned(),
                    "--script-shell".into(),
                    plain(&bash).to_string_lossy().into_owned(),
                    "--allow-scripts=zcode-acp-server".into(),
                    "zcode-acp-server@0.65.1".into(),
                ],
            ]
        );
        assert!(
            runs.iter()
                .all(|(_, timeout)| *timeout <= Duration::from_secs(NPM_SECONDS))
        );
        assert_eq!(
            (
                row["status"].as_str(),
                row["messageCode"].as_str(),
                row["updateAttempted"].as_bool()
            ),
            (Some("updated"), Some("updated"), Some(true))
        );
        assert_eq!(
            parts_of(&row),
            vec![
                (
                    "muse-acp".into(),
                    Some("0.10.0".into()),
                    Some("0.10.1".into())
                ),
                (
                    "zcode-acp-server".into(),
                    Some("0.65.0".into()),
                    Some("0.65.1".into())
                ),
            ]
        );
        assert!(
            row["parts"]
                .as_array()
                .expect("parts")
                .iter()
                .all(|p| p.get("inUse").is_none())
        );
    }

    /// test_windows_current_package_is_never_reinstalled
    #[tokio::test]
    async fn windows_current_package_is_never_reinstalled() {
        let (row, runs, ..) = run_windows_npm(
            [
                ("@brokkai/muse-acp", "0.10.1"),
                ("zcode-acp-server", "0.65.1"),
            ],
            ["0.10.1", "0.65.1"],
            Vec::new(),
            [NpmOutcome::Updated, NpmOutcome::Updated],
            true,
        )
        .await;
        assert_eq!(
            (
                row["status"].as_str(),
                row["messageCode"].as_str(),
                row["updateAttempted"].as_bool()
            ),
            (Some("current"), Some("current"), Some(false))
        );
        assert!(runs.is_empty());
        let (_, runs, ..) = run_windows_npm(
            [
                ("@brokkai/muse-acp", "0.10.0"),
                ("zcode-acp-server", "0.65.1"),
            ],
            ["0.10.1", "0.65.1"],
            Vec::new(),
            [NpmOutcome::Updated, NpmOutcome::Updated],
            true,
        )
        .await;
        assert_eq!(
            runs.iter()
                .map(|(argv, _)| argv.last().expect("pkg").as_str())
                .collect::<Vec<_>>(),
            vec!["@brokkai/muse-acp@0.10.1"]
        );
    }

    /// test_windows_zcode_postinstall_failure_leaves_muse_acp_updated
    #[tokio::test]
    async fn windows_zcode_postinstall_failure_leaves_muse_acp_updated() {
        let (row, runs, ..) = run_windows_npm(
            [
                ("@brokkai/muse-acp", "0.10.0"),
                ("zcode-acp-server", "0.65.0"),
            ],
            ["0.10.1", "0.65.1"],
            Vec::new(),
            [
                NpmOutcome::Updated,
                NpmOutcome::Fail(UpdateFailure::failed()),
            ],
            true,
        )
        .await;
        assert_eq!(runs.len(), 2);
        assert_eq!(
            (
                row["status"].as_str(),
                row["messageCode"].as_str(),
                row["updateAttempted"].as_bool()
            ),
            (Some("failed"), Some("update_failed"), Some(true))
        );
        assert_eq!(
            parts_of(&row),
            vec![
                (
                    "muse-acp".into(),
                    Some("0.10.0".into()),
                    Some("0.10.1".into())
                ),
                (
                    "zcode-acp-server".into(),
                    Some("0.65.0".into()),
                    Some("0.65.0".into())
                ),
            ]
        );
    }

    /// test_windows_running_muse_is_never_installed_while_zcode_still_updates + running muse alone
    #[tokio::test]
    async fn windows_running_muse_is_held_while_free_zcode_still_updates() {
        let mut rig = Rig::start();
        let mods = modules(rig.path(), Platform::Windows);
        write_package(&mods, "@brokkai/muse-acp", "0.10.0");
        write_package(&mods, "zcode-acp-server", "0.65.0");
        let (node, _) = write_npm_pair(&rig.path().join("Program Files/nodejs"), Platform::Windows);
        rig.search_path
            .push(node.parent().expect("nodejs").to_path_buf());
        *rig.table.lock().expect("table") = vec![muse_running_under(rig.path())];
        rig.system
            .clone()
            .lookup(feed(None, Some("0.10.1"), Some("0.65.1")));
        let mods_for_run = mods.clone();
        rig.system.clone().answering(move |spec| {
            let pkg = spec.argv.last().expect("pkg").to_string_lossy();
            if pkg.starts_with("zcode-acp-server@") {
                write_package(&mods_for_run, "zcode-acp-server", "0.65.1");
            }
            Ok(CommandOutput::default())
        });
        let cx = rig.cx(Platform::Windows, 900);
        let row = Rig::finish(&cx, adapters_update(&cx, &Lookups::default()).await);
        assert_eq!(
            rig.system
                .run_strings()
                .iter()
                .map(|argv| argv.last().expect("pkg").as_str())
                .collect::<Vec<_>>(),
            vec!["zcode-acp-server@0.65.1"]
        );
        assert_eq!(
            (
                row["status"].as_str(),
                row["messageCode"].as_str(),
                row["updateAttempted"].as_bool()
            ),
            (Some("action_required"), Some("in_use"), Some(true))
        );
        assert_eq!(
            row["parts"],
            json!([
                {"name": "muse-acp", "previousVersion": "0.10.0", "version": "0.10.0", "inUse": true},
                {"name": "zcode-acp-server", "previousVersion": "0.65.0", "version": "0.65.1"},
            ])
        );

        // Now zcode-acp-server is already current; only running muse is behind -> npm never runs.
        rig.system.runs.lock().expect("runs").clear();
        let cx = rig.cx(Platform::Windows, 900);
        let row = Rig::finish(&cx, adapters_update(&cx, &Lookups::default()).await);
        assert!(rig.system.runs().is_empty());
        assert_eq!(
            (
                row["status"].as_str(),
                row["messageCode"].as_str(),
                row["updateAttempted"].as_bool()
            ),
            (Some("action_required"), Some("in_use"), Some(false))
        );
        assert_eq!(
            row["parts"]
                .as_array()
                .expect("parts")
                .iter()
                .map(|p| p.get("inUse").and_then(Value::as_bool).unwrap_or(false))
                .collect::<Vec<_>>(),
            vec![true, false]
        );
    }

    /// test_windows_denied_without_a_process_is_permission_but_ebusy_is_in_use
    #[tokio::test]
    async fn windows_denied_without_a_process_is_permission_but_ebusy_is_in_use() {
        let escaped = [
            r"npm error code EPERM",
            r"npm error syscall rmdir",
            r"npm error path C:\\Users\\user\\AppData\\Roaming\\npm\\node_modules\\@brokkai\\muse-acp\\native",
            r"npm error Error: EPERM: operation not permitted, rmdir 'C:\Users\user\AppData\Roaming\npm\node_modules\@brokkai\muse-acp\native'",
        ]
        .join("\n");
        let (row, ..) = run_windows_npm(
            [
                ("@brokkai/muse-acp", "0.10.0"),
                ("zcode-acp-server", "0.65.0"),
            ],
            ["0.10.1", "0.65.1"],
            Vec::new(),
            [
                NpmOutcome::Fail(UpdateFailure {
                    output: escaped,
                    ..UpdateFailure::failed()
                }),
                NpmOutcome::Updated,
            ],
            true,
        )
        .await;
        assert_eq!(
            (
                row["status"].as_str(),
                row["messageCode"].as_str(),
                row["updateAttempted"].as_bool()
            ),
            (
                Some("action_required"),
                Some("prefix_not_writable"),
                Some(true)
            )
        );
        assert_eq!(
            row["parts"],
            json!([
                {"name": "muse-acp", "previousVersion": "0.10.0", "version": "0.10.0"},
                {"name": "zcode-acp-server", "previousVersion": "0.65.0", "version": "0.65.1"},
            ])
        );

        let busy = [
            r"npm error code EBUSY",
            r"npm error path C:\Users\user\AppData\Roaming\npm\node_modules\zcode-acp-server",
            r"npm error Error: EBUSY: resource busy or locked, unlink 'C:\Users\user\AppData\Roaming\npm\node_modules\zcode-acp-server\dist\cli.js'",
        ]
        .join("\n");
        let (row, ..) = run_windows_npm(
            [
                ("@brokkai/muse-acp", "0.10.0"),
                ("zcode-acp-server", "0.65.0"),
            ],
            ["0.10.1", "0.65.1"],
            Vec::new(),
            [
                NpmOutcome::Updated,
                NpmOutcome::Fail(UpdateFailure {
                    output: busy,
                    ..UpdateFailure::failed()
                }),
            ],
            true,
        )
        .await;
        assert_eq!(
            (row["status"].as_str(), row["messageCode"].as_str()),
            (Some("action_required"), Some("in_use"))
        );
        assert_eq!(
            row["parts"],
            json!([
                {"name": "muse-acp", "previousVersion": "0.10.0", "version": "0.10.1"},
                {"name": "zcode-acp-server", "previousVersion": "0.65.0", "version": "0.65.0", "inUse": true},
            ])
        );
    }

    /// test_windows_npm_permission_error_naming_no_such_package_is_a_failure + unchanged + no git bash
    #[tokio::test]
    async fn windows_npm_unrelated_eperm_unchanged_and_missing_git_bash_fail_honestly() {
        let other = "npm error code EPERM\nnpm error path C:\\Users\\user\\AppData\\Local\\npm-cache\\_cacache\\tmp";
        let cross = "npm error code EPERM\nnpm error path C:\\Users\\user\\AppData\\Roaming\\npm\\node_modules\\zcode-acp-server\\dist";
        for text in [other, cross] {
            let (row, ..) = run_windows_npm(
                [
                    ("@brokkai/muse-acp", "0.10.0"),
                    ("zcode-acp-server", "0.65.1"),
                ],
                ["0.10.1", "0.65.1"],
                Vec::new(),
                [
                    NpmOutcome::Fail(UpdateFailure {
                        output: text.into(),
                        ..UpdateFailure::failed()
                    }),
                    NpmOutcome::Updated,
                ],
                true,
            )
            .await;
            assert_eq!(
                (row["status"].as_str(), row["messageCode"].as_str()),
                (Some("failed"), Some("update_failed"))
            );
            assert_eq!(
                parts_of(&row),
                vec![
                    (
                        "muse-acp".into(),
                        Some("0.10.0".into()),
                        Some("0.10.0".into())
                    ),
                    (
                        "zcode-acp-server".into(),
                        Some("0.65.1".into()),
                        Some("0.65.1".into())
                    ),
                ]
            );
        }
        // Clean npm run that moves nothing is a failure.
        let (row, ..) = run_windows_npm(
            [
                ("@brokkai/muse-acp", "0.10.0"),
                ("zcode-acp-server", "0.65.0"),
            ],
            ["0.10.1", "0.65.1"],
            Vec::new(),
            [NpmOutcome::Unchanged, NpmOutcome::Unchanged],
            true,
        )
        .await;
        assert_eq!(
            (
                row["status"].as_str(),
                row["messageCode"].as_str(),
                row["updateAttempted"].as_bool()
            ),
            (Some("failed"), Some("update_failed"), Some(true))
        );
        // Without Git Bash, npm gets no --script-shell and postinstall failure is visible.
        let (row, runs, ..) = run_windows_npm(
            [
                ("@brokkai/muse-acp", "0.10.0"),
                ("zcode-acp-server", "0.65.0"),
            ],
            ["0.10.1", "0.65.1"],
            Vec::new(),
            [
                NpmOutcome::Updated,
                NpmOutcome::Fail(UpdateFailure::failed()),
            ],
            false,
        )
        .await;
        assert!(
            runs.iter()
                .all(|(argv, _)| !argv.iter().any(|a| a == "--script-shell"))
        );
        assert_eq!(
            (row["status"].as_str(), row["messageCode"].as_str()),
            (Some("failed"), Some("update_failed"))
        );
    }

    /// test_git_bash_is_found_only_in_git_for_windows_own_bin
    #[test]
    fn git_bash_is_found_only_in_git_for_windows_own_bin() {
        let rig = Rig::start();
        let base = rig.path();
        let git = base.join("Program Files/Git/bin/bash.exe");
        std::fs::create_dir_all(git.parent().expect("bin")).expect("git bin");
        std::fs::write(&git, b"").expect("bash");
        let system = base.join("Windows/System32/bash.exe");
        std::fs::create_dir_all(system.parent().expect("sys32")).expect("sys32");
        std::fs::write(&system, b"").expect("sys32 bash");
        let mut env = std::collections::HashMap::new();
        env.insert(
            "ProgramFiles".to_owned(),
            base.join("Program Files").to_string_lossy().into_owned(),
        );
        assert_eq!(
            git_bash(&rig.host(Platform::Windows), &Lookups(env.clone())),
            Some(git.clone())
        );
        env.insert(
            "ProgramFiles".to_owned(),
            base.join("Nothing").to_string_lossy().into_owned(),
        );
        assert_eq!(
            git_bash(&rig.host(Platform::Windows), &Lookups(env.clone())),
            None
        );
        // Also resolved via git.exe on PATH (`.../Git/cmd/git.exe` -> `.../Git/bin/bash.exe`).
        let git_cmd = base.join("Program Files/Git/cmd/git.exe");
        std::fs::create_dir_all(git_cmd.parent().expect("cmd")).expect("cmd dir");
        std::fs::write(&git_cmd, b"").expect("git.exe");
        let mut host = Arc::into_inner(rig.host(Platform::Windows)).expect("host");
        host.path.push(git_cmd.parent().expect("cmd").to_path_buf());
        assert_eq!(git_bash(&host, &Lookups(env)), Some(git));
    }

    /// test_adapter_owners_match_the_muse_native_exe_in_any_letter_case_on_windows
    #[test]
    fn adapter_owners_match_the_muse_native_exe_in_any_letter_case_on_windows() {
        let root = PathBuf::from(r"C:\Users\user\AppData\Roaming\npm\node_modules");
        let roots: [Vec<PathBuf>; PARTS] = [vec![root.clone()], vec![root.clone()]];
        let native = r"C:\Users\user\AppData\Roaming\npm\node_modules\@brokkai\muse-acp\native\x86_64-pc-windows-msvc\muse-acp.exe";
        for exe in [
            native.to_owned(),
            native.to_lowercase(),
            native.to_uppercase(),
            r"c:\Users\User\AppData\Roaming\NPM\node_modules\@Brokkai\Muse-Acp\native\x86_64-pc-windows-msvc\muse-acp.exe".to_owned(),
        ] {
            let item = process(40, 4, &exe, &[&exe]);
            assert_eq!(adapter_owners(&item, &roots), [true, false], "{exe}");
        }
        let node = r"C:\Program Files\nodejs\node.exe";
        let script_path =
            r"C:\Users\user\AppData\Roaming\npm\node_modules\@brokkai\muse-acp\bin\muse-acp.cjs";
        assert_eq!(
            adapter_owners(&process(41, 4, node, &[node, script_path]), &roots),
            [true, false]
        );
        let martty = r"C:\Users\user\AppData\Roaming\npm\node_modules\zcode-acp-server\node_modules\zcode-acp-martty\vendor\win32-x64\martty.exe";
        assert_eq!(
            adapter_owners(&process(42, 4, martty, &[martty]), &roots),
            [false, true]
        );
        let cli = r"C:\Users\user\AppData\Roaming\npm\node_modules\zcode-acp-server\dist\cli.js";
        assert_eq!(
            adapter_owners(&process(43, 4, node, &[node, cli]), &roots),
            [false, true]
        );
        let sibling = r"C:\Users\user\AppData\Roaming\npm\node_modules\@brokkai\muse-acp-tools\muse-acp-tools.exe";
        assert_eq!(
            adapter_owners(&process(44, 4, sibling, &[sibling]), &roots),
            [false, false]
        );
    }

    /// test_only_npm_error_lines_naming_a_package_folder_lock_that_package
    /// + test_windows_postinstall_failure_with_a_rollback_warning_is_failed_not_in_use
    #[tokio::test]
    async fn only_npm_error_lines_naming_a_package_folder_lock_that_package() {
        let escaped = "npm error code EPERM\nnpm error path C:\\\\Users\\\\user\\\\AppData\\\\Roaming\\\\npm\\\\node_modules\\\\@brokkai\\\\muse-acp\\\\native";
        let plain = "npm error code EBUSY\nnpm error path C:\\Users\\user\\AppData\\Roaming\\npm\\node_modules\\@brokkai\\muse-acp\\native";
        for text in [escaped, plain] {
            assert!(locked_here(text, MUSE), "{text}");
            assert!(!locked_here(text, ACP), "{text}");
        }
        let warning = "npm warn cleanup [ 'C:\\\\Users\\\\user\\\\AppData\\\\Roaming\\\\npm\\\\node_modules\\\\@brokkai\\\\muse-acp', [Error: EPERM: operation not permitted, rmdir 'C:\\Users\\user\\AppData\\Roaming\\npm\\node_modules\\@brokkai\\muse-acp\\native'] ]\nnpm error code 1\nnpm error path C:\\Users\\user\\AppData\\Roaming\\npm\\node_modules\\@brokkai\\muse-acp";
        assert!(!locked_here(warning, MUSE));
        assert!(!locked_here(
            "npm error code ENOENT\nnpm error path C:\\Users\\user\\AppData\\Roaming\\npm\\node_modules\\@brokkai\\muse-acp",
            MUSE
        ));
        assert!(!locked_here(
            "npm error code EPERM\nnpm error path C:\\Users\\user\\AppData\\Roaming\\npm\\node_modules\\@brokkai\\muse-acp-tools",
            MUSE
        ));
        // Incident: rollback warning with EPERM + postinstall error code 1 is failed, not in_use.
        let incident = [
            r"npm warn cleanup Failed to remove some directories [",
            r"npm warn cleanup     'C:\\Users\\user\\AppData\\Roaming\\npm\\node_modules\\zcode-acp-server',",
            r"npm warn cleanup     [Error: EPERM: operation not permitted, rmdir 'C:\Users\user\AppData\Roaming\npm\node_modules\zcode-acp-server\node_modules\zod\src'] {",
            r"npm warn cleanup       code: 'EPERM',",
            r"npm error code 1",
            r"npm error path C:\Users\user\AppData\Roaming\npm\node_modules\zcode-acp-server",
            r"npm error command failed",
            r"npm error command C:\WINDOWS\system32\cmd.exe /d /s /c node dist/remote/hub-upgrade-notify.js 2>/dev/null || true",
        ]
        .join("\n");
        let (row, ..) = run_windows_npm(
            [
                ("@brokkai/muse-acp", "0.10.0"),
                ("zcode-acp-server", "0.65.0"),
            ],
            ["0.10.1", "0.65.1"],
            Vec::new(),
            [
                NpmOutcome::Updated,
                NpmOutcome::Fail(UpdateFailure {
                    output: incident,
                    ..UpdateFailure::failed()
                }),
            ],
            true,
        )
        .await;
        assert_eq!(
            (
                row["status"].as_str(),
                row["messageCode"].as_str(),
                row["updateAttempted"].as_bool()
            ),
            (Some("failed"), Some("update_failed"), Some(true))
        );
        assert!(
            row["parts"]
                .as_array()
                .expect("parts")
                .iter()
                .all(|p| p.get("inUse").is_none())
        );
    }

    /// Fix round 2 C8: EPERM/EACCES across syscalls without a running adapter is prefix_not_writable.
    #[tokio::test]
    async fn windows_access_denied_without_a_running_adapter_is_not_a_lock() {
        for errno in ["EPERM", "EACCES"] {
            for syscall in ["rename", "unlink", "mkdir", "scandir"] {
                let output = format!(
                    "npm error code {errno}\nnpm error syscall {syscall}\nnpm error path C:/Program Files/nodejs/node_modules/zcode-acp-server"
                );
                let (row, ..) = run_windows_npm(
                    [
                        ("@brokkai/muse-acp", "0.10.1"),
                        ("zcode-acp-server", "0.84.2"),
                    ],
                    ["0.10.1", "0.84.3"],
                    Vec::new(),
                    [
                        NpmOutcome::Updated,
                        NpmOutcome::Fail(UpdateFailure {
                            output,
                            ..UpdateFailure::failed()
                        }),
                    ],
                    false,
                )
                .await;
                assert_eq!(
                    (row["status"].as_str(), row["messageCode"].as_str()),
                    (Some("action_required"), Some("prefix_not_writable")),
                    "{errno}/{syscall}"
                );
                assert!(
                    row["parts"]
                        .as_array()
                        .expect("parts")
                        .iter()
                        .all(|p| p.get("inUse").is_none())
                );
            }
        }
    }

    // ------------------------------------------------------------ Ownership, managers, fix rounds

    /// Fix round 1 B5: every enabled T3 consumer is updated; one left behind is never `updated`.
    #[tokio::test]
    async fn every_enabled_t3_consumer_of_an_adapter_is_updated_and_one_left_behind_fails() {
        let mut rig = Rig::start();
        let first = write_npm_prefix_adapter(rig.path(), "opt/homebrew", "0.84.2");
        let second = write_npm_prefix_adapter(rig.path(), "second-node", "0.84.2");
        write_t3_consumers(
            rig.path(),
            &[
                first.join("zcode-acp-server/dist/cli.js"),
                second.join("zcode-acp-server/dist/cli.js"),
            ],
        );
        let (node, _) = write_npm_pair(&rig.path().join("node"), Platform::Mac);
        rig.search_path
            .push(node.parent().expect("bin").to_path_buf());
        rig.system.clone().lookup(feed(None, None, Some("0.84.3")));
        rig.system.clone().answering(|spec| {
            let argv: Vec<String> = spec
                .argv
                .iter()
                .map(|word| word.to_string_lossy().into_owned())
                .collect();
            let at = argv
                .iter()
                .position(|word| word == "--prefix")
                .expect("prefix");
            let prefix = PathBuf::from(&argv[at + 1]);
            let (pkg, ver) = argv.last().expect("pkg").rsplit_once('@').expect("ver");
            write_package(&prefix.join("lib/node_modules"), pkg, ver);
            Ok(CommandOutput::default())
        });
        let row = rig.adapters(Platform::Mac).await;
        assert_eq!(
            (
                package_version(&first, ACP).as_deref(),
                package_version(&second, ACP).as_deref()
            ),
            (Some("0.84.3"), Some("0.84.3"))
        );
        assert_eq!(row["status"], "updated");
        assert_eq!(rig.system.runs().len(), 2);

        // Now second consumer stays at 0.84.2 -> never `updated`, lowest version kept.
        write_package(&first, "zcode-acp-server", "0.84.2");
        write_package(&second, "zcode-acp-server", "0.84.2");
        // The install names its prefix plainly (a verbatim test folder on Windows).
        let second_prefix = plain(&rig.path().join("second-node"));
        rig.system.clone().answering(move |spec| {
            let argv: Vec<String> = spec
                .argv
                .iter()
                .map(|word| word.to_string_lossy().into_owned())
                .collect();
            let at = argv
                .iter()
                .position(|word| word == "--prefix")
                .expect("prefix");
            let prefix = PathBuf::from(&argv[at + 1]);
            let (pkg, ver) = argv.last().expect("pkg").rsplit_once('@').expect("ver");
            let written = if prefix == second_prefix {
                "0.84.2"
            } else {
                ver
            };
            write_package(&prefix.join("lib/node_modules"), pkg, written);
            Ok(CommandOutput::default())
        });
        let row = rig.adapters(Platform::Mac).await;
        assert_ne!(row["status"], "updated");
        assert_eq!(row["parts"][1]["version"], "0.84.2");
    }

    /// Fix round 1 B6 + Fix round 2 C3: mirror downgrade and unplanned sibling downgrade.
    #[tokio::test]
    async fn mirror_serving_an_older_latest_is_version_went_down_and_unplanned_siblings_checked() {
        let mut rig = Rig::start();
        let homebrew = write_npm_prefix_adapter(rig.path(), "opt/homebrew", "0.84.2");
        write_t3_consumers(rig.path(), &[homebrew.join("zcode-acp-server/dist/cli.js")]);
        let (node, _) = write_npm_pair(&rig.path().join("node"), Platform::Mac);
        rig.search_path
            .push(node.parent().expect("bin").to_path_buf());
        rig.system.clone().lookup(feed(None, None, Some("0.84.3")));
        let hb_for_run = homebrew.clone();
        rig.system.clone().answering(move |_| {
            write_package(&hb_for_run, "zcode-acp-server", "0.83.0");
            Ok(CommandOutput::default())
        });
        let row = rig.adapters(Platform::Mac).await;
        assert_eq!(
            (row["status"].as_str(), row["messageCode"].as_str()),
            (Some("failed"), Some("version_went_down"))
        );
        assert!(
            rig.system.run_strings()[0]
                .last()
                .expect("pkg")
                .ends_with("@0.84.3")
        );

        // C3: second owner started ahead (0.85.0) and dropped to 0.84.3 -> version_went_down even
        // when behind is empty (sibling outside plan).
        let first = write_npm_prefix_adapter(rig.path(), "first", "0.84.2");
        let second = write_npm_prefix_adapter(rig.path(), "second", "0.85.0");
        write_t3_consumers(
            rig.path(),
            &[
                first.join("zcode-acp-server/dist/cli.js"),
                second.join("zcode-acp-server/dist/cli.js"),
            ],
        );
        let nothing = Lookups::default();
        let layout = discover_owners(&rig.host(Platform::Mac), &nothing);
        write_package(&first, "zcode-acp-server", "0.84.3");
        write_package(&second, "zcode-acp-server", "0.84.3");
        let targets: Parts = [None, Some("0.84.3".into())];
        let cx = rig.cx(Platform::Mac, 600);
        for behind in [&[ACP][..], &[][..]] {
            let row = adapters_settle(
                &cx,
                &nothing,
                &layout,
                behind,
                &targets,
                Codes::default(),
                &mut HashSet::new(),
                true,
                None,
            )
            .await
            .expect("settle")
            .to_value();
            assert_eq!(
                (row["status"].as_str(), row["messageCode"].as_str()),
                (Some("failed"), Some("version_went_down"))
            );
        }
    }

    /// Fix round 1 B7 + Robustness alternate managers + Fix round 2 C6 (pnpm store copy skips).
    #[cfg(unix)]
    #[tokio::test]
    async fn pnpm_and_bun_install_into_their_own_global_folders_never_into_pnpm_store() {
        // 1. B7: pnpm symlinked store installs into global dir with PNPM_HOME.
        {
            let mut rig = Rig::start();
            let global_dir = rig.path().join(".local/share/pnpm/global");
            let mods = global_dir.join("5/node_modules");
            let populate = |version: &str| {
                let entry = mods.join(format!(".pnpm/zcode-acp-server@{version}/node_modules"));
                write_package(&entry, "zcode-acp-server", version);
                let cli = entry.join("zcode-acp-server/dist/cli.js");
                std::fs::create_dir_all(cli.parent().expect("dist")).expect("dist");
                std::fs::write(&cli, b"cli").expect("cli");
                symlink_path(
                    &entry.join("zcode-acp-server"),
                    &mods.join("zcode-acp-server"),
                );
            };
            populate("0.84.2");
            write_t3_consumers(rig.path(), &[mods.join("zcode-acp-server/dist/cli.js")]);
            let tool = rig.path().join("tools/pnpm");
            std::fs::create_dir_all(tool.parent().expect("tools")).expect("tools");
            std::fs::write(&tool, b"x").expect("pnpm");
            make_executable(&tool);
            rig.search_path
                .push(tool.parent().expect("tools").to_path_buf());
            rig.system.clone().lookup(feed(None, None, Some("0.84.3")));
            let global_for_run = global_dir.clone();
            let mods_for_run = mods.clone();
            rig.system.clone().answering(move |spec| {
                let argv: Vec<String> = spec
                    .argv
                    .iter()
                    .map(|word| word.to_string_lossy().into_owned())
                    .collect();
                let at = argv
                    .iter()
                    .position(|w| w == "--global-dir")
                    .expect("global-dir");
                if Path::new(&argv[at + 1]) == global_for_run {
                    let ver = argv.last().expect("pkg").rsplit_once('@').expect("ver").1;
                    let entry =
                        mods_for_run.join(format!(".pnpm/zcode-acp-server@{ver}/node_modules"));
                    write_package(&entry, "zcode-acp-server", ver);
                    symlink_path(
                        &entry.join("zcode-acp-server"),
                        &mods_for_run.join("zcode-acp-server"),
                    );
                }
                Ok(CommandOutput::default())
            });
            let row = rig.adapters(Platform::Ubuntu).await;
            assert_eq!(row["status"], "updated");
            let argv = &rig.system.run_strings()[0];
            assert_eq!(
                argv,
                &vec![
                    tool.to_string_lossy().into_owned(),
                    "add".into(),
                    "--global".into(),
                    "--global-dir".into(),
                    global_dir.to_string_lossy().into_owned(),
                    "zcode-acp-server@0.84.3".into(),
                ]
            );
            let env = &rig.system.run_envs()[0];
            assert!(env.iter().any(|(k, v)| {
                k == "PNPM_HOME" && v == &global_dir.parent().expect("pnpm").to_string_lossy()
            }));
        }
        // 2. bun global install sets BUN_INSTALL_GLOBAL_DIR.
        {
            let mut rig = Rig::start();
            let mods = rig.path().join(".bun/install/global/node_modules");
            write_package(&mods, "zcode-acp-server", "0.84.2");
            let cli = mods.join("zcode-acp-server/dist/cli.js");
            std::fs::create_dir_all(cli.parent().expect("dist")).expect("dist");
            std::fs::write(&cli, b"cli").expect("cli");
            write_t3_consumers(rig.path(), &[cli]);
            let tool = rig.path().join("tools/bun");
            std::fs::create_dir_all(tool.parent().expect("tools")).expect("tools");
            std::fs::write(&tool, b"x").expect("bun");
            make_executable(&tool);
            rig.search_path
                .push(tool.parent().expect("tools").to_path_buf());
            rig.system.clone().lookup(feed(None, None, Some("0.84.3")));
            let mods_for_run = mods.clone();
            rig.system.clone().answering(move |_| {
                write_package(&mods_for_run, "zcode-acp-server", "0.84.3");
                Ok(CommandOutput::default())
            });
            let row = rig.adapters(Platform::Ubuntu).await;
            assert_eq!(row["status"], "updated");
            assert_eq!(
                rig.system.run_strings()[0],
                vec![
                    tool.to_string_lossy().into_owned(),
                    "add".into(),
                    "--global".into(),
                    "zcode-acp-server@0.84.3".into(),
                ]
            );
            assert!(rig.system.run_envs()[0].iter().any(|(k, v)| {
                k == "BUN_INSTALL_GLOBAL_DIR"
                    && v == &mods.parent().expect("global").to_string_lossy()
            }));
        }
        // 3. C6: T3 pointing directly into `.pnpm/...` store skips with `pnpm_store_copy` before lookup.
        {
            let rig = Rig::start();
            let mods = rig.path().join(".local/share/pnpm/global/5/node_modules");
            let entry = mods.join(".pnpm/zcode-acp-server@0.84.2/node_modules");
            write_package(&entry, "zcode-acp-server", "0.84.2");
            let script_path = entry.join("zcode-acp-server/dist/cli.js");
            std::fs::create_dir_all(script_path.parent().expect("dist")).expect("dist");
            std::fs::write(&script_path, b"fixture").expect("cli");
            symlink_path(
                &entry.join("zcode-acp-server"),
                &mods.join("zcode-acp-server"),
            );
            write_t3_consumers(rig.path(), &[script_path]);
            rig.system.clone().lookup(feed(None, None, Some("0.84.3")));
            let row = rig.adapters(Platform::Ubuntu).await;
            assert_eq!(
                (row["status"].as_str(), row["messageCode"].as_str()),
                (Some("skipped"), Some("pnpm_store_copy"))
            );
            assert!(rig.system.asked().is_empty());
            assert!(rig.system.runs().is_empty());
            assert_eq!(package_version(&entry, ACP).as_deref(), Some("0.84.2"));
        }
    }

    /// Fix round 1 B7 + R1-L8: Windows pnpm.cmd and corepack pnpm run through node, never cmd.exe.
    #[test]
    fn windows_pnpm_cmd_and_corepack_pnpm_use_node_never_cmd_exe() {
        let mut rig = Rig::start();
        let shim = rig.path().join("AppData/Roaming/npm/pnpm.cmd");
        let entry = shim
            .parent()
            .expect("npm")
            .join("node_modules/pnpm/bin/pnpm.cjs");
        std::fs::create_dir_all(entry.parent().expect("bin")).expect("bin");
        std::fs::write(&entry, b"x").expect("pnpm.cjs");
        std::fs::write(&shim, b"@echo off").expect("pnpm.cmd");
        let node = rig.path().join("nodejs/node.exe");
        std::fs::create_dir_all(node.parent().expect("nodejs")).expect("nodejs");
        std::fs::write(&node, b"x").expect("node.exe");
        rig.search_path
            .push(shim.parent().expect("npm").to_path_buf());
        rig.search_path
            .push(node.parent().expect("nodejs").to_path_buf());
        let host = rig.host(Platform::Windows);
        let owner = Owner {
            root: rig.path().join("pnpm/global/5/node_modules"),
            prefix: rig.path().join("pnpm/global"),
            manager: "pnpm",
            source: "default",
            node: None,
            bin_dir: None,
        };
        let planned = install_command(&host, &Lookups::default(), &owner, ACP, None, "0.84.3")
            .expect("planned");
        assert_eq!(
            &planned.argv[..2],
            &[node.as_os_str().to_owned(), entry.as_os_str().to_owned()]
        );

        // R1-L8: corepack cache uses lastKnownGood.json first, else newest version.
        std::fs::remove_file(&entry).expect("rm pnpm.cjs");
        let cache = rig.path().join("AppData/Local/node/corepack");
        for ver in ["9.15.4", "10.12.1", "11.0.0"] {
            let cached = cache.join(format!("v1/pnpm/{ver}/bin/pnpm.cjs"));
            std::fs::create_dir_all(cached.parent().expect("bin")).expect("cache dir");
            std::fs::write(&cached, b"x").expect("cached");
        }
        let mut env = std::collections::HashMap::new();
        env.insert(
            "COREPACK_HOME".to_owned(),
            cache.to_string_lossy().into_owned(),
        );
        let vars = Lookups(env);
        assert_eq!(
            pnpm_argv(&host, &vars),
            Some(vec![
                node.to_string_lossy().into_owned(),
                cache
                    .join("v1/pnpm/11.0.0/bin/pnpm.cjs")
                    .to_string_lossy()
                    .into_owned(),
            ])
        );
        std::fs::write(
            cache.join("lastKnownGood.json"),
            json!({"pnpm": "10.12.1"}).to_string(),
        )
        .expect("lkg");
        assert_eq!(
            pnpm_argv(&host, &vars),
            Some(vec![
                node.to_string_lossy().into_owned(),
                cache
                    .join("v1/pnpm/10.12.1/bin/pnpm.cjs")
                    .to_string_lossy()
                    .into_owned(),
            ])
        );
    }

    /// Fix round 1: Windows npm prefers the prefix's own node and T3's configured node.
    #[test]
    fn windows_npm_prefers_the_prefixs_own_node_and_t3s_node() {
        let mut rig = Rig::start();
        let prefix = rig.path().join("nvm/nodejs");
        let other = rig.path().join("Program Files/nodejs");
        let (prefix_node, _) = write_npm_pair(&prefix, Platform::Windows);
        let (_, _) = write_npm_pair(&other, Platform::Windows);
        std::fs::write(other.join("npm.cmd"), b"@echo off").expect("npm.cmd");
        rig.search_path.push(other);
        let host = rig.host(Platform::Windows);
        assert_eq!(
            shared::resolve_npm(&host, &prefix).expect("resolve").0,
            prefix_node
        );
        let owner = Owner {
            root: prefix.join("node_modules"),
            prefix: prefix.clone(),
            manager: "npm",
            source: "t3-config",
            node: Some(prefix_node.clone()),
            bin_dir: None,
        };
        assert_eq!(npm_pair(&host, &owner).expect("pair").0, real(&prefix_node));
    }

    /// Fix round 1 B11 + Lane D R1-L6: unwritable owner runs nothing, sibling still updates, real
    /// failure outranks prefix_not_writable.
    #[cfg(unix)]
    #[tokio::test]
    async fn unwritable_owner_folder_runs_nothing_and_sibling_still_updates() {
        use std::os::unix::fs::PermissionsExt as _;
        if std::fs::metadata("/proc/self").is_ok_and(|info| {
            use std::os::unix::fs::MetadataExt as _;
            info.uid() == 0
        }) {
            return;
        }
        let mut rig = Rig::start();
        let first = write_npm_prefix_adapter(rig.path(), "opt/homebrew", "0.84.2");
        let second = write_npm_prefix_adapter(rig.path(), "second-node", "0.84.2");
        write_t3_consumers(
            rig.path(),
            &[
                first.join("zcode-acp-server/dist/cli.js"),
                second.join("zcode-acp-server/dist/cli.js"),
            ],
        );
        let (node, _) = write_npm_pair(&rig.path().join("node"), Platform::Mac);
        rig.search_path
            .push(node.parent().expect("bin").to_path_buf());
        rig.system.clone().lookup(feed(None, None, Some("0.84.3")));
        rig.system.clone().answering(|spec| {
            let argv: Vec<String> = spec
                .argv
                .iter()
                .map(|word| word.to_string_lossy().into_owned())
                .collect();
            let at = argv.iter().position(|w| w == "--prefix").expect("prefix");
            let prefix = PathBuf::from(&argv[at + 1]);
            let (pkg, ver) = argv.last().expect("pkg").rsplit_once('@').expect("ver");
            write_package(&prefix.join("lib/node_modules"), pkg, ver);
            Ok(CommandOutput::default())
        });
        std::fs::set_permissions(&first, std::fs::Permissions::from_mode(0o555)).expect("chmod");
        let row = rig.adapters(Platform::Mac).await;
        std::fs::set_permissions(&first, std::fs::Permissions::from_mode(0o755)).expect("restore");
        assert_eq!(package_version(&second, ACP).as_deref(), Some("0.84.3"));
        assert_eq!(rig.system.runs().len(), 1);
        assert!(
            rig.system.run_strings()[0]
                .iter()
                .any(|w| w.contains("second-node"))
        );
        assert_eq!(
            (
                row["status"].as_str(),
                row["messageCode"].as_str(),
                row["diagnostics"]["cause"].as_str()
            ),
            (
                Some("action_required"),
                Some("prefix_not_writable"),
                Some("not_writable")
            )
        );
        assert_eq!(row["parts"][1]["version"], "0.84.2");

        // R1-L6: a real failure on one copy outranks prefix_not_writable on its sibling.
        write_package(&second, "zcode-acp-server", "0.84.2");
        rig.system
            .clone()
            .answering(|_| Err(UpdateFailure::failed()));
        std::fs::set_permissions(&second, std::fs::Permissions::from_mode(0o555)).expect("chmod");
        let row = rig.adapters(Platform::Mac).await;
        std::fs::set_permissions(&second, std::fs::Permissions::from_mode(0o755)).expect("restore");
        assert_eq!(
            (row["status"].as_str(), row["messageCode"].as_str()),
            (Some("failed"), Some("update_failed"))
        );
    }

    /// Robustness: a plain node_modules folder on POSIX has no installer to bind and runs nothing.
    #[tokio::test]
    async fn plain_node_modules_folder_on_posix_is_unsupported_and_runs_nothing() {
        let rig = Rig::start();
        let mods = rig.path().join("vendor/node_modules");
        write_package(&mods, "zcode-acp-server", "0.84.2");
        let cli = mods.join("zcode-acp-server/dist/cli.js");
        std::fs::create_dir_all(cli.parent().expect("dist")).expect("dist");
        std::fs::write(&cli, b"cli").expect("cli");
        write_t3_consumers(rig.path(), &[cli]);
        rig.system.clone().lookup(feed(None, None, Some("0.84.3")));
        let layout = discover_owners(&rig.host(Platform::Mac), &Lookups::default());
        assert_eq!(
            layout.owners[ACP]
                .iter()
                .map(|o| o.manager)
                .collect::<Vec<_>>(),
            vec!["other"]
        );
        let row = rig.adapters(Platform::Mac).await;
        assert_eq!(
            (row["status"].as_str(), row["messageCode"].as_str()),
            (Some("failed"), Some("unsupported"))
        );
        assert!(rig.system.runs().is_empty());
    }

    /// Robustness: Mac installs into the prefix T3 runs; changing another copy reports wrong_prefix
    /// with `diagnostics.owner.elsewhere`.
    #[cfg(unix)]
    #[tokio::test]
    async fn mac_prefix_t3_runs_is_updated_and_wrong_prefix_reports_elsewhere() {
        let mut rig = Rig::start();
        let brew = rig.host(Platform::Mac).system("/opt/homebrew");
        let brew_mods = brew.join("lib/node_modules");
        write_package(&brew_mods, "@brokkai/muse-acp", "0.21.0");
        write_package(&brew_mods, "zcode-acp-server", "0.84.2");
        let zcode_cli = brew_mods.join("zcode-acp-server/dist/cli.js");
        std::fs::create_dir_all(zcode_cli.parent().expect("dist")).expect("dist");
        std::fs::write(&zcode_cli, b"cli").expect("cli");
        let muse_cli = brew_mods.join("@brokkai/muse-acp/bin/muse-acp.cjs");
        std::fs::create_dir_all(muse_cli.parent().expect("bin")).expect("bin");
        std::fs::write(&muse_cli, b"cli").expect("muse");
        let muse_bin = brew.join("bin/muse-acp");
        symlink_path(&muse_cli, &muse_bin);
        let node_home = rig.path().join(".local/opt/node-v26.13.1-darwin-arm64");
        let (node_real, cli_real) = write_npm_pair(&node_home, Platform::Mac);
        let node_link = brew.join("bin/node");
        symlink_path(&node_real, &node_link);
        rig.search_path.push(brew.join("bin"));
        // T3 config invokes Homebrew's node symlink with Homebrew's zcode-acp-server script.
        let settings = rig.path().join(".t3/userdata/settings.json");
        std::fs::create_dir_all(settings.parent().expect("t3")).expect("t3");
        std::fs::write(
            &settings,
            json!({
                "providerInstances": {
                    "zcode": {
                        "enabled": true,
                        "config": {
                            "commandPath": node_link.to_string_lossy(),
                            "commandArgs": [zcode_cli.to_string_lossy(), "server"],
                        }
                    }
                }
            })
            .to_string(),
        )
        .expect("settings");
        rig.system
            .clone()
            .lookup(feed(None, Some("0.21.0"), Some("0.84.3")));
        // 1. Honest npm installs into --prefix (Homebrew).
        rig.system.clone().answering(|spec| {
            let argv: Vec<String> = spec
                .argv
                .iter()
                .map(|word| word.to_string_lossy().into_owned())
                .collect();
            let at = argv.iter().position(|w| w == "--prefix").expect("prefix");
            let prefix = PathBuf::from(&argv[at + 1]);
            let (pkg, ver) = argv.last().expect("pkg").rsplit_once('@').expect("ver");
            write_package(&prefix.join("lib/node_modules"), pkg, ver);
            Ok(CommandOutput::default())
        });
        let row = rig.adapters(Platform::Mac).await;
        assert_eq!(
            (row["status"].as_str(), row["updateAttempted"].as_bool()),
            (Some("updated"), Some(true))
        );
        assert_eq!(
            parts_of(&row),
            vec![
                (
                    "muse-acp".into(),
                    Some("0.21.0".into()),
                    Some("0.21.0".into())
                ),
                (
                    "zcode-acp-server".into(),
                    Some("0.84.2".into()),
                    Some("0.84.3".into())
                ),
            ]
        );
        let run = &rig.system.run_strings()[0];
        assert_eq!(
            &run[..2],
            &[
                node_real.to_string_lossy().into_owned(),
                cli_real.to_string_lossy().into_owned()
            ]
        );
        let at = run.iter().position(|w| w == "--prefix").expect("prefix");
        assert_eq!(PathBuf::from(&run[at + 1]), real(&brew));
        let packages = row["diagnostics"]["owner"]["packages"]
            .as_array()
            .expect("packages");
        let sources: HashSet<(&str, &str)> = packages
            .iter()
            .map(|p| {
                (
                    p["package"].as_str().expect("pkg"),
                    p["source"].as_str().expect("src"),
                )
            })
            .collect();
        assert_eq!(
            sources,
            HashSet::from([("zcode-acp-server", "t3-config"), ("muse-acp", "path")])
        );
        assert!(
            !row.to_string()
                .contains(&rig.path().to_string_lossy().into_owned())
        );

        // 2. Installer writes into standalone node's prefix instead -> wrong_prefix + elsewhere.
        write_package(&brew_mods, "zcode-acp-server", "0.84.2");
        let node_mods = node_home.join("lib/node_modules");
        rig.system.clone().answering(move |_| {
            write_package(&node_mods, "zcode-acp-server", "0.84.3");
            Ok(CommandOutput::default())
        });
        let row = rig.adapters(Platform::Mac).await;
        assert_eq!(
            (
                row["status"].as_str(),
                row["messageCode"].as_str(),
                row["diagnostics"]["cause"].as_str()
            ),
            (Some("failed"), Some("wrong_prefix"), Some("wrong_prefix"))
        );
        assert_eq!(
            parts_of(&row)[1],
            (
                "zcode-acp-server".into(),
                Some("0.84.2".into()),
                Some("0.84.2".into())
            )
        );
        assert!(
            row["diagnostics"]["owner"]["elsewhere"]
                .as_array()
                .expect("elsewhere")
                .contains(&json!({
                    "package": "zcode-acp-server",
                    "prefix": "~/.local/opt/node-v26.13.1-darwin-arm64",
                    "version": "0.84.3",
                }))
        );
    }

    /// Fix round 2 C5 + R2-4: mixed explicit + bare commands resolve both copies, unresolved bare
    /// skips with `mixed_ownership`, and bare missing from PATH falls back to default prefix.
    #[cfg(unix)]
    #[tokio::test]
    async fn mixed_explicit_and_bare_commands_resolve_or_skip_honestly() {
        // 1. C5: explicit + bare on PATH -> both updated.
        {
            let mut rig = Rig::start();
            let first = write_npm_prefix_adapter(rig.path(), "first", "0.84.2");
            let second = write_npm_prefix_adapter(rig.path(), "second", "0.84.2");
            let binary = rig.path().join("second/bin/zcode-acp-server");
            symlink_path(&second.join("zcode-acp-server/dist/cli.js"), &binary);
            let settings = rig.path().join(".t3/userdata/settings.json");
            std::fs::create_dir_all(settings.parent().expect("t3")).expect("t3");
            std::fs::write(
                &settings,
                json!({
                    "providerInstances": {
                        "zcode-0": {"enabled": true, "config": {"commandPath": "node", "commandArgs": [first.join("zcode-acp-server/dist/cli.js").to_string_lossy(), "server"]}},
                        "bare": {"enabled": true, "config": {"commandPath": "zcode-acp-server"}},
                    }
                })
                .to_string(),
            )
            .expect("settings");
            let (node, _) = write_npm_pair(&rig.path().join("node"), Platform::Mac);
            rig.search_path
                .push(binary.parent().expect("bin").to_path_buf());
            rig.search_path
                .push(node.parent().expect("bin").to_path_buf());
            rig.system.clone().lookup(feed(None, None, Some("0.84.3")));
            rig.system.clone().answering(|spec| {
                let argv: Vec<String> = spec
                    .argv
                    .iter()
                    .map(|w| w.to_string_lossy().into_owned())
                    .collect();
                let at = argv.iter().position(|w| w == "--prefix").expect("prefix");
                let prefix = PathBuf::from(&argv[at + 1]);
                let (pkg, ver) = argv.last().expect("pkg").rsplit_once('@').expect("ver");
                write_package(&prefix.join("lib/node_modules"), pkg, ver);
                Ok(CommandOutput::default())
            });
            let row = rig.adapters(Platform::Mac).await;
            assert_eq!(row["status"], "updated");
            assert_eq!(rig.system.runs().len(), 2);
            assert_eq!(
                (
                    package_version(&first, ACP).as_deref(),
                    package_version(&second, ACP).as_deref()
                ),
                (Some("0.84.3"), Some("0.84.3"))
            );
        }
        // 2. C5: unresolved bare command skips with mixed_ownership without running npm.
        {
            let rig = Rig::start();
            let first = write_npm_prefix_adapter(rig.path(), "first", "0.84.2");
            let settings = rig.path().join(".t3/userdata/settings.json");
            std::fs::create_dir_all(settings.parent().expect("t3")).expect("t3");
            std::fs::write(
                &settings,
                json!({
                    "providerInstances": {
                        "zcode-0": {"enabled": true, "config": {"commandPath": "node", "commandArgs": [first.join("zcode-acp-server/dist/cli.js").to_string_lossy(), "server"]}},
                        "bare": {"enabled": true, "config": {"commandPath": "zcode-acp-server"}},
                    }
                })
                .to_string(),
            )
            .expect("settings");
            rig.system.clone().lookup(feed(None, None, Some("0.84.3")));
            let row = rig.adapters(Platform::Mac).await;
            assert_eq!(
                (row["status"].as_str(), row["messageCode"].as_str()),
                (Some("skipped"), Some("mixed_ownership"))
            );
            assert!(rig.system.runs().is_empty());
        }
        // 3. R2-4: bare command missing from helper PATH falls back to default prefix `~/.local/bin`.
        {
            let mut rig = Rig::start();
            let mods = write_npm_prefix_adapter(rig.path(), ".local", "0.84.2");
            let binary = rig.path().join(".local/bin/zcode-acp-server");
            symlink_path(&mods.join("zcode-acp-server/dist/cli.js"), &binary);
            let settings = rig.path().join(".t3/userdata/settings.json");
            std::fs::create_dir_all(settings.parent().expect("t3")).expect("t3");
            std::fs::write(
                &settings,
                json!({
                    "providerInstances": {
                        "bare": {"enabled": true, "config": {"commandPath": "zcode-acp-server"}}
                    }
                })
                .to_string(),
            )
            .expect("settings");
            let (node, _) = write_npm_pair(&rig.path().join("node"), Platform::Ubuntu);
            rig.search_path
                .push(node.parent().expect("bin").to_path_buf());
            rig.system.clone().lookup(feed(None, None, Some("0.84.3")));
            rig.system.clone().answering(|spec| {
                let argv: Vec<String> = spec
                    .argv
                    .iter()
                    .map(|w| w.to_string_lossy().into_owned())
                    .collect();
                let at = argv.iter().position(|w| w == "--prefix").expect("prefix");
                let prefix = PathBuf::from(&argv[at + 1]);
                let (pkg, ver) = argv.last().expect("pkg").rsplit_once('@').expect("ver");
                write_package(&prefix.join("lib/node_modules"), pkg, ver);
                Ok(CommandOutput::default())
            });
            let row = rig.adapters(Platform::Ubuntu).await;
            assert_eq!(row["status"], "updated");
            assert_eq!(package_version(&mods, ACP).as_deref(), Some("0.84.3"));
            let run = &rig.system.run_strings()[0];
            let at = run.iter().position(|w| w == "--prefix").expect("prefix");
            assert_eq!(PathBuf::from(&run[at + 1]), rig.path().join(".local"));
        }
    }

    // ------------------------------------------------- Windows ACP adapters (tools-lead 2026-11-27)

    /// npm 11.20 exits 1 ("Maximum call stack size exceeded") for a verbatim `--prefix`; the
    /// install is spelled with the plain path, and npm 11 runs the adapter's own postinstall
    /// only when the install names it in `--allow-scripts`.
    #[test]
    fn a_verbatim_owner_prefix_installs_with_a_plain_prefix_and_allows_the_adapter_script() {
        let argv: Vec<String> = npm_argv(
            Path::new(r"\\?\C:\Program Files\nodejs\node.exe"),
            Path::new(r"\\?\C:\Program Files\nodejs\node_modules\npm\bin\npm-cli.js"),
            Path::new(r"\\?\C:\Users\someone\AppData\Roaming\npm"),
            ACP,
            Some(Path::new(r"\\?\C:\Program Files\Git\bin\bash.exe")),
            "0.96.3",
        )
        .iter()
        .map(|word| word.to_string_lossy().into_owned())
        .collect();
        assert_eq!(
            argv,
            [
                r"C:\Program Files\nodejs\node.exe",
                r"C:\Program Files\nodejs\node_modules\npm\bin\npm-cli.js",
                "install",
                "--global",
                "--prefix",
                r"C:\Users\someone\AppData\Roaming\npm",
                "--script-shell",
                r"C:\Program Files\Git\bin\bash.exe",
                "--allow-scripts=zcode-acp-server",
                "zcode-acp-server@0.96.3",
            ]
        );
        let muse: Vec<String> = npm_argv(
            Path::new("/usr/bin/node"),
            Path::new("/usr/lib/node_modules/npm/bin/npm-cli.js"),
            Path::new("/usr/local"),
            MUSE,
            None,
            "0.26.0",
        )
        .iter()
        .map(|word| word.to_string_lossy().into_owned())
        .collect();
        assert!(muse.contains(&"--allow-scripts=@brokkai/muse-acp".to_owned()));
        assert!(muse.contains(&"/usr/local".to_owned()));
    }

    /// `dunce::simplified`'s rule: a verbatim drive or UNC path is written plainly; anything
    /// else (a plain path, a device or volume path, one too long for the plain form) stays.
    #[test]
    fn verbatim_paths_are_simplified_only_when_a_plain_spelling_exists() {
        for (given, plain_form) in [
            (r"\\?\C:\Users\someone\npm", r"C:\Users\someone\npm"),
            (r"\\?\d:\agent-scratch", r"d:\agent-scratch"),
            (r"\\?\UNC\server\share\npm", r"\\server\share\npm"),
            (r"C:\Users\someone\npm", r"C:\Users\someone\npm"),
            ("/home/someone/.npm-global", "/home/someone/.npm-global"),
            (r"\\?\Volume{0b1a}\npm", r"\\?\Volume{0b1a}\npm"),
            (r"\\.\pipe\npm", r"\\.\pipe\npm"),
            (r"\\?\C:", r"\\?\C:"),
            (r"\\?\UNC\", r"\\?\UNC\"),
        ] {
            assert_eq!(
                plain(Path::new(given)),
                PathBuf::from(plain_form),
                "{given}"
            );
        }
        let long = format!(r"\\?\C:\{}", "a".repeat(300));
        assert_eq!(plain(Path::new(&long)), PathBuf::from(&long));
    }

    /// A failed install keeps npm's deciding line (scrubbed, bounded) in the row's diagnostics,
    /// so the next failure says why instead of only "installer_failed".
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_adapter_install_keeps_the_npm_error_line() {
        let mut rig = Rig::start();
        let prefix = write_npm_prefix_adapter(rig.path(), "opt/homebrew", "0.95.1");
        write_t3_consumers(rig.path(), &[prefix.join("zcode-acp-server/dist/cli.js")]);
        let (node, _) = write_npm_pair(&rig.path().join("node"), Platform::Mac);
        rig.search_path
            .push(node.parent().expect("bin").to_path_buf());
        rig.system.clone().lookup(feed(None, None, Some("0.96.3")));
        rig.system.clone().answering(|_| {
            Err(UpdateFailure {
                output: [
                    "npm warn config something harmless",
                    "npm error Maximum call stack size exceeded",
                    "npm error A complete log of this run can be found in: /x/_logs/debug-0.log",
                    "npm error token=sk-ant-oat01-abcdefghijklmnopqrstuvwxyz0123456789",
                    "",
                ]
                .join("\n"),
                exit_code: Some(1),
                ..UpdateFailure::failed()
            })
        });
        let row = rig.adapters(Platform::Mac).await;
        assert_eq!(row["status"], "failed", "{row}");
        let output = row["diagnostics"]["output"]
            .as_str()
            .expect("npm output kept");
        assert!(
            output.contains("Maximum call stack size exceeded"),
            "{output}"
        );
        assert!(
            !output.contains("abcdefghijklmnopqrstuvwxyz0123456789"),
            "{output}"
        );
        assert!(!output.contains("harmless"), "{output}");
        assert!(output.chars().count() <= 300, "{output}");
    }

    // W3-TESTS-END
}
