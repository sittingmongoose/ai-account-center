//! History wire admission and guarded create-only writes.
use super::*;
use aac_files::history_transaction::{self as storage, Guard, Record};
use std::collections::HashMap;
fn decode(records: &Value) -> Checked<Vec<(String, Vec<u8>, Value)>> {
    let records = records.as_array().filter(|r| r.len() <= 200).ok_or(())?;
    let mut out = vec![];
    let mut ids = HashSet::new();
    let mut cli = HashSet::new();
    let mut total = 0;
    for r in records {
        keys(r, &["name", "base64", "sha256"], &[])?;
        let n = text(&r["name"])?;
        let b64 = text(&r["base64"])?;
        let sha = text(&r["sha256"])?;
        if !name(n)
            || sha.len() != 64
            || !sha
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || b64.len() > 2_666_668
        {
            return Err(());
        }
        let raw = STANDARD.decode(b64).map_err(|_| ())?;
        total += raw.len();
        if raw.len() > 2_000_000
            || total > 16_000_000
            || STANDARD.encode(&raw) != b64
            || hash(&raw) != sha
        {
            return Err(());
        }
        let value = object(&raw)?;
        let sid = text(&value["sessionId"])?;
        let cid = text(&value["cliSessionId"])?;
        if format!("{sid}.json") != n
            || !uuid(cid)
            || !ids.insert(sid.to_owned())
            || !cli.insert(cid.to_owned())
        {
            return Err(());
        }
        out.push((n.into(), raw, value));
    }
    Ok(out)
}
fn reference(value: &Value, policy: &Value) -> Checked<String> {
    let raw = clean_text(&value["sshRemoteTranscriptPath"], 4096)?;
    let root = text(&policy["project"]["transcriptRoot"])?;
    let suffix = raw
        .strip_prefix(root)
        .and_then(|r| r.strip_prefix('/'))
        .ok_or(())?;
    let pieces = suffix.split('/').collect::<Vec<_>>();
    if pieces.len() != 2
        || pieces
            .iter()
            .any(|p| p.is_empty() || matches!(*p, "." | ".."))
        || raw.contains('\\')
        || pieces[1] != format!("{}.jsonl", text(&value["cliSessionId"])?)
    {
        return Err(());
    }
    Ok(raw.into())
}
fn neutral(value: &Value, bound: &Bound<'_>) -> Checked<()> {
    keys(
        value,
        &[
            "sessionId",
            "cliSessionId",
            "cwd",
            "originCwd",
            "createdAt",
            "lastActivityAt",
            "lastFocusedAt",
            "title",
            "isArchived",
            "permissionMode",
            "importedFrom",
            "resumeConfirmed",
            "remoteControlAutoEligible",
            "sshConfig",
            "sshRemoteTranscriptPath",
        ],
        &["model", "effort", "completedTurns"],
    )?;
    if value["cwd"] != bound.policy["project"]["cwd"]
        || value["originCwd"] != bound.policy["project"]["originCwd"]
        || value["sshConfig"] != json!({"sshHost":bound.policy["ssh"][bound.platform]["alias"]})
        || (value["lastFocusedAt"].as_f64() != Some(0.) && value["lastFocusedAt"] != false)
        || value["permissionMode"] != "default"
        || value["importedFrom"] != "local-1p-code"
        || value["resumeConfirmed"] != false
        || value["remoteControlAutoEligible"] != false
        || !value["isArchived"].is_boolean()
    {
        return Err(());
    }
    for k in ["createdAt", "lastActivityAt"] {
        if !value[k].as_f64().is_some_and(|n| n.is_finite() && n >= 0.) {
            return Err(());
        }
    }
    clean_text(&value["title"], 1000)?;
    reference(value, bound.policy)?;
    if let Some(v) = value.get("model") {
        let s = text(v)?.strip_prefix("claude-").ok_or(())?;
        if s.is_empty()
            || s.len() > 100
            || !s
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(());
        }
    }
    if let Some(v) = value.get("effort")
        && !["low", "medium", "high", "max", "xhigh"].contains(&text(v)?)
    {
        return Err(());
    }
    if let Some(v) = value.get("completedTurns")
        && !v.as_u64().is_some_and(|v| v <= 1_000_000)
    {
        return Err(());
    }
    Ok(())
}
pub(super) fn references(records: &Value, policy: &Value) -> Checked<Vec<String>> {
    decode(records)?
        .iter()
        .map(|(_, _, v)| reference(v, policy))
        .collect()
}
pub(super) fn unchanged(bound: &Bound<'_>, args: &Value) -> Checked<Value> {
    let expected = args.get("expectedTarget").ok_or(())?;
    if expected["profileId"] != bound.profile
        || expected["platform"] != bound.platform
        || expected["snapshotStable"] != true
        || expected["protectedSnapshotStable"] != true
    {
        return Err(());
    }
    let baseline = decode(&expected["records"])?;
    let empty = json!([]);
    let candidates = decode(args.get("records").unwrap_or(&empty))?;
    for (_, _, v) in &candidates {
        neutral(v, bound)?;
    }
    let mut sorted = baseline.iter().collect::<Vec<_>>();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let revision = hash(&encoded(&Value::Array(
        sorted.iter().map(|(n, b, _)| json!([n, hash(b)])).collect(),
    ))?);
    if expected["revision"] != revision {
        return Err(());
    }
    let ids = baseline
        .iter()
        .map(|(_, _, v)| text(&v["cliSessionId"]))
        .collect::<Checked<HashSet<_>>>()?;
    if candidates.iter().any(|(_, _, v)| {
        v["cliSessionId"]
            .as_str()
            .is_some_and(|id| ids.contains(id))
    }) {
        return Err(());
    }
    if !bound.closed()? {
        return Err(());
    }
    let owned = candidates
        .iter()
        .map(|r| r.0.clone())
        .collect::<HashSet<_>>();
    let current = bound.collect_owned(&owned)?;
    for k in [
        "localProfileClosed",
        "snapshotStable",
        "protectedSnapshotStable",
        "noPendingInput",
        "noScheduledWork",
    ] {
        if current[k] != true {
            return Err(());
        }
    }
    for k in ["identity", "endpoint", "nativeGuard", "protectedSnapshot"] {
        if current[k] != expected[k] {
            return Err(());
        }
    }
    let mut allowed = HashMap::new();
    for (name, raw, _) in &baseline {
        allowed.insert(name, raw);
    }
    for (name, raw, _) in &candidates {
        if allowed.insert(name, raw).is_some() {
            return Err(());
        }
    }
    let current = decode(&current["records"])?;
    let names = current.iter().map(|r| r.0.as_str()).collect::<HashSet<_>>();
    if baseline.iter().any(|r| !names.contains(r.0.as_str()))
        || current
            .iter()
            .any(|(n, raw, _)| allowed.get(n).is_none_or(|b| *b != raw))
        || !bound.closed()?
    {
        return Err(());
    }
    Ok(json!({"unchanged":true}))
}
struct BoundGuard<'a, 'b> {
    bound: &'a Bound<'b>,
    args: &'a Value,
}
impl Guard for BoundGuard<'_, '_> {
    fn closed(&mut self) -> bool {
        unchanged(self.bound, self.args).is_ok()
    }
    fn private(&mut self, path: &Path, dir: bool) -> bool {
        if let Some(fixture) = self.bound.fixture {
            return fixture["private"].as_bool().unwrap_or(true);
        }
        native::private(path, dir)
    }
}
pub(super) fn append(bound: &Bound<'_>, args: &Value) -> Checked<Value> {
    unchanged(bound, args)?;
    let records = decode(&args["records"])?;
    if records.is_empty() {
        return Err(());
    }
    if args.get("transactionSource").is_some() {
        return Err(());
    } // No executable source is accepted by the native writer.
    let (registry, _, _) = bound.bind()?;
    let mut protected = Vec::new();
    for n in [
        "config.json",
        "ssh_configs.json",
        "ssh-remote-server-state.json",
    ] {
        protected.push(Record {
            name: n.into(),
            bytes: regular(&bound.root.join(n), true, false)?,
        });
    }
    let records = records
        .into_iter()
        .map(|(name, bytes, _)| Record {
            name,
            bytes: Some(bytes),
        })
        .collect::<Vec<_>>();
    let mut guard = BoundGuard { bound, args };
    Ok(storage::append(
        &bound.root,
        &registry,
        &records,
        &protected,
        bound.platform == "windows",
        &mut guard,
    ))
}
pub(super) fn cleanup(bound: &Bound<'_>) -> Checked<Value> {
    bound.bind()?;
    #[cfg(unix)]
    {
        aac_files::history_snapshots::prune_history_snapshots(&bound.root)
            .map(|v| json!(v))
            .map_err(|_| ())
    }
    #[cfg(not(unix))]
    {
        storage::cleanup(&bound.root).map_err(|_| ())
    }
}
