//! Claude trash approvals and host receipts for the worker-free boot.
use crate::{
    account_approvals::Approvals,
    agent_transport::{Agents, Selector},
    claude_account_store::{read_trash, trash_id},
    claude_lifecycle::{self, Effect, Workflow},
    claude_open::manifest,
    lifecycle_queue::LifecycleQueues,
    public_auth::{
        DashboardAccess, DashboardEvent, DashboardEvents, Method, Reply, Request, error, lifecycle,
        now, response, storage,
    },
};
use aac_files::{
    OwnedStore,
    read::{self, Read, ReadPolicy},
    registry::{self, Owners},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Copy)]
pub(super) enum Action {
    Restore,
    Purge,
}

impl Action {
    fn wire(self) -> &'static str {
        match self {
            Self::Restore => "trash-restore",
            Self::Purge => "trash-purge",
        }
    }

    fn accepts(self, entry: &Value) -> bool {
        entry["state"] == "trashed" || (matches!(self, Self::Purge) && entry["state"] == "deleting")
    }
}

pub(super) fn route<'a>(method: &Method, path: &'a str) -> Option<(Action, &'a str)> {
    if !matches!(*method, Method::POST | Method::OPTIONS) {
        return None;
    }
    let (id, action) = path.strip_prefix("/api/accounts/trash/")?.split_once('/')?;
    let action = match action {
        "restore" => Action::Restore,
        "purge" => Action::Purge,
        _ => return None,
    };
    (!id.is_empty()).then_some((action, id))
}

struct Refusal {
    status: u16,
    code: &'static str,
    message: &'static str,
    host: Option<&'static str>,
}

impl Refusal {
    const UNKNOWN: Self = Self::new(404, "unknown_trash", "That trash entry was not found.");
    const STALE: Self = Self::new(
        409,
        "confirmation_stale",
        "The account changed. Review it again.",
    );
    const IN_USE: Self = Self::new(
        409,
        "id_in_use",
        "That name is already used by another account.",
    );
    const WRITE: Self = Self::new(500, "write_failed", "The change could not be saved safely.");
    const PURGE: Self = Self::new(
        500,
        "purge_failed",
        "The profile could not be deleted safely. It stays in the trash.",
    );
    const RESTORE: Self = Self::new(
        500,
        "restore_failed",
        "The account could not be restored safely.",
    );

    const fn new(status: u16, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            code,
            message,
            host: None,
        }
    }

    fn on(mut self, host: &'static str) -> Self {
        self.host = Some(host);
        self
    }

    fn reply(self) -> Reply {
        match self.host {
            Some(host) => super::no_store(response(
                self.status,
                json!({
                    "error":self.message,"code":self.code,"host":host
                }),
            )),
            None => error(self.status, self.code, self.message),
        }
    }
}

pub(super) struct Trash {
    root: PathBuf,
    owners: Owners,
    agents: Arc<Agents>,
    queues: LifecycleQueues,
    approvals: Approvals,
}

impl Trash {
    pub(super) fn new(root: PathBuf, owners: Owners, agents: Arc<Agents>) -> Self {
        Self {
            queues: LifecycleQueues::new(root.clone()),
            root,
            owners,
            agents,
            approvals: Approvals::default(),
        }
    }

    pub(super) async fn handle(
        self: Arc<Self>,
        action: Action,
        raw_id: &str,
        request: Request,
        events: DashboardEvents,
    ) -> Reply {
        if request.method() == Method::OPTIONS {
            return lifecycle::options("POST", "4");
        }
        let session = request
            .extensions()
            .get::<DashboardAccess>()
            .and_then(|access| access.session_id.as_deref())
            .unwrap_or_default()
            .to_owned();
        let body = match lifecycle::mutation_body(request).await {
            Ok(body) => body,
            Err(reply) => return reply,
        };
        let Some(id) = crate::public_auth::decode_component(raw_id).filter(|id| trash_id(id))
        else {
            return error(400, "invalid_account", "That account id is not valid.");
        };
        let token = body
            .get("confirmationToken")
            .and_then(Value::as_str)
            .filter(|token| {
                token.len() == 43
                    && token
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
            });
        let purging = matches!(action, Action::Purge);
        if !body.is_empty()
            && (token.is_none()
                || (purging
                    && (body.len() != 2 || !body.get("confirm").is_some_and(Value::is_string)))
                || (!purging && body.len() != 1))
        {
            return lifecycle::invalid_action_body();
        }
        if std::env::var("CCS_CLAUDE_HOST_LIFECYCLE").as_deref() != Ok("on") {
            return error(
                409,
                "not_implemented",
                "This action is not available on this server yet.",
            );
        }
        let root = self.root.clone();
        let target = id.clone();
        let reviewed = tokio::task::spawn_blocking(move || {
            let entry = find(read_trash(&root).ok_or(Refusal::UNKNOWN)?, action, &target)?;
            if !purging && profile_exists(&root, account_id(&entry), false).unwrap_or(false) {
                return Err(Refusal::IN_USE);
            }
            Ok(entry)
        })
        .await;
        let reviewed = match reviewed {
            Ok(Ok(entry)) => entry,
            Ok(Err(refusal)) => return refusal.reply(),
            Err(_) => return Refusal::WRITE.reply(),
        };
        let fingerprint = fingerprint(&reviewed);
        let Some(token) = token else {
            return self.issue(action, &id, &session, &fingerprint);
        };
        if purging && body.get("confirm").and_then(Value::as_str) != Some("DELETE") {
            return lifecycle::invalid_action_body();
        }
        if self
            .approval("consume", action, &id, &session, &fingerprint, token)
            .is_none_or(|reply| reply["consumed"] != true)
        {
            return Refusal::STALE.reply();
        }
        // An admitted mutation settles its host receipts and registry write even if
        // the caller closes the HTTP connection.
        let outcome = tokio::spawn(async move {
            let reply = match action {
                Action::Purge => {
                    self.queues
                        .run("claude-purge", self.purge(&id, &fingerprint, None))
                        .await
                        .map_err(|_| Refusal::PURGE)??;
                    json!({"purged":true,"trashId":id})
                }
                Action::Restore => {
                    let account = self.restore(&id, &fingerprint).await?;
                    json!({"restored":true,"accountId":account})
                }
            };
            events.publish(DashboardEvent::AccountsChanged {});
            Ok::<_, Refusal>(reply)
        })
        .await;
        match outcome {
            Ok(Ok(reply)) => super::no_store(response(200, reply)),
            Ok(Err(refusal)) => refusal.reply(),
            Err(_) => Refusal::WRITE.reply(),
        }
    }

    fn issue(&self, action: Action, id: &str, session: &str, fingerprint: &str) -> Reply {
        let Ok(random) = storage::random::<32>() else {
            return Refusal::WRITE.reply();
        };
        let token = URL_SAFE_NO_PAD.encode(random);
        let expires = self
            .approval("issue", action, id, session, fingerprint, &token)
            .and_then(|reply| reply["expiresAtMs"].as_i64())
            .and_then(storage::iso);
        let Some(expires) = expires else {
            return Refusal::WRITE.reply();
        };
        let confirmation = match action {
            Action::Restore => json!({"token":token,"expiresAt":expires,"effects":[
                "Its Claude data moves back on Mac and Windows.","Its launchers are created again."
            ]}),
            Action::Purge => json!({"token":token,"expiresAt":expires,"effects":[
                "Its Claude data is deleted for good on Mac and Windows.",
                "This cannot be undone. Type DELETE to confirm."
            ],"expectsTyped":"DELETE"}),
        };
        super::no_store(response(200, json!({"confirmation":confirmation})))
    }

    fn approval(
        &self,
        operation: &str,
        action: Action,
        id: &str,
        session: &str,
        fingerprint: &str,
        token: &str,
    ) -> Option<Value> {
        let binding = json!({
            "operation":operation,"action":action.wire(),"subject":id,
            "sessionKey":storage::sha(&format!("aac-session\0{session}")),
            "stateFingerprint":fingerprint,"tokenDigest":storage::sha(token)
        });
        self.approvals.apply(binding.to_string().as_bytes())
    }

    fn claim(&self, id: &str) -> Result<OwnedStore, Refusal> {
        let rows = registry::rows().map_err(|_| Refusal::WRITE)?;
        self.owners
            .claim(rows, id, &self.root, None)
            .map_err(|_| Refusal::WRITE)
    }

    async fn purge(&self, id: &str, reviewed: &str, due_by: Option<i64>) -> Result<(), Refusal> {
        let entry = {
            let writer = self.claim("claude-trash").map_err(|_| Refusal::PURGE)?;
            let mut entries = read_trash(&self.root).ok_or(Refusal::PURGE)?;
            let entry = current(&mut entries, Action::Purge, id, reviewed)?;
            if protected(entry) {
                return Err(Refusal::new(
                    409,
                    "account_protected",
                    "This is this computer's default Claude profile. It can't be removed here.",
                ));
            }
            if due_by.is_some_and(|cutoff| !due(entry, cutoff)) {
                return Err(Refusal::UNKNOWN);
            }
            entry["state"] = json!("deleting");
            let saved = entry.clone();
            save(&writer, &json!({"version":1,"entries":entries})).map_err(|_| Refusal::PURGE)?;
            saved
        };
        claude_lifecycle::run(Workflow::Purge, |effect| {
            let entry = &entry;
            async move {
                if matches!(effect, Effect::Commit) {
                    let writer = self.claim("claude-trash").map_err(|_| Refusal::PURGE)?;
                    let mut entries = read_trash(&self.root).ok_or(Refusal::PURGE)?;
                    if let Some(position) = entries
                        .iter()
                        .position(|row| row["trashId"] == id && row == entry)
                    {
                        entries.remove(position);
                        save(&writer, &json!({"version":1,"entries":entries}))
                            .map_err(|_| Refusal::PURGE)?;
                    }
                    return Ok(true);
                }
                let host = host(effect);
                let detail = &entry["hosts"][host];
                if detail.is_null() {
                    return Ok(false);
                }
                let trash_name = detail["trashName"].as_str().ok_or(Refusal::PURGE)?;
                let expires = u64::try_from(now()).map_err(|_| Refusal::PURGE)? + 120_000;
                let token = aac_core::claude_purge::issue(host, trash_name, expires)
                    .ok_or(Refusal::PURGE)?;
                let args = json!({"host":host,"op":"purge","trashName":trash_name,"token":token});
                let result = self.host_call(host, detail, args).await;
                if result.is_some_and(|result| result["ok"] == true) {
                    Ok(true)
                } else {
                    Err(Refusal::new(
                        502,
                        "host_unreachable",
                        "A computer could not be reached. Nothing was changed.",
                    )
                    .on(host))
                }
            }
        })
        .await
    }

    pub(super) async fn sweep_due(&self) {
        if std::env::var("CCS_CLAUDE_HOST_LIFECYCLE").as_deref() != Ok("on") {
            return;
        }
        let _ = self
            .queues
            .run("claude-purge", async {
                let cutoff = now();
                let Some(entries) = read_trash(&self.root) else {
                    return;
                };
                for entry in entries.iter().filter(|entry| due(entry, cutoff)) {
                    if let Some(id) = entry["trashId"].as_str() {
                        let _ = self.purge(id, &fingerprint(entry), Some(cutoff)).await;
                    }
                }
            })
            .await;
    }

    async fn restore(&self, id: &str, reviewed: &str) -> Result<String, Refusal> {
        let writer = self.claim("claude-trash")?;
        let mut entries = read_trash(&self.root).ok_or(Refusal::RESTORE)?;
        let entry = current(&mut entries, Action::Restore, id, reviewed)?.clone();
        let account = account_id(&entry);
        let pending = self.claim("claude-pending")?;
        let inventory = self.claim("claude-desktop-profiles")?;
        if profile_exists(&self.root, account, true)? {
            return Err(Refusal::IN_USE);
        }
        let (source, target) = if entry["source"] == "pending" {
            ("claude-pending", &pending)
        } else {
            ("claude-desktop-profiles", &inventory)
        };
        let mut document = profile_document(&self.root, source)?;
        document["profiles"]
            .as_array_mut()
            .ok_or(Refusal::WRITE)?
            .push(entry["entry"].clone());
        if source == "claude-pending" && manifest::parse_pending(&document).is_none() {
            return Err(Refusal::WRITE);
        }
        claude_lifecycle::run(Workflow::Restore, |effect| {
            let entry = &entry;
            let document = &document;
            async move {
                if matches!(effect, Effect::Commit) {
                    save(target, document)?;
                    return Ok(true);
                }
                let host = host(effect);
                let detail = &entry["hosts"][host];
                let Some(mut args) = detail["launcher"].as_object().cloned() else {
                    return Ok(false);
                };
                let undo = matches!(effect, Effect::UndoMac | Effect::UndoWindows);
                let op = if undo { "trash" } else { "restore" };
                args.insert("op".into(), json!(op));
                args.insert("host".into(), json!(host));
                args.insert("profileId".into(), json!(account));
                args.insert("trashName".into(), detail["trashName"].clone());
                let receipt = self.host_call(host, detail, Value::Object(args)).await;
                if receipt.is_some_and(|value| {
                    if undo {
                        value["result"] == "moved"
                    } else {
                        value["ok"] == true
                    }
                }) {
                    Ok(true)
                } else {
                    Err(Refusal::RESTORE.on(host))
                }
            }
        })
        .await?;
        entries.retain(|row| row["trashId"] != id);
        save(&writer, &json!({"version":1,"entries":entries})).map_err(|_| Refusal::RESTORE)?;
        Ok(format!("claude:{account}"))
    }

    async fn host_call(&self, host: &str, detail: &Value, args: Value) -> Option<Value> {
        let alias = detail["launcher"]["sshHost"].as_str()?;
        self.agents
            .call_value(
                "claude-host",
                Selector::usage(host, Some(alias)),
                args,
                60_000,
                62_000,
                65_536,
            )
            .await
    }
}

fn find(entries: Vec<Value>, action: Action, id: &str) -> Result<Value, Refusal> {
    entries
        .into_iter()
        .find(|entry| entry["trashId"] == id && action.accepts(entry))
        .ok_or(Refusal::UNKNOWN)
}

fn current<'a>(
    entries: &'a mut [Value],
    action: Action,
    id: &str,
    reviewed: &str,
) -> Result<&'a mut Value, Refusal> {
    let entry = entries
        .iter_mut()
        .find(|entry| entry["trashId"] == id && action.accepts(entry))
        .ok_or(Refusal::UNKNOWN)?;
    if fingerprint(entry) != reviewed {
        return Err(Refusal::STALE);
    }
    Ok(entry)
}

fn fingerprint(entry: &Value) -> String {
    aac_core::sha256_hex(aac_views::js_json::write(&json!({
        "trashId":entry["trashId"],"state":entry["state"],"entry":entry["entry"]
    })))
}

fn host(effect: Effect) -> &'static str {
    match effect {
        Effect::ApplyMac | Effect::UndoMac => "mac",
        _ => "windows",
    }
}

fn account_id(entry: &Value) -> &str {
    entry["accountId"]
        .as_str()
        .and_then(|value| value.strip_prefix("claude:"))
        .unwrap_or_default()
}

fn due(entry: &Value, cutoff: i64) -> bool {
    entry["purgeAfter"]
        .as_str()
        .and_then(storage::timestamp)
        .is_some_and(|at| at <= cutoff)
}

fn protected(entry: &Value) -> bool {
    ["mac", "windows"].iter().any(|host| {
        let launcher = &entry["entry"][*host];
        (entry["source"] == "inventory" && launcher["isDefault"] == true)
            || manifest::is_default_data_folder(launcher["profilePath"].as_str())
    })
}

fn profile_document(root: &Path, id: &str) -> Result<Value, Refusal> {
    let row = registry::row(id).map_err(|_| Refusal::WRITE)?;
    let file = root.join(row.file(None).map_err(|_| Refusal::WRITE)?);
    let result = if id == "claude-pending" {
        read::private_store(&file, row.max_bytes)
    } else {
        read::read(&file, &ReadPolicy::capped(row.max_bytes))
    };
    let bytes = match result {
        Read::Absent => return Ok(json!({"version":1,"profiles":[]})),
        Read::Bytes(bytes, _) => bytes,
        _ => return Err(Refusal::WRITE),
    };
    let mut document = crate::claude_open::js::parse(&bytes).ok_or(Refusal::WRITE)?;
    if id == "claude-pending" {
        manifest::parse_pending(&document).ok_or(Refusal::WRITE)?;
        // Existing pending entries predating email verification normalized the
        // optional email to null on each store read.
        let profiles = document["profiles"].as_array_mut().ok_or(Refusal::WRITE)?;
        for entry in profiles {
            let order = [
                "id",
                "label",
                "mac",
                "windows",
                "createdAt",
                "expectedEmail",
            ];
            let normalized = order
                .into_iter()
                .map(|key| (key.to_string(), entry[key].clone()))
                .collect();
            *entry = Value::Object(normalized);
        }
    } else if manifest::manifest_rows(&document)
        .is_none_or(|rows| rows.iter().any(|row| !row.is_object()))
    {
        return Err(Refusal::WRITE);
    }
    Ok(document)
}

fn profile_exists(root: &Path, id: &str, fold: bool) -> Result<bool, Refusal> {
    for store in ["claude-desktop-profiles", "claude-pending"] {
        let doc = profile_document(root, store)?;
        if doc["profiles"].as_array().is_some_and(|rows| {
            rows.iter().any(|row| {
                row["id"]
                    .as_str()
                    .is_some_and(|known| known == id || (fold && known.eq_ignore_ascii_case(id)))
            })
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn save(writer: &OwnedStore, document: &Value) -> Result<(), Refusal> {
    let mut bytes = serde_json::to_vec_pretty(document).map_err(|_| Refusal::WRITE)?;
    bytes.push(b'\n');
    writer.replace_settings(&bytes).map_err(|_| Refusal::WRITE)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const ID: &str = "tr_0123456789abcdef";

    fn fixture() -> (tempfile::TempDir, Trash, Value) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let rows = registry::rows().unwrap();
        let files = rows
            .iter()
            .map(|row| {
                let owner = if row.moves_with.as_deref() == Some("writes") {
                    registry::Role::RustService
                } else {
                    row.owner
                };
                (row.id.clone(), owner.word().to_owned())
            })
            .collect();
        let owners = Owners::from_boot(rows, &files).unwrap();
        let owner = Trash::new(
            root.clone(),
            owners,
            Arc::new(Agents::with_default(root, true)),
        );
        let entry = json!({
            "trashId":ID,"provider":"claude","accountId":"claude:work","label":"Work",
            "source":"inventory","hosts":{},"entry":{"id":"work","email":"person@example.test"},
            "trashedAt":"2026-11-19T00:00:00Z","purgeAfter":"2026-12-19T00:00:00Z",
            "state":"trashed"
        });
        (dir, owner, entry)
    }

    #[tokio::test]
    async fn restore_checks_the_reviewed_state_then_restores_the_exact_inventory_entry() {
        let (_dir, owner, entry) = fixture();
        let writer = owner
            .claim("claude-trash")
            .unwrap_or_else(|_| panic!("trash lease"));
        save(&writer, &json!({"version":1,"entries":[entry]}))
            .unwrap_or_else(|_| panic!("trash write"));
        drop(writer);
        let denied = owner.restore(ID, &"0".repeat(64)).await.unwrap_err();
        assert_eq!(denied.code, "confirmation_stale");
        assert!(profile_exists(&owner.root, "work", true).is_ok_and(|exists| !exists));
        assert_eq!(
            owner
                .restore(ID, &fingerprint(&entry))
                .await
                .unwrap_or_else(|_| panic!("restore")),
            "claude:work"
        );
        let inventory = profile_document(&owner.root, "claude-desktop-profiles")
            .unwrap_or_else(|_| panic!("inventory"));
        assert_eq!(inventory["profiles"], json!([entry["entry"]]));
        assert!(read_trash(&owner.root).unwrap().is_empty());
    }

    #[tokio::test]
    async fn purge_reuses_the_session_bound_one_shot_approval_and_protects_defaults() {
        let (_dir, owner, mut entry) = fixture();
        entry["entry"]["mac"] = json!({"profilePath":"/fixture/Application Support/Claude"});
        let writer = owner
            .claim("claude-trash")
            .unwrap_or_else(|_| panic!("trash lease"));
        save(&writer, &json!({"version":1,"entries":[entry]}))
            .unwrap_or_else(|_| panic!("trash write"));
        drop(writer);
        let fingerprint = fingerprint(&entry);
        let token = "a".repeat(43);
        assert!(
            owner
                .approval(
                    "issue",
                    Action::Purge,
                    ID,
                    "session-a",
                    &fingerprint,
                    &token
                )
                .is_some()
        );
        for session in ["session-b", "session-a"] {
            let result = owner
                .approval("consume", Action::Purge, ID, session, &fingerprint, &token)
                .unwrap();
            assert_eq!(result["consumed"], false);
        }
        let denied = owner.purge(ID, &fingerprint, None).await.unwrap_err();
        assert_eq!(denied.code, "account_protected");
        assert_eq!(read_trash(&owner.root).unwrap(), vec![entry]);
    }
}
