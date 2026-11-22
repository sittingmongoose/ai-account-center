//! The guarded preferences writer: the TS-validated documents, and the public route's own
//! validation of a full document or a patch (zone, sources, path overlap) before them.
use aac_core::Cause;
use aac_files::{
    OwnedStore,
    read::{Read, read},
    registry::{self, Owners},
};
use aac_providers::LEGACY_STORAGE_HOSTS;
use aac_views::decline::Decline;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    time::{Duration, Instant},
};

const PRIMARY: &str = "dashboard-preferences";
const COMPANION: &str = "dashboard-preferences-extra-sources";
pub(crate) const TRANSACTION: &str = "dashboard-preferences-transaction";
pub(crate) const LEASE_WAIT: Duration = Duration::from_millis(250);
const DOC_CAP: usize = 64 * 1024;
pub(crate) const INPUT_CAP: usize = 512 * 1024;

/// Retry only an active lease. A claim that took some leases before failing releases them
/// before another attempt. The initial attempt also serves callers with a zero wait.
pub(crate) fn retry_busy_lease<T>(
    deadline: Instant,
    mut claim: impl FnMut() -> Result<T, Decline>,
) -> Result<T, Decline> {
    loop {
        match claim() {
            Err(Decline::Busy) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(Decline::Busy);
                }
                std::thread::sleep(remaining.min(Duration::from_millis(5)));
                if Instant::now() >= deadline {
                    return Err(Decline::Busy);
                }
            }
            result => return result,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Documents {
    primary: String,
    companion: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    previous: Option<[Option<String>; 2]>,
    primary: String,
    companion: Option<String>,
}

fn stored(root: &Path, id: &str) -> Result<Option<Vec<u8>>, Decline> {
    let (file, policy) = crate::registered::file(root, id).ok_or(Decline::Internal)?;
    match read(&file, &policy) {
        Read::Absent => Ok(None),
        Read::Bytes(bytes, _) => Ok(Some(bytes)),
        _ => Err(Decline::Unsupported),
    }
}

/// A committed intent the reader gives precedence over the materialized files, exactly as
/// `readDashboardPreferences` does after an interrupted save.
pub(crate) fn pending(root: &Path) -> Option<Value> {
    let bytes = stored(root, TRANSACTION).ok()??;
    let documents: Documents =
        crate::json_object::from_slice(&bytes, &["primary", "companion"]).ok()?;
    response(&documents).ok()
}

fn revision(root: &Path) -> Result<[Option<String>; 2], Decline> {
    let hash = |bytes: Vec<u8>| {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    Ok([
        stored(root, PRIMARY)?.map(hash),
        stored(root, COMPANION)?.map(hash),
    ])
}

fn response(documents: &Documents) -> Result<Value, Decline> {
    if documents.primary.len() > DOC_CAP
        || documents
            .companion
            .as_ref()
            .is_some_and(|text| text.len() > DOC_CAP)
    {
        return Err(Decline::Unsupported);
    }
    let primary = crate::json_object::from_slice::<Value>(
        documents.primary.as_bytes(),
        &["timeZone", "snapshotCleanup", "usageLogSources"],
    )
    .map_err(|_| Decline::Unsupported)?;
    let mut companion = match &documents.companion {
        Some(text) => {
            let mut value =
                crate::json_object::from_slice::<Value>(text.as_bytes(), &["usageLogSources"])
                    .map_err(|_| Decline::Unsupported)?;
            value["usageLogSources"]
                .take()
                .as_array()
                .cloned()
                .ok_or(Decline::Unsupported)?
        }
        None => Vec::new(),
    };
    crate::account_reads::prepared_preferences(primary, &mut companion).ok_or(Decline::Unsupported)
}

fn materialize(
    primary: &OwnedStore,
    companion: &OwnedStore,
    documents: &Documents,
) -> Result<(), Decline> {
    match &documents.companion {
        Some(text) => companion.replace_settings(text.as_bytes()),
        None => companion.remove_settings(),
    }
    .map_err(|_| Decline::Internal)?;
    primary
        .replace_settings(documents.primary.as_bytes())
        .map_err(|_| Decline::Internal)
}

struct WriteLease {
    primary: OwnedStore,
    companion: OwnedStore,
    transaction: OwnedStore,
}

impl WriteLease {
    fn acquire(root: &Path, owners: &Owners, deadline: Instant) -> Result<Self, Decline> {
        let rows = registry::rows().map_err(|_| Decline::Internal)?;
        let claim = |id| {
            owners.claim(rows, id, root, None).map_err(|error| {
                if error.cause == Cause::Busy {
                    Decline::Busy
                } else {
                    Decline::Unsupported
                }
            })
        };
        let claim_all = || {
            Ok(Self {
                primary: claim(PRIMARY)?,
                companion: claim(COMPANION)?,
                transaction: claim(TRANSACTION)?,
            })
        };
        let lease = retry_busy_lease(deadline, claim_all)?;
        // Resume an earlier committed intent before reading or comparing the current files.
        if let Some(pending) = stored(root, TRANSACTION)? {
            let old: Documents =
                crate::json_object::from_slice(&pending, &["primary", "companion"])
                    .map_err(|_| Decline::Unsupported)?;
            response(&old)?;
            materialize(&lease.primary, &lease.companion, &old)?;
            lease
                .transaction
                .remove_settings()
                .map_err(|_| Decline::Internal)?;
        }
        Ok(lease)
    }

    fn publish(&self, documents: &Documents) -> Result<(), Decline> {
        self.primary
            .can_replace_settings()
            .map_err(|_| Decline::Unsupported)?;
        self.companion
            .can_replace_settings()
            .map_err(|_| Decline::Unsupported)?;

        // Once the intent has been published, the next writer can finish materializing it.
        let mut pending = serde_json::to_vec(documents).map_err(|_| Decline::Internal)?;
        pending.push(b'\n');
        self.transaction
            .write(&pending)
            .map_err(|_| Decline::Internal)?;
        materialize(&self.primary, &self.companion, documents)?;
        self.transaction
            .remove_settings()
            .map_err(|_| Decline::Internal)?;
        self.primary.clean_settings_temporaries();
        self.companion.clean_settings_temporaries();
        self.transaction.clean_settings_temporaries();
        Ok(())
    }
}

pub(crate) fn save(
    root: &Path,
    owners: &Owners,
    patch: bool,
    bytes: &[u8],
) -> Result<Value, Decline> {
    let request: Request =
        crate::json_object::from_slice(bytes, &["previous", "primary", "companion"])
            .map_err(|_| Decline::Unsupported)?;
    if request.previous.is_some() != patch {
        return Err(Decline::Unsupported);
    }
    let documents = Documents {
        primary: request.primary,
        companion: request.companion,
    };
    let saved = response(&documents)?;
    let lease = WriteLease::acquire(root, owners, Instant::now()).map_err(|error| match error {
        Decline::Busy => Decline::Unsupported,
        error => error,
    })?;
    if let Some(expected) = request.previous
        && revision(root)? != expected
    {
        return Err(Decline::Stale);
    }
    lease.publish(&documents)?;
    Ok(saved)
}

/// A public preferences save the route must refuse with its own fixed sentence.
pub(crate) enum PublicError {
    /// The body is not a whole document or a known patch.
    Invalid,
    /// A rule sentence the TS route answered verbatim (`ConfigError`).
    Refused(String),
    /// Another writer still holds a registry lease.
    Busy,
    /// The documents, or the files that hold them, could not be read or written safely.
    Unavailable,
}

impl From<Decline> for PublicError {
    fn from(error: Decline) -> Self {
        match error {
            Decline::Busy => Self::Busy,
            _ => Self::Unavailable,
        }
    }
}

/// The public route's write: validate one whole document or patch exactly as the TS route
/// did. A patch reads, merges and installs both documents under the same writer leases.
pub(crate) fn save_public(
    root: &Path,
    owners: &Owners,
    patch: bool,
    body: &[u8],
    deadline: Instant,
) -> Result<Value, PublicError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| PublicError::Invalid)?;
    if !value.is_object() {
        return Err(PublicError::Invalid);
    }
    let hosts = crate::account_reads::usage_hosts().ok_or(PublicError::Unavailable)?;
    // The route refuses a generic JSONL source on a non-ubuntu computer before the shape
    // checks, whichever member carried it.
    let named = value
        .get("usageLogSources")
        .and_then(Value::as_array)
        .cloned()
        .or_else(|| {
            value
                .get("addUsageLogSource")
                .map(|source| vec![source.clone()])
        })
        .unwrap_or_default();
    if named.iter().any(|source| {
        source.get("tool").and_then(Value::as_str) == Some("jsonl")
            && source.get("host").and_then(Value::as_str) != Some("ubuntu")
    }) {
        return Err(PublicError::Refused(
            "Generic JSONL usage log sources are supported only on ubuntu".into(),
        ));
    }
    let (documents, lease) = if patch {
        if !patch_valid(&value, &hosts) {
            return Err(PublicError::Invalid);
        }
        let lease = WriteLease::acquire(root, owners, deadline)?;
        // Refuse unsafe leaves as the revision check did. The reader may otherwise default an
        // unreadable preference, which would let a partial update discard the existing fields.
        stored(root, PRIMARY).map_err(|_| PublicError::Unavailable)?;
        stored(root, COMPANION).map_err(|_| PublicError::Unavailable)?;
        let mut current = crate::account_reads::preferences(root)
            .unwrap_or_else(|| Value::Object(crate::account_reads::defaults()));
        apply_patch(&mut current, &value);
        (prepare_documents(&current, &hosts)?, lease)
    } else {
        let documents = prepare_documents(&value, &hosts)?;
        let lease = WriteLease::acquire(root, owners, deadline)?;
        (documents, lease)
    };
    let saved = response(&documents).map_err(|_| PublicError::Unavailable)?;
    lease
        .publish(&documents)
        .map_err(|_| PublicError::Unavailable)?;
    Ok(saved)
}

/// `isDashboardPreferencesPatch`: one or more known members with valid values.
fn patch_valid(patch: &Value, hosts: &[(String, String)]) -> bool {
    let Some(patch) = patch.as_object() else {
        return false;
    };
    if patch.is_empty() {
        return false;
    }
    patch.iter().all(|(key, value)| match key.as_str() {
        "timeZone" => crate::account_reads::time_zone(Some(value)) == Some(true),
        "snapshotCleanup" => {
            let Some(cleanup) = value.as_object() else {
                return false;
            };
            cleanup.len() == 1 && cleanup.contains_key("auto") && cleanup["auto"].is_boolean()
        }
        "addUsageLogSource" => crate::account_reads::usage_log_source(value, false, hosts),
        "removeUsageLogSource" => value
            .as_str()
            .is_some_and(crate::account_reads::public_source_id),
        _ => false,
    })
}

/// `patchedPreferences`: assignment keeps an existing member's position, as TS spread does.
fn apply_patch(current: &mut Value, patch: &Value) {
    let Some(current) = current.as_object_mut() else {
        return;
    };
    let Some(patch) = patch.as_object() else {
        return;
    };
    if let Some(zone) = patch.get("timeZone") {
        current.insert("timeZone".into(), zone.clone());
    }
    if let Some(cleanup) = patch.get("snapshotCleanup") {
        current.insert("snapshotCleanup".into(), cleanup.clone());
    }
    if let Some(remove) = patch.get("removeUsageLogSource").and_then(Value::as_str)
        && let Some(sources) = current
            .get_mut("usageLogSources")
            .and_then(Value::as_array_mut)
    {
        sources.retain(|source| source.get("id").and_then(Value::as_str) != Some(remove));
    }
    if let Some(add) = patch.get("addUsageLogSource")
        && let Some(sources) = current
            .get_mut("usageLogSources")
            .and_then(Value::as_array_mut)
    {
        sources.push(add.clone());
    }
}

/// The two documents the TS writer materialized: legacy-storage hosts in the primary file,
/// every other host in the companion; the same validation and path-overlap rules run first.
fn prepare_documents(value: &Value, hosts: &[(String, String)]) -> Result<Documents, PublicError> {
    let sources = value
        .get("usageLogSources")
        .and_then(Value::as_array)
        .ok_or(PublicError::Invalid)?;
    let valid = crate::account_reads::preferences_valid(
        value.as_object().ok_or(PublicError::Invalid)?,
        hosts,
    );
    if valid != Some(true) {
        return Err(PublicError::Invalid);
    }
    check_overlap(sources, hosts)?;
    let legacy = |source: &Value| {
        source
            .get("host")
            .and_then(Value::as_str)
            .is_some_and(|host| LEGACY_STORAGE_HOSTS.contains(&host))
    };
    let mut primary = value.clone();
    if let Some(row) = primary.as_object_mut() {
        row.insert(
            "usageLogSources".into(),
            Value::Array(sources.iter().filter(|s| legacy(s)).cloned().collect()),
        );
    }
    let extra: Vec<Value> = sources.iter().filter(|s| !legacy(s)).cloned().collect();
    Ok(Documents {
        primary: format!("{}\n", primary),
        companion: (!extra.is_empty()).then(|| format!("{}\n", json!({"usageLogSources": extra}))),
    })
}

/// One source's comparable root after the host's own path syntax.
struct Root {
    id: String,
    tool: String,
    host: String,
    root: String,
    sep: char,
}

/// `path.posix.normalize`: collapse separators and `.`; `..` never survives validation.
fn normalize_posix(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    let body = parts.join("/");
    if absolute { format!("/{body}") } else { body }
}

/// `path.win32.normalize`, keeping the drive when the path carries one.
fn normalize_windows(path: &str) -> String {
    let text = path.replace('/', "\\");
    let (prefix, rest) = match text.split_once(':') {
        Some((drive, rest)) if drive.len() == 1 && drive.as_bytes()[0].is_ascii_alphabetic() => {
            (format!("{drive}:"), rest)
        }
        _ => (String::new(), text.as_str()),
    };
    let absolute = rest.starts_with('\\');
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split('\\') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    let body = parts.join("\\");
    if absolute {
        format!("{prefix}\\{body}")
    } else {
        format!("{prefix}{body}")
    }
}

/// Node's `path.normalize` plus the trailing-separator strip the TS route applied.
fn comparable_root(path: &str, windows: bool) -> String {
    let mut root = if windows {
        normalize_windows(path)
    } else {
        normalize_posix(path)
    };
    if windows {
        while root.ends_with('\\') {
            root.pop();
        }
    } else {
        while root.ends_with('/') {
            root.pop();
        }
    }
    root
}

/// The built-in read roots a saved source must not cover, from the TS overlap rule.
fn builtin_roots(tool: &str, relative: Option<&str>) -> Vec<String> {
    let mut roots: Vec<String> = match tool {
        "claude-code" => vec![".claude/projects".into()],
        "codex" => vec![".codex/sessions".into()],
        "omp" => vec![".omp/agent/sessions".into()],
        "muse" => vec![".local/share/muse/sessions".into()],
        "zcode" => vec![".zcode/cli/db".into()],
        _ => Vec::new(),
    };
    if tool == "omp"
        && let Some(relative) = relative
        && let Some(rest) = relative.strip_prefix(".omp/profiles/")
        && let Some(name) = rest.split('/').next()
        && !name.is_empty()
        && (rest.len() == name.len() || rest.as_bytes()[name.len()] == b'/')
    {
        roots.push(format!(".omp/profiles/{name}/agent/sessions"));
    }
    roots
}

/// The home prefix Node's regex matched, including the separator it consumed.
fn home_prefix(portable: &str) -> Option<usize> {
    let with_segment = |prefix: &str| {
        let rest = portable.strip_prefix(prefix)?;
        if rest.is_empty() {
            return None;
        }
        match rest.find('/') {
            Some(end) => Some(prefix.len() + end + 1),
            None => Some(portable.len()),
        }
    };
    if let Some(end) = with_segment("/Users/") {
        return Some(end);
    }
    if let Some(end) = with_segment("/home/") {
        return Some(end);
    }
    if let Some(rest) = portable.strip_prefix("/root") {
        if rest.is_empty() {
            return Some("/root".len());
        }
        if rest.starts_with('/') {
            return Some("/root/".len());
        }
    }
    // Windows homes are already lowercased; the drive keeps its original letter.
    let bytes = portable.as_bytes();
    if bytes.len() > 8 && bytes[1] == b':' {
        let lower = portable.to_ascii_lowercase();
        if let Some(rest) = lower.get(3..).and_then(|r| r.strip_prefix("users/")) {
            if rest.is_empty() {
                return None;
            }
            return Some(match rest.find('/') {
                Some(end) => 3 + "users/".len() + end + 1,
                None => portable.len(),
            });
        }
    }
    None
}

fn check_overlap(sources: &[Value], hosts: &[(String, String)]) -> Result<(), PublicError> {
    let mut roots: Vec<Root> = Vec::new();
    for source in sources {
        let id = source
            .get("id")
            .and_then(Value::as_str)
            .ok_or(PublicError::Invalid)?
            .to_owned();
        let tool = source
            .get("tool")
            .and_then(Value::as_str)
            .ok_or(PublicError::Invalid)?
            .to_owned();
        let host = source
            .get("host")
            .and_then(Value::as_str)
            .ok_or(PublicError::Invalid)?
            .to_owned();
        let path = source
            .get("path")
            .and_then(Value::as_str)
            .ok_or(PublicError::Invalid)?;
        let os = hosts
            .iter()
            .find(|(name, _)| name == &host)
            .map(|(_, os)| os.as_str())
            .ok_or(PublicError::Invalid)?;
        let windows = os == "windows";
        let mut root = path.to_owned();
        // The TS route resolved an existing Ubuntu root before the lexical overlap test.
        #[cfg(not(windows))]
        if host == "ubuntu"
            && let Ok(real) = std::fs::canonicalize(&root)
            && let Some(real) = real.to_str()
        {
            root = real.to_owned();
        }
        let sep = if windows { '\\' } else { '/' };
        let mut root = comparable_root(&root, windows);
        if tool == "codex" {
            if !root.is_empty() {
                root.push(sep);
            }
            root.push_str("sessions");
        }
        if windows {
            root = root.to_ascii_lowercase();
        }
        roots.push(Root {
            id,
            tool,
            host,
            root,
            sep,
        });
    }
    for (index, current) in roots.iter().enumerate() {
        let portable = current.root.replace('\\', "/");
        let home = home_prefix(&portable);
        let relative = home.map(|end| portable[end..].to_owned());
        let read_roots = if current.tool == "jsonl" {
            // Every usage-log tool with its own built-in roots; the generic JSONL format has
            // none (its roots are the extra source's own).
            aac_providers::builtin_root_tools()
                .flat_map(|tool| builtin_roots(tool, relative.as_deref()))
                .collect::<Vec<_>>()
        } else {
            builtin_roots(&current.tool, relative.as_deref())
        };
        let overlap = relative.as_ref().is_some_and(|relative| {
            read_roots.iter().any(|root| {
                relative.is_empty()
                    || relative == root
                    || relative.starts_with(&format!("{root}/"))
                    || root.starts_with(&format!("{relative}/"))
            })
        });
        if overlap {
            return Err(PublicError::Refused(format!(
                "Usage log source {} overlaps a built-in root",
                current.id
            )));
        }
        for prior in &roots[..index] {
            if prior.host != current.host {
                continue;
            }
            if prior.tool != current.tool && prior.tool != "jsonl" && current.tool != "jsonl" {
                continue;
            }
            let overlaps = prior.root == current.root
                || current
                    .root
                    .starts_with(&format!("{}{}", prior.root, current.sep))
                || prior
                    .root
                    .starts_with(&format!("{}{}", current.root, current.sep));
            if overlaps {
                return Err(PublicError::Refused(format!(
                    "Usage log sources {} and {} overlap",
                    prior.id, current.id
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// The built-in roots a generic JSONL source may not cover are the usage-log tools of the
    /// tool table, minus the generic format itself (the old hand list).
    #[test]
    fn builtin_root_tools_keep_the_old_hand_list() {
        let mut tools: Vec<&str> = aac_providers::builtin_root_tools().collect();
        tools.sort_unstable();
        assert_eq!(tools, ["claude-code", "codex", "muse", "omp", "zcode"]);
    }
}
