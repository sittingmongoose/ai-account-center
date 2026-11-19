//! Conservative Claude index reader. Unqualified guards and all writes delegate intact.
use crate::proof;
use aac_core::Failure;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

type Checked<T> = Result<T, ()>;
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn encoded(v: &Value) -> Checked<Vec<u8>> {
    serde_json::to_vec(v).map_err(|_| ())
}
fn object(bytes: &[u8]) -> Checked<Value> {
    let v = proof::json(bytes, 2_000_000, true).ok_or(())?;
    if !v.is_object() {
        return Err(());
    }
    Ok(v)
}
fn text(v: &Value) -> Checked<&str> {
    v.as_str().ok_or(())
}
/// Python's `clean_text`: nonempty, at most `limit` characters, no C0 control or DEL.
fn clean_text(v: &Value, limit: usize) -> Checked<&str> {
    let s = text(v)?;
    if s.is_empty() || s.chars().count() > limit || s.chars().any(|c| c < ' ' || c == '\u{7f}') {
        return Err(());
    }
    Ok(s)
}
fn keys(v: &Value, required: &[&str], optional: &[&str]) -> Checked<()> {
    let map = v.as_object().ok_or(())?;
    if required.iter().any(|k| !map.contains_key(*k))
        || map
            .keys()
            .any(|k| !required.contains(&k.as_str()) && !optional.contains(&k.as_str()))
    {
        return Err(());
    }
    Ok(())
}
fn uuid(raw: &str) -> bool {
    raw.len() == 36
        && raw.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}
fn name(raw: &str) -> bool {
    raw.strip_prefix("local_")
        .and_then(|s| s.strip_suffix(".json"))
        .is_some_and(|s| uuid(s) && !s.bytes().any(|b| b.is_ascii_uppercase()))
}

#[cfg(unix)]
fn directory(path: &Path) -> Checked<Value> {
    use std::os::unix::fs::MetadataExt;
    for part in path.ancestors() {
        let m = fs::symlink_metadata(part).map_err(|_| ())?;
        if !m.is_dir() || m.file_type().is_symlink() {
            return Err(());
        }
    }
    let m = fs::symlink_metadata(path).map_err(|_| ())?;
    if m.dev() == 0 || m.ino() == 0 {
        return Err(());
    }
    Ok(json!({"dev":m.dev().to_string(),"ino":m.ino().to_string()}))
}
#[cfg(not(unix))]
fn directory(_path: &Path) -> Checked<Value> {
    Err(())
}

#[cfg(unix)]
fn regular(path: &Path, optional: bool, owned: bool) -> Checked<Option<Vec<u8>>> {
    use std::os::unix::fs::MetadataExt;
    directory(path.parent().ok_or(())?)?;
    let (raw, opened) = match crate::files::read_regular(path, 2_000_000) {
        crate::files::Regular::Read(raw, opened) => (raw, opened),
        crate::files::Regular::Missing if optional => return Ok(None),
        _ => return Err(()),
    };
    // The helper's own link rule: one name per record (two for the owned copy it keeps).
    if !(opened.nlink() == 1 || owned && opened.nlink() == 2) {
        return Err(());
    }
    let after = fs::symlink_metadata(path).map_err(|_| ())?;
    if (after.dev(), after.ino()) != (opened.dev(), opened.ino()) {
        return Err(());
    }
    Ok(Some(raw))
}
#[cfg(not(unix))]
fn regular(_path: &Path, _optional: bool, _owned: bool) -> Checked<Option<Vec<u8>>> {
    Err(())
}

fn entries(path: &Path, cap: usize) -> Checked<Vec<fs::DirEntry>> {
    // An entry error or more than `cap` entries refuses the listing.
    let values: Vec<_> = aac_files::read::entries(path, cap)
        .map_err(|_| ())?
        .collect::<Result<_, _>>()
        .map_err(|_| ())?;
    if values
        .iter()
        .any(|e| e.file_name().to_str().is_none_or(|s| !proof::ascii(s)))
    {
        return Err(());
    }
    Ok(values)
}
struct Bound<'a> {
    profile: &'a str,
    platform: &'a str,
    policy: &'a Value,
    home: PathBuf,
    root: PathBuf,
    fixture: Option<&'a Value>,
}
impl<'a> Bound<'a> {
    fn new(args: &'a Value) -> Checked<Self> {
        keys(
            args,
            &["mode", "profileId", "platform", "policy"],
            &[
                "expectedEmail",
                "expectedTarget",
                "records",
                "transactionSource",
                "_historyFixture",
            ],
        )?;
        let profile = text(&args["profileId"])?;
        let platform = text(&args["platform"])?;
        if !matches!(profile, "platyr" | "gmail") || !matches!(platform, "mac" | "windows") {
            return Err(());
        }
        let policy = &args["policy"];
        keys(
            policy,
            &[
                "version",
                "enabled",
                "sourcePlatform",
                "identity",
                "project",
                "ssh",
            ],
            &[],
        )?;
        if policy["version"] != 1
            || policy["enabled"] != true
            || policy["sourcePlatform"]
                != if profile == "platyr" {
                    "mac"
                } else {
                    "windows"
                }
        {
            return Err(());
        }
        keys(
            &policy["identity"],
            &["accountUuid", "organizationUuid"],
            &[],
        )?;
        for v in policy["identity"].as_object().ok_or(())?.values() {
            if !uuid(text(v)?) {
                return Err(());
            }
        }
        keys(
            &policy["project"],
            &["cwd", "originCwd", "transcriptRoot"],
            &[],
        )?;
        if policy["project"]["cwd"] != "/mnt/Cursor/PuppetMaster"
            || policy["project"]["originCwd"] != policy["project"]["cwd"]
        {
            return Err(());
        }
        let transcript = clean_text(&policy["project"]["transcriptRoot"], 4096)?;
        if !transcript.starts_with('/')
            || transcript.contains('\\')
            || transcript.contains("//")
            || transcript.ends_with('/')
            || transcript.split('/').any(|s| matches!(s, "." | ".."))
        {
            return Err(());
        }
        keys(&policy["ssh"], &["mac", "windows"], &[])?;
        for p in ["mac", "windows"] {
            let e = &policy["ssh"][p];
            keys(e, &["alias", "hostname", "username", "port"], &[])?;
            for k in ["alias", "hostname", "username"] {
                let s = text(&e[k])?;
                if s.is_empty() || s.len() > 255 || s.bytes().any(|b| !(32..127).contains(&b)) {
                    return Err(());
                }
            }
            let alias = text(&e["alias"])?;
            if !alias.as_bytes()[0].is_ascii_alphanumeric()
                || !alias
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                || !e["port"].as_u64().is_some_and(|n| (1..=65535).contains(&n))
            {
                return Err(());
            }
        }
        if let Some(email) = args.get("expectedEmail") {
            let s = text(email)?;
            if s.is_empty() || s.len() > 254 || s.bytes().any(|b| !(32..127).contains(&b)) {
                return Err(());
            }
        }
        #[cfg(feature = "parity-fixtures")]
        let fixture = args.get("_historyFixture");
        #[cfg(not(feature = "parity-fixtures"))]
        let fixture: Option<&Value> = {
            if args.get("_historyFixture").is_some() {
                return Err(());
            }
            None
        };
        if fixture.is_none()
            && ((platform == "mac") != cfg!(target_os = "macos")
                || (platform == "windows") != cfg!(windows))
        {
            return Err(());
        }
        let home = match fixture {
            Some(f) => PathBuf::from(text(&f["home"])?),
            None => std::env::var_os("HOME").map(PathBuf::from).ok_or(())?,
        };
        // The agent's one home rule (crate::files): Python reads `Path.home()`, which rewrites
        // other spellings, so only an already-canonical home is proved here.
        if !home.to_str().is_some_and(crate::files::canonical_home) {
            return Err(());
        }
        let root = if platform == "mac" {
            home.join("Library/Application Support")
                .join(if profile == "platyr" {
                    "Claude".into()
                } else {
                    format!("Claude-{profile}")
                })
        } else if profile == "gmail" {
            home.join("AppData/Local/Packages/Claude_pzs8sxrjxfjjc/LocalCache/Roaming/Claude")
        } else {
            home.join("AppData/Roaming")
                .join(format!("Claude-{profile}"))
        };
        Ok(Self {
            profile,
            platform,
            policy,
            home,
            root,
            fixture,
        })
    }
    fn bind(&self) -> Checked<(PathBuf, Value, Value)> {
        directory(&self.root)?;
        let cfg = object(&regular(&self.root.join("config.json"), false, false)?.ok_or(())?)?;
        let account = text(&cfg["lastKnownAccountUuid"])?;
        if !uuid(account)
            || !account.eq_ignore_ascii_case(text(&self.policy["identity"]["accountUuid"])?)
        {
            return Err(());
        }
        let account_root = self.root.join("claude-code-sessions").join(account);
        directory(&account_root)?;
        let orgs: Vec<_> = entries(&account_root, 200)?
            .into_iter()
            .filter(|e| e.file_name().to_str().is_some_and(uuid))
            .collect();
        if orgs.len() != 1 {
            return Err(());
        }
        let org = orgs[0].file_name().into_string().map_err(|_| ())?;
        if !org.eq_ignore_ascii_case(text(&self.policy["identity"]["organizationUuid"])?) {
            return Err(());
        }
        let registry = orgs[0].path();
        directory(&registry)?;
        let cfg = object(&regular(&self.root.join("ssh_configs.json"), false, false)?.ok_or(())?)?;
        let endpoint = &self.policy["ssh"][self.platform];
        if cfg["configs"]
            .as_array()
            .ok_or(())?
            .iter()
            .filter(|v| v.is_object() && v["sshHost"] == endpoint["alias"])
            .count()
            != 1
        {
            return Err(());
        }
        let raw = regular(&self.home.join(".ssh/config"), false, false)?.ok_or(())?;
        let ssh = std::str::from_utf8(raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&raw))
            .map_err(|_| ())?;
        if !proof::ascii(ssh) {
            return Err(());
        }
        let alias = text(&endpoint["alias"])?;
        let mut selected = false;
        let mut blocks = Vec::new();
        let mut fields = serde_json::Map::new();
        for line in ssh
            .split(['\n', '\r', '\u{b}', '\u{c}'])
            .chain(std::iter::once("Host __ccs_end__"))
        {
            let line = proof::strip(line);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let split = line.find(proof::space).ok_or(())?;
            let key = line[..split].to_ascii_lowercase();
            let value = proof::strip(&line[split..]).trim_matches('"');
            if matches!(
                key.as_str(),
                "include"
                    | "match"
                    | "proxycommand"
                    | "proxyjump"
                    | "localcommand"
                    | "remotecommand"
                    | "permitlocalcommand"
            ) {
                return Err(());
            }
            if key == "host" {
                if selected {
                    blocks.push(std::mem::take(&mut fields));
                }
                let patterns: Vec<_> = proof::words(value).collect();
                // Wildcard/negation parsing remains the Python reference's responsibility.
                if patterns
                    .iter()
                    .any(|p| p.bytes().any(|b| b"!*?[]".contains(&b)))
                {
                    return Err(());
                }
                selected = patterns.contains(&alias);
                fields.clear();
            } else if selected
                && matches!(key.as_str(), "hostname" | "user" | "port")
                && fields.insert(key, Value::String(value.into())).is_some()
            {
                return Err(());
            }
        }
        if blocks.len() != 1 {
            return Err(());
        }
        let f = &mut blocks[0];
        f.entry("port").or_insert(Value::String("22".into()));
        let port = endpoint["port"].as_u64().ok_or(())?.to_string();
        if f.len() != 3
            || f.get("hostname") != Some(&endpoint["hostname"])
            || f.get("user") != Some(&endpoint["username"])
            || f.get("port").and_then(Value::as_str) != Some(port.as_str())
        {
            return Err(());
        }
        Ok((
            registry,
            json!({"accountSha256":hash(account.to_ascii_lowercase().as_bytes()),"orgSha256":hash(org.to_ascii_lowercase().as_bytes())}),
            json!({"hostnameSha256":hash(text(&endpoint["hostname"])?.as_bytes()),"usernameSha256":hash(text(&endpoint["username"])?.as_bytes()),"portSha256":hash(port.as_bytes())}),
        ))
    }
    fn protection(&self, registry: &Path) -> Checked<(Value, Option<Vec<u8>>)> {
        let mut files = Vec::new();
        let mut tasks = None;
        for (relative, path) in [
            ("config.json", self.root.join("config.json")),
            ("ssh_configs.json", self.root.join("ssh_configs.json")),
            (
                "ssh-remote-server-state.json",
                self.root.join("ssh-remote-server-state.json"),
            ),
            (
                "scheduled-tasks.json",
                registry.join("scheduled-tasks.json"),
            ),
            ("ssh-user-config", self.home.join(".ssh/config")),
        ] {
            let raw = regular(&path, true, false)?;
            files.push(json!({"relative":relative,"present":raw.is_some(),"sha256":raw.as_ref().map(|r| hash(r))}));
            if relative == "scheduled-tasks.json" {
                tasks = raw;
            }
        }
        let waiting = registry.join("waiting-input");
        let waiting_present = match fs::symlink_metadata(&waiting) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => return Err(()),
            Ok(_) => true,
        };
        let waiting_state = if waiting_present {
            let id = directory(&waiting)?;
            let mut names: Vec<_> = entries(&waiting, 200)?
                .into_iter()
                .map(|e| hash(e.file_name().as_encoded_bytes()))
                .collect();
            names.sort();
            json!({"present":true,"entryNameHashes":names,"directory":id})
        } else {
            json!({"present":false,"entryNameHashes":[]})
        };
        let mut state = json!({"files":files,"directories":{"profile":directory(&self.root)?,"registry":directory(registry)?},"waitingState":waiting_state});
        let stamp = hash(&encoded(&state)?);
        state["stamp"] = Value::String(stamp);
        Ok((state, tasks))
    }
    fn closed(&self) -> Checked<bool> {
        if let Some(f) = self.fixture {
            return f["closed"].as_bool().ok_or(());
        }
        // The no-process case is exact. Active-process argv/alias qualification delegates.
        if self.platform != "mac" {
            return Err(());
        }
        let out = std::process::Command::new("/usr/bin/pgrep")
            .args(["-x", "Claude"])
            .output()
            .map_err(|_| ())?;
        if out.status.code() == Some(1) && out.stdout.is_empty() {
            directory(&self.root)?;
            Ok(true)
        } else {
            Err(())
        }
    }
    fn native(&self) -> Checked<Value> {
        if let Some(f) = self.fixture {
            return f.get("native").cloned().ok_or(());
        }
        if self.platform != "mac" {
            return Err(());
        }
        mac_native()
    }

    fn unchanged(&self, args: &Value) -> Checked<Value> {
        // Candidate-bearing checks include the writer's owned staging links and neutral
        // record contract; delegate that complete guard to the reference transaction.
        if args
            .get("records")
            .is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()))
        {
            return Err(());
        }
        let expected = args.get("expectedTarget").ok_or(())?;
        if expected["profileId"] != self.profile
            || expected["platform"] != self.platform
            || expected["snapshotStable"] != true
            || expected["protectedSnapshotStable"] != true
            || !self.closed()?
        {
            return Err(());
        }
        let current = self.collect()?;
        for key in [
            "localProfileClosed",
            "snapshotStable",
            "protectedSnapshotStable",
            "noPendingInput",
            "noScheduledWork",
        ] {
            if current[key] != true {
                return Err(());
            }
        }
        for key in [
            "identity",
            "endpoint",
            "nativeGuard",
            "protectedSnapshot",
            "records",
            "revision",
        ] {
            if current.get(key) != expected.get(key) {
                return Err(());
            }
        }
        if !self.closed()? {
            return Err(());
        }
        Ok(json!({"unchanged":true}))
    }
    fn record_names(&self, registry: &Path) -> Checked<Vec<String>> {
        let mut names: Vec<_> = entries(registry, 1024)?
            .into_iter()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|s| s.starts_with("local_") && s.ends_with(".json"))
            .collect();
        names.sort();
        if names.len() > 200 {
            return Err(());
        }
        Ok(names)
    }
    fn collect(&self) -> Checked<Value> {
        let (registry, identity, endpoint) = self.bind()?;
        let (before, _) = self.protection(&registry)?;
        let names = self.record_names(&registry)?;
        let mut records = Vec::new();
        let mut decoded = Vec::new();
        let mut ids = HashSet::new();
        let mut cids = HashSet::new();
        let mut total = 0;
        let mut stable = true;
        for name in &names {
            if !self::name(name) {
                return Err(());
            }
            let raw = regular(&registry.join(name), false, false)?.ok_or(())?;
            total += raw.len();
            if total > 16_000_000 {
                return Err(());
            }
            let value = object(&raw)?;
            let sid = text(&value["sessionId"])?;
            let cid = text(&value["cliSessionId"])?;
            if format!("{sid}.json") != *name
                || !uuid(cid)
                || !ids.insert(sid.to_string())
                || !cids.insert(cid.to_string())
            {
                return Err(());
            }
            records.push(json!({"name":name,"base64":STANDARD.encode(&raw),"sha256":hash(&raw)}));
            decoded.push(value);
        }
        for row in &records {
            if regular(&registry.join(text(&row["name"])?), false, false)?.ok_or(())?
                != STANDARD.decode(text(&row["base64"])?).map_err(|_| ())?
            {
                stable = false;
            }
        }
        let after_names = self.record_names(&registry)?;
        let (after, tasks) = self.protection(&registry)?;
        let (_, after_identity, after_endpoint) = self.bind()?;
        stable = stable
            && names == after_names
            && identity == after_identity
            && endpoint == after_endpoint;
        let empty = |v: &Value| {
            v.is_null() || *v == false || *v == "" || *v == json!([]) || *v == json!({}) || *v == 0
        };
        let pending_keys = [
            "pendingFirstStart",
            "pendingMessages",
            "armedWork",
            "armedWorkAtQuit",
            "query",
            "inputStream",
            "resumeSeed",
            "heldAcrossLoss",
        ];
        let no_pending = after["waitingState"]["entryNameHashes"]
            .as_array()
            .ok_or(())?
            .is_empty()
            && decoded
                .iter()
                .all(|v| pending_keys.iter().all(|k| empty(&v[*k])));
        let no_scheduled = match tasks {
            None => true,
            Some(t) => object(&t)?["scheduledTasks"] == json!([]),
        } && decoded
            .iter()
            .all(|v| v["scheduledTaskId"].is_null() || v["scheduledTaskId"] == "");
        let revision = hash(&encoded(&Value::Array(
            records
                .iter()
                .map(|r| json!([r["name"], r["sha256"]]))
                .collect(),
        ))?);
        Ok(
            json!({"profileId":self.profile,"platform":self.platform,"identity":identity,"records":records,"revision":revision,"snapshotStable":stable,"endpoint":endpoint,"nativeGuard":self.native()?,"noPendingInput":no_pending,"noScheduledWork":no_scheduled,"protectedSnapshotStable":before==after,"protectedSnapshot":after,"localProfileClosed":self.closed()?,"pendingInputState":if no_pending {"no_waiting_records_or_saved_pending_fields"}else{"waiting_or_saved_pending_state_unknown"}}),
        )
    }
}

pub(crate) fn run(args: &Value) -> Result<Value, Failure> {
    if !proof::proven(args) {
        return Ok(proof::fallback());
    }
    // These paths keep the exact packaged writer, lease/receipt and pending-marker protocol.
    // In particular, an append fallback is emitted BEFORE any filesystem mutation.
    if !matches!(
        args["mode"].as_str(),
        Some("collect" | "closed-check" | "protected-check" | "snapshot-cleanup")
    ) || args.get("transactionSource").is_some()
    {
        return Ok(proof::fallback());
    }
    let result = (|| {
        let bound = Bound::new(args)?;
        if args["mode"] == "snapshot-cleanup" {
            #[cfg(unix)]
            {
                bound.bind()?;
                let receipt = aac_files::history_snapshots::prune_history_snapshots(&bound.root)
                    .map_err(|_| ())?;
                return Ok(json!(receipt));
            }
            #[cfg(not(unix))]
            return Err(());
        }
        if args["mode"] == "closed-check" {
            bound.bind()?;
            return Ok(json!({"closed":bound.closed()?}));
        }
        if args["mode"] == "protected-check" {
            return bound.unchanged(args);
        }
        bound.collect()
    })();
    match result {
        Ok(v) if proof::proven(&v) => Ok(v),
        _ => Ok(proof::fallback()),
    }
}

#[cfg(unix)]
fn mac_native() -> Checked<Value> {
    use std::io::{Seek, SeekFrom, Write};
    use std::os::unix::fs::MetadataExt;
    use std::process::{Command, Stdio};
    let root = Path::new("/Applications/Claude.app/Contents");
    let plist = regular(&root.join("Info.plist"), false, false)?.ok_or(())?;
    // Convert the already checked bytes over stdin; no path or command comes from a request.
    // Binary and XML plists use Apple's parser, just as Python uses its plist parser.
    let mut child = Command::new("/usr/bin/plutil")
        .args(["-convert", "json", "-o", "-", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ())?;
    let write = child
        .stdin
        .take()
        .ok_or(())
        .and_then(|mut input| input.write_all(&plist).map_err(|_| ()));
    if write.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(());
    }
    let output = child.wait_with_output().map_err(|_| ())?;
    if !output.status.success() || output.stdout.len() > 2_000_000 {
        return Err(());
    }
    let metadata: Value = serde_json::from_slice(&output.stdout).map_err(|_| ())?;
    if metadata["CFBundleShortVersionString"] != "2.19675.0" {
        return Err(());
    }
    let archive = root.join("Resources/app.asar");
    directory(archive.parent().ok_or(())?)?;
    // `before` is the descriptor's own metadata: the checked, opened inode.
    let (mut file, before) = crate::files::open_regular(&archive, 1_000_000_000).ok_or(())?;
    let mut header = [0u8; 16];
    file.read_exact(&mut header).map_err(|_| ())?;
    let word =
        |i| u32::from_le_bytes([header[i], header[i + 1], header[i + 2], header[i + 3]]) as usize;
    let header_size = word(4);
    let json_size = word(12);
    if json_size == 0 || json_size >= 20_000_000 || header_size < json_size {
        return Err(());
    }
    let mut raw = vec![0u8; json_size];
    file.read_exact(&mut raw).map_err(|_| ())?;
    let mut entry: Value = serde_json::from_slice(&raw).map_err(|_| ())?;
    for name in [".vite", "build", "index.chunk-DPWtnchX.js"] {
        entry = entry
            .get("files")
            .and_then(|v| v.get(name))
            .cloned()
            .ok_or(())?;
    }
    let size = entry["size"]
        .as_u64()
        .filter(|n| *n > 0 && *n < 10_000_000)
        .ok_or(())?;
    if entry
        .get("unpacked")
        .is_some_and(|v| *v != false && !v.is_null())
        || entry.get("link").is_some_and(|v| *v != "" && !v.is_null())
    {
        return Err(());
    }
    let offset = text(&entry["offset"])?.parse::<u64>().map_err(|_| ())?;
    let start = 8u64
        .checked_add(header_size as u64)
        .and_then(|n| n.checked_add(offset))
        .ok_or(())?;
    if start.checked_add(size).is_none_or(|n| n > before.len()) {
        return Err(());
    }
    file.seek(SeekFrom::Start(start)).map_err(|_| ())?;
    let mut bytes = vec![0u8; size as usize];
    file.read_exact(&mut bytes).map_err(|_| ())?;
    let after = fs::symlink_metadata(&archive).map_err(|_| ())?;
    let stamp = |m: &fs::Metadata| (m.dev(), m.ino(), m.len(), m.mtime(), m.mtime_nsec());
    const EXPECTED: &str = "102d49173311cfcba1bfa62abc5ea986797f5b97ec16caa3784ee65cd19774ac";
    if stamp(&before) != stamp(&after) || hash(&bytes) != EXPECTED {
        return Err(());
    }
    Ok(
        json!({"version":"2.19675.0","managerSha256":EXPECTED,"warmGuardVerified":true,"autoResumeGuardVerified":true}),
    )
}
#[cfg(not(unix))]
fn mac_native() -> Checked<Value> {
    Err(())
}
