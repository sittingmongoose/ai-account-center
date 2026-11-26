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

/// Python `UBUNTU_RUN`: the one t3-acp-update run of a job, shared by the ZCode and adapter
/// rows. The helper had one process per job; here an entry lives as long as the job's
/// [`Shared`] does, so a later job never inherits it.
static UNIT_RUNS: Mutex<Vec<(Weak<Shared>, UnitCode)>> = Mutex::new(Vec::new());

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

/// Python `os.path.expandvars` for `$NAME` and `${NAME}`; an unset name is empty.
fn expand_vars(text: &str, vars: &dyn Vars) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let (name, tail) = match after.strip_prefix('{') {
            Some(inner) => match inner.find('}') {
                Some(end) => (&inner[..end], &inner[end + 1..]),
                None => {
                    out.push('$');
                    break;
                }
            },
            None => {
                let end = after
                    .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
                    .unwrap_or(after.len());
                (&after[..end], &after[end..])
            }
        };
        if name.is_empty() {
            out.push('$');
            rest = after;
            continue;
        }
        out.push_str(&vars.get(name).unwrap_or_default());
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
            at += 4 + text[at + 4..].find("-->")?;
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
            if stack.is_empty() {
                break;
            }
            continue;
        }
        if depth == 2 && parent == Some("dict") && matches!(name, "key" | "string") {
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
    install.version = installed_version(cx.platform, &path);
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
    if let Some(prefix) = &host.npm_prefix {
        return Some(expand_user(prefix, &host.home));
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
    let layout = discover_owners(&cx.host, vars);
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
    let wrong_size =
        size.is_some_and(|size| std::fs::metadata(&package).is_ok_and(|info| info.len() != size));
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
    let text = output.replace('\\', "/");
    let errors = text
        .lines()
        .filter(|line| line.starts_with("npm error"))
        .collect::<Vec<_>>()
        .join("\n");
    if !LOCKED.is_match(&errors) {
        return false;
    }
    let needle = format!("node_modules/{}", PACKAGES[adapter].1);
    errors
        .to_lowercase()
        .match_indices(&needle)
        .any(|(at, found)| {
            !errors[at + found.len()..]
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
    let mut argv: Vec<OsString> = [node, cli].map(|path| path.as_os_str().to_owned()).to_vec();
    argv.extend(["install", "--global", "--prefix"].map(OsString::from));
    argv.push(prefix.as_os_str().to_owned());
    if let Some(shell) = shell {
        argv.extend(["--script-shell".into(), shell.as_os_str().to_owned()]);
    }
    argv.push(format!("{}@{version}", PACKAGES[adapter].1).into());
    argv
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
                .nth(1)
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
                None if cfg!(windows) => host.home.join("AppData/Local"),
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
            argv.push(owner.prefix.as_os_str().to_owned());
            argv.push(package.into());
            // pnpm refuses a global install without its bin folder (PNPM_HOME) on PATH.
            let bin_dir = owner
                .bin_dir
                .as_deref()
                .unwrap_or(&owner.prefix)
                .to_string_lossy()
                .into_owned();
            let path = format!(
                "{bin_dir}{}{}",
                if cfg!(windows) { ';' } else { ':' },
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
                    owner.prefix.to_string_lossy().into_owned(),
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

/// This job's entry in [`UNIT_RUNS`], created on first use. Entries of finished jobs are dropped.
fn unit_slot(shared: &Arc<Shared>) -> usize {
    let mut runs = UNIT_RUNS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    runs.retain(|(job, _)| job.strong_count() > 0);
    if let Some(index) = runs
        .iter()
        .position(|(job, _)| std::ptr::eq(job.as_ptr(), Arc::as_ptr(shared)))
    {
        return index;
    }
    runs.push((Arc::downgrade(shared), None));
    runs.len() - 1
}

/// Python `UBUNTU_RUN["code"]`: `None` until this job's one unit run finished; then its
/// failure code, or `None` for a run that succeeded.
fn unit_ran(shared: &Arc<Shared>) -> Option<Option<&'static str>> {
    let runs = UNIT_RUNS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    runs.iter()
        .find(|(job, _)| std::ptr::eq(job.as_ptr(), Arc::as_ptr(shared)))
        .and_then(|(_, code)| *code)
}

/// Start t3-acp-update and wait for its run, once per job; its failure code or `None`.
/// At the timeout only this wait stops: the unit finishes on its own, under its own lock
/// and TimeoutStartSec, so the shared npm resources stay reserved until a terminal
/// state is verified.
async fn run_unit(cx: &Context) -> Result<Option<&'static str>, UpdateFailure> {
    let slot = unit_slot(&cx.shared);
    let previous = {
        let runs = UNIT_RUNS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        runs[slot].1
    };
    if let Some(code) = previous {
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
    UNIT_RUNS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)[slot]
        .1 = Some(code);
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

/// Python `os.access(path, os.W_OK)` by the permission bits; a missing folder is not writable.
fn writable_folder(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|info| !info.permissions().readonly())
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
        let _ = services.restart(cx, install, &gone).await;
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
            Ok(count) => AppRow::of(
                install,
                Status::Updated,
                install.version.as_deref(),
                Some(replace.version),
                "desktop_reopened",
            )
            .attempted(count),
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
    if !contexts.is_empty() {
        row.restart_targets = Some(vec![json!({"kind": "desktop"})]);
    }
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
        let _ = std::fs::rename(backup, recovery);
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
            let mixed = error.code != "version_went_down" && after.is_some() && after != before;
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
            Err(error) => failures.push((step, error.code)),
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
    // again here: `attempt` runs detection and update back to back.
    let layout = discover_owners(&cx.host, vars);
    adapters_update_layout(cx, vars, &layout).await
}

async fn adapters_update_layout(cx: &Context, vars: &dyn Vars, layout: &Layout) -> AppRow {
    let mut state = Attempt::default();
    let mut behind: Vec<Adapter> = Vec::new();
    let error = match adapters_steps(cx, vars, &layout, &mut state, &mut behind).await {
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
            &layout,
            Status::ActionRequired,
            None,
            error.code,
            state.attempted,
            &held,
        );
    }
    let after = state.attempted.then(|| parts_now(&layout));
    adapters_row(
        cx,
        vars,
        &layout,
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

    /// A scripted host: answers commands, the process table, page lookups and downloads,
    /// and records what each lookup was asked for (URL, size cap, URL prefix).
    struct Fixture {
        runs: Mutex<Vec<(Vec<OsString>, Duration)>>,
        answer: Mutex<Answer>,
        table: Table,
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
                .map(|(argv, _)| argv.clone())
                .collect()
        }

        fn timeouts(&self) -> Vec<Duration> {
            self.runs
                .lock()
                .expect("runs")
                .iter()
                .map(|(_, timeout)| *timeout)
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
                self.runs
                    .lock()
                    .expect("runs")
                    .push((spec.argv.clone(), spec.timeout));
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
                self.scans
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(ProcessTable::new(
                    self.table.lock().expect("table").clone(),
                ))
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
        events: Mutex<Vec<&'static str>>,
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
            Some(1)
        }

        fn terminate<'a>(
            &'a self,
            _cx: &'a Context,
            _install: &'a Installed,
            contexts: &'a [Process],
        ) -> BoxFuture<'a, Result<u32, UpdateFailure>> {
            Box::pin(async move {
                self.events.lock().expect("events").push("close");
                let mains: HashSet<u32> = contexts.iter().map(|item| item.pid).collect();
                self.table
                    .lock()
                    .expect("table")
                    .retain(|item| !mains.contains(&item.pid) && !mains.contains(&item.ppid));
                Ok(u32::try_from(contexts.len()).unwrap_or(u32::MAX))
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
        root: tempfile::TempDir,
        system: Arc<Fixture>,
        services: Arc<Scripted>,
        table: Table,
        phases: Arc<Mutex<Vec<&'static str>>>,
        /// The test computer's PATH (empty until a test adds a folder).
        search_path: Vec<PathBuf>,
    }

    impl Rig {
        fn start() -> Self {
            let table: Table = Arc::new(Mutex::new(Vec::new()));
            Self {
                root: tempfile::tempdir().expect("root"),
                system: Fixture::plain(table.clone()),
                services: Arc::new(Scripted {
                    arguments: Mutex::new(Vec::new()),
                    events: Mutex::new(Vec::new()),
                    reopen: Mutex::new(None),
                    table: table.clone(),
                }),
                table,
                phases: Arc::new(Mutex::new(Vec::new())),
                search_path: Vec::new(),
            }
        }

        fn path(&self) -> &Path {
            self.root.path()
        }

        fn host(&self, platform: Platform) -> Arc<Host> {
            let mut host = Arc::into_inner(host(self.path(), platform)).expect("own host");
            host.path = self.search_path.clone();
            Arc::new(host)
        }

        /// A context with its own shared state (one job with one row so far).
        fn cx(&self, platform: Platform, seconds: u64) -> Context {
            self.cx_shared(platform, seconds, Arc::new(Shared::default()))
        }

        /// A context that shares `shared` with the job's other rows.
        fn cx_shared(&self, platform: Platform, seconds: u64, shared: Arc<Shared>) -> Context {
            let phases = self.phases.clone();
            Context::new(
                platform,
                self.host(platform),
                self.system.clone(),
                shared,
                Instant::now() + Duration::from_secs(seconds),
                self.path().join("stage"),
                self.path().join("state"),
                None,
                || false,
                move |phase| phases.lock().expect("phases").push(phase),
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
    }

    fn host(root: &Path, platform: Platform) -> Arc<Host> {
        Arc::new(Host {
            platform,
            home: root.to_path_buf(),
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
        std::fs::create_dir_all(&file.parent().expect("unit folder")).expect("user units");
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

    // W3-TESTS-END
}
