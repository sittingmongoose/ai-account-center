//! Claude trash approvals and host receipts for the worker-free boot.
use super::NativeOwners;
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
use serde_json::{Map, Value, json};
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
    queues: Arc<LifecycleQueues>,
    approvals: Arc<Approvals>,
}

impl Trash {
    pub(super) fn new(
        root: PathBuf,
        owners: Owners,
        agents: Arc<Agents>,
        queues: Arc<LifecycleQueues>,
    ) -> Self {
        Self::with_approvals(root, owners, agents, queues, Arc::default())
    }

    pub(super) fn with_approvals(
        root: PathBuf,
        owners: Owners,
        agents: Arc<Agents>,
        queues: Arc<LifecycleQueues>,
        approvals: Arc<Approvals>,
    ) -> Self {
        Self {
            queues,
            root,
            owners,
            agents,
            approvals,
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
                || (!purging
                    && (body
                        .keys()
                        .any(|key| !matches!(key.as_str(), "confirmationToken" | "confirm"))
                        || body.get("confirm").is_some_and(|value| {
                            value
                                .as_str()
                                .is_none_or(|text| text.encode_utf16().count() > 320)
                        }))))
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
            .approval("consume", action.wire(), &id, &session, &fingerprint, token)
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
            .approval("issue", action.wire(), id, session, fingerprint, &token)
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

    pub(super) fn approval(
        &self,
        operation: &str,
        action: &str,
        id: &str,
        session: &str,
        fingerprint: &str,
        token: &str,
    ) -> Option<Value> {
        let binding = json!({
            "operation":operation,"action":action,"subject":id,
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
            let (entry, document) = (&entry, &document);
            async move {
                if matches!(effect, Effect::Commit) {
                    save(target, document)?;
                    return Ok(true);
                }
                let host = host(effect);
                let detail = &entry["hosts"][host];
                let Some(launcher) = detail.get("launcher") else {
                    return Ok(false);
                };
                let undo = matches!(effect, Effect::UndoMac | Effect::UndoWindows);
                let op = if undo { "trash" } else { "restore" };
                let args = json!({
                    "op":op,"host":host,"profileId":account,
                    "trashName":detail["trashName"],"launcher":launcher
                });
                let receipt = self.host_call(host, detail, args).await;
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

pub(super) async fn add_claude(native: &NativeOwners, body: &Map<String, Value>) -> Reply {
    use crate::{account_usage::sources, claude_account_store::pending_id};
    if body
        .keys()
        .any(|field| !matches!(field.as_str(), "provider" | "profileId" | "email" | "label"))
    {
        return lifecycle::invalid_action_body();
    }
    let Some(id) = body
        .get("profileId")
        .and_then(Value::as_str)
        .filter(|id| pending_id(id))
    else {
        return lifecycle::invalid_action_body();
    };
    let Some(email) = body.get("email").and_then(Value::as_str).filter(|email| {
        email.encode_utf16().count() <= 254 && crate::signin_callbacks::muse_email_valid(email)
    }) else {
        return lifecycle::invalid_action_body();
    };
    let label = match body.get("label") {
        None | Some(Value::Null) => None,
        Some(Value::String(name)) if sources::valid_label(name) => Some(name.clone()),
        _ => return lifecycle::invalid_action_body(),
    };
    let Some(owner) = native.trash.clone().filter(|_| enabled()) else {
        return unavailable();
    };
    let id = id.to_owned();
    let email = email.to_owned();
    let native = native.clone();
    let result = tokio::spawn(async move {
        let profile = owner.add(&id, label, email).await?;
        native.changed();
        Ok::<_, Refusal>(profile)
    })
    .await;
    match result {
        Ok(Ok(profile)) => super::no_store(response(
            201,
            json!({
                "account":{
                    "id":format!("claude:{}",profile["id"].as_str().unwrap_or_default()),"provider":"claude",
                    "label": profile["label"].as_str().or_else(||profile["id"].as_str()),"email":null,"platform":"mac",
                    "credential":null,"hidden":false,"trayHidden":false,"lifecycle":{"state":"pending_sign_in","jobId":null},
                    "actions":{"signInAgain":true,"replaceKey":false,"remove":true,"open":[],"recheck":false},
                    "removeRefusal":null
                },"launchers":{"mac":"created","windows":"created"}
            }),
        )),
        Ok(Err(refusal)) => refusal.reply(),
        Err(_) => Refusal::WRITE.reply(),
    }
}

fn enabled() -> bool {
    std::env::var("CCS_CLAUDE_HOST_LIFECYCLE").as_deref() == Ok("on")
}

fn unavailable() -> Reply {
    error(
        409,
        "not_implemented",
        "This action is not available on this server yet.",
    )
}

impl Trash {
    async fn add(&self, id: &str, label: Option<String>, email: String) -> Result<Value, Refusal> {
        let _trash_writer = self.claim("claude-trash")?;
        let pending_writer = self.claim("claude-pending")?;
        let _inventory_writer = self.claim("claude-desktop-profiles")?;
        let inventory = profile_document(&self.root, "claude-desktop-profiles")?;
        let mut pending = profile_document(&self.root, "claude-pending")?;
        let inventory_rows = inventory["profiles"].as_array().ok_or(Refusal::WRITE)?;
        let pending_rows = pending["profiles"].as_array_mut().ok_or(Refusal::WRITE)?;
        if pending_rows.len() + inventory_rows.len() >= 16 {
            return Err(Refusal::new(
                409,
                "too_many_accounts",
                "This provider already has the most accounts allowed.",
            ));
        }
        let trash = read_trash(&self.root).ok_or(Refusal::WRITE)?;
        if pending_rows.iter().chain(inventory_rows).any(|profile| {
            profile["id"]
                .as_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(id))
        }) || trash.iter().any(|entry| account_id(entry) == id)
        {
            return Err(Refusal::IN_USE);
        }
        let aliases = ["mac", "windows"]
            .into_iter()
            .map(|host| {
                inventory_rows.iter().find_map(|profile| {
                    let alias = profile[host]["sshHost"].as_str()?;
                    (!alias.is_empty()
                        && alias.len() <= 128
                        && alias.as_bytes()[0].is_ascii_alphanumeric()
                        && alias
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)))
                    .then_some(alias)
                })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or(Refusal::new(
                409,
                "not_configured",
                "That app has no launcher on that computer.",
            ))?;
        let created = std::sync::Mutex::new(json!({
            "id":id,"label":label,"mac":null,"windows":null,
            "createdAt":second_stamp(now())?,"expectedEmail":email
        }));
        claude_lifecycle::run(Workflow::Add, |effect| {
            let (created, pending_writer, pending, aliases) =
                (&created, &pending_writer, &pending, &aliases);
            async move {
                if matches!(effect, Effect::Commit) {
                    let mut pending = pending.clone();
                    let created = created.lock().map_err(|_| Refusal::WRITE)?;
                    pending["profiles"]
                        .as_array_mut()
                        .ok_or(Refusal::WRITE)?
                        .push(created.clone());
                    manifest::parse_pending(&pending).ok_or(Refusal::WRITE)?;
                    save(pending_writer, &pending)?;
                    return Ok(true);
                }
                let host = host(effect);
                let alias = aliases[usize::from(host == "windows")];
                if matches!(effect, Effect::UndoMac | Effect::UndoWindows) {
                    let launcher = created.lock().map_err(|_| Refusal::WRITE)?[host].clone();
                    let _ = self
                        .call_host(
                            host,
                            alias,
                            json!({"host":host,"op":"undo","profileId":id,"launcher":launcher}),
                        )
                        .await;
                    return Ok(false);
                }
                let mut receipt = self
                    .call_host(
                        host,
                        alias,
                        json!({"host":host,"op":"create","profileId":id}),
                    )
                    .await
                    .ok_or_else(|| unreachable_host(host))?;
                if let Some(receipt) = receipt.as_object_mut() {
                    receipt.insert("sshHost".into(), json!(alias));
                }
                crate::claude_account_store::parse_pending_launcher(&receipt)
                    .ok_or_else(|| unreachable_host(host))?;
                created.lock().map_err(|_| Refusal::WRITE)?[host] = receipt;
                Ok(true)
            }
        })
        .await?;
        created.into_inner().map_err(|_| Refusal::WRITE)
    }

    async fn call_host(&self, host: &str, alias: &str, args: Value) -> Option<Value> {
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

fn unreachable_host(host: &'static str) -> Refusal {
    Refusal::new(
        502,
        "host_unreachable",
        "A computer could not be reached. Nothing was changed.",
    )
    .on(host)
}

fn second_stamp(time: i64) -> Result<String, Refusal> {
    storage::iso(time - time.rem_euclid(1000))
        .map(|stamp| stamp.replace(".000Z", "Z"))
        .ok_or(Refusal::WRITE)
}

struct ProfileRemoval {
    source: &'static str,
    kind: &'static str,
    entry: Value,
    hosts: Map<String, Value>,
    protected: bool,
    incomplete: bool,
}

impl ProfileRemoval {
    fn fingerprint(&self) -> String {
        aac_core::sha256_hex(aac_views::js_json::write(&json!({
            "source":self.kind,"entry":self.entry
        })))
    }

    fn email(&self) -> Option<&str> {
        if self.kind == "inventory" {
            self.entry["email"].as_str()
        } else {
            None
        }
    }
}

fn removal_profile(root: &Path, id: &str) -> Result<Option<ProfileRemoval>, Refusal> {
    for (source, kind) in [
        ("claude-desktop-profiles", "inventory"),
        ("claude-pending", "pending"),
    ] {
        let document = profile_document(root, source)?;
        let Some(entry) = document["profiles"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["id"] == id))
        else {
            continue;
        };
        let mut hosts = Map::new();
        let mut protected = false;
        let mut incomplete = false;
        for host in ["mac", "windows"] {
            let Some(raw) = entry.get(host).filter(|raw| !raw.is_null()) else {
                continue;
            };
            protected |= (kind == "inventory" && raw["isDefault"] == true)
                || manifest::is_default_data_folder(raw["profilePath"].as_str());
            let launcher = Value::Object(
                [
                    "launcherName",
                    "launcherPath",
                    "profilePath",
                    "startMenuPath",
                    "sshHost",
                ]
                .into_iter()
                .filter_map(|key| Some((key.to_owned(), raw.get(key)?.clone())))
                .collect(),
            );
            if crate::claude_account_store::parse_pending_launcher(&launcher).is_some() {
                hosts.insert(host.to_owned(), launcher);
            } else {
                incomplete = true;
            }
        }
        return Ok(Some(ProfileRemoval {
            source,
            kind,
            entry: entry.clone(),
            hosts,
            protected,
            incomplete,
        }));
    }
    Ok(None)
}

pub(super) async fn remove_claude(
    native: &NativeOwners,
    id: &str,
    session: &str,
    token: Option<&str>,
    typed: Option<&str>,
) -> Reply {
    let Some(owner) = native.trash.clone() else {
        return unavailable();
    };
    let profile = match removal_profile(&owner.root, id) {
        Ok(Some(profile)) => profile,
        _ if token.is_some() => return Refusal::STALE.reply(),
        _ => return error(404, "unknown_account", "That account was not found."),
    };
    let account = format!("claude:{id}");
    if !enabled() && !profile.protected {
        return unavailable();
    }
    if let Err(refusal) = owner.remove_refusal(&profile).await {
        return refusal.reply();
    }
    if super::lifecycle_remove::running(native, &account) {
        return error(
            409,
            "signin_running",
            "A sign-in for this account is running.",
        );
    }
    let fingerprint = profile.fingerprint();
    let Some(token) = token else {
        let Ok(random) = storage::random::<32>() else {
            return Refusal::WRITE.reply();
        };
        let token = URL_SAFE_NO_PAD.encode(random);
        let expires = owner
            .approval("issue", "remove", &account, session, &fingerprint, &token)
            .and_then(|receipt| receipt["expiresAtMs"].as_i64())
            .and_then(storage::iso);
        let Some(expires) = expires else {
            return Refusal::WRITE.reply();
        };
        let first = if profile.protected {
            "This is this computer’s default Claude profile. Its Claude data moves to the trash on Mac and Windows for 30 days."
        } else {
            "Its Claude data moves to the trash on Mac and Windows for 30 days."
        };
        let mut confirmation = json!({"token":token,"expiresAt":expires,"effects":[first,"Its launchers are removed on Mac and Windows."]});
        if profile.protected {
            confirmation["expectsTyped"] = json!("email");
        }
        return super::no_store(response(200, json!({"confirmation":confirmation})));
    };
    if profile.protected {
        let typed = typed.map(|email| email.trim().to_lowercase());
        let expected = profile.email().map(|email| email.trim().to_lowercase());
        if !typed
            .zip(expected)
            .is_some_and(|(typed, expected)| !typed.is_empty() && storage::same(&typed, &expected))
        {
            return lifecycle::invalid_action_body();
        }
    }
    if owner
        .approval("consume", "remove", &account, session, &fingerprint, token)
        .is_none_or(|receipt| receipt["consumed"] != true)
    {
        return Refusal::STALE.reply();
    }
    if !enabled() {
        return unavailable();
    }
    let native = native.clone();
    let id = id.to_owned();
    match tokio::spawn(async move {
        owner.remove_refusal(&profile).await?;
        if super::lifecycle_remove::running(&native, &account) {
            return Err(Refusal::new(
                409,
                "signin_running",
                "A sign-in for this account is running.",
            ));
        }
        let reply = owner.remove(&id, &fingerprint).await?;
        native.changed();
        Ok(reply)
    })
    .await
    {
        Ok(Ok(body)) => super::no_store(response(200, body)),
        Ok(Err(refusal)) => refusal.reply(),
        Err(_) => remove_failed().reply(),
    }
}

fn remove_failed() -> Refusal {
    Refusal::new(
        500,
        "remove_failed",
        "The account could not be removed safely. Nothing was changed.",
    )
}

impl Trash {
    async fn remove_refusal(&self, profile: &ProfileRemoval) -> Result<(), Refusal> {
        if profile.protected && profile.email().is_none() {
            return Err(Refusal::new(
                409,
                "account_protected",
                "This is this computer's default Claude profile. It can't be removed here.",
            ));
        }
        let mut unknown = false;
        for (host, launcher) in &profile.hosts {
            let state = if let Some(alias) = launcher["sshHost"].as_str() {
                self.call_host(
                    host,
                    alias,
                    json!({"host":host,"op":"state","launcher":launcher}),
                )
                .await
            } else {
                None
            };
            match state.as_ref().and_then(|state| state["running"].as_bool()) {
                Some(true) => {
                    return Err(Refusal::new(
                        409,
                        "app_running",
                        "Quit the app for this profile on that computer first.",
                    ));
                }
                Some(false) => {}
                None => unknown = true,
            }
        }
        if unknown {
            return Err(Refusal::new(
                409,
                "app_state_unknown",
                "Could not check whether the app is running. Try again later.",
            ));
        }
        Ok(())
    }

    async fn remove(&self, id: &str, reviewed: &str) -> Result<Value, Refusal> {
        let trash_writer = self.claim("claude-trash").map_err(|_| remove_failed())?;
        let pending_writer = self.claim("claude-pending").map_err(|_| remove_failed())?;
        let inventory_writer = self
            .claim("claude-desktop-profiles")
            .map_err(|_| remove_failed())?;
        let profile = removal_profile(&self.root, id)?.ok_or(Refusal::STALE)?;
        if profile.fingerprint() != reviewed {
            return Err(Refusal::STALE);
        }
        if profile.incomplete
            || profile.hosts.is_empty()
            || !crate::claude_account_store::pending_id(id)
        {
            return Err(Refusal::new(
                409,
                "not_removable",
                "This account cannot be removed from the dashboard.",
            ));
        }
        let entries = read_trash(&self.root).ok_or_else(remove_failed)?;
        if entries.len() >= 64 {
            return Err(remove_failed());
        }
        let mut profiles = profile_document(&self.root, profile.source)?;
        let rows = profiles["profiles"]
            .as_array_mut()
            .ok_or_else(remove_failed)?;
        rows.retain(|entry| entry["id"] != id);
        let writer = if profile.kind == "pending" {
            &pending_writer
        } else {
            &inventory_writer
        };
        let time = now();
        let stamp = second_stamp(time)?;
        let trash_name = format!("{id}-{}", stamp.replace(['-', ':'], ""));
        let purge_after = second_stamp(time + 30 * 86_400_000)?;
        let trash_id = format!(
            "tr_{}",
            storage::hex(&storage::random::<8>().map_err(|_| remove_failed())?)
        );
        let label = profile.entry["label"]
            .as_str()
            .or_else(|| profile.email())
            .unwrap_or(id);
        let moved = std::sync::Mutex::new(Map::new());
        claude_lifecycle::run(Workflow::Remove, |effect| {
            let profile = &profile;
            let (entries, moved, trash_id, stamp, purge_after, trash_name, trash_writer, profiles) =
                (&entries, &moved, &trash_id, &stamp, &purge_after, &trash_name, &trash_writer, &profiles);
            async move {
            if matches!(effect,Effect::Commit) {
                let mut entries = entries.clone();
                let moved = moved.lock().map_err(|_|remove_failed())?;
                entries.push(json!({"trashId":trash_id,"provider":"claude","accountId":format!("claude:{id}"),"label":label,
                    "source":profile.kind,"hosts":*moved,"entry":profile.entry,"trashedAt":stamp,
                    "purgeAfter":purge_after,"state":"trashed"}));
                save(trash_writer,&json!({"version":1,"entries":entries})).map_err(|_|remove_failed())?;
                if save(writer,profiles).is_err(){
                    entries.pop();
                    let _=save(trash_writer,&json!({"version":1,"entries":entries}));
                    return Err(remove_failed());
                }
                return Ok(true);
            }
            let host=host(effect);
            let Some(launcher)=profile.hosts.get(host)else{return Ok(false)};
            let alias=launcher["sshHost"].as_str().ok_or_else(remove_failed)?;
            let undo=matches!(effect,Effect::UndoMac|Effect::UndoWindows);
            let op=if undo{"restore"}else{"trash"};
            let receipt=self.call_host(host,alias,json!({
                "host":host,"op":op,"profileId":id,"trashName":trash_name,"launcher":launcher
            })).await.ok_or_else(||remove_failed().on(host))?;
            if undo {return Ok(receipt["ok"]==true);}
            if receipt["result"]=="cross_volume" {
                return Err(Refusal::new(409,"trash_cross_volume","The trash is on another disk on that computer. Nothing was moved.").on(host));
            }
            if receipt["result"]!="moved"{return Err(remove_failed().on(host));}
            moved.lock().map_err(|_|remove_failed())?.insert(host.to_owned(),json!({"trashName":trash_name,"launcher":launcher}));
            Ok(true)
            }
        }).await?;
        Ok(json!({"removed":true,"trashId":trash_id,"purgeAfter":purge_after}))
    }
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
            Arc::new(Agents::with_default(root.clone(), true)),
            Arc::new(LifecycleQueues::new(root)),
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
                    Action::Purge.wire(),
                    ID,
                    "session-a",
                    &fingerprint,
                    &token
                )
                .is_some()
        );
        for session in ["session-b", "session-a"] {
            let result = owner
                .approval(
                    "consume",
                    Action::Purge.wire(),
                    ID,
                    session,
                    &fingerprint,
                    &token,
                )
                .unwrap();
            assert_eq!(result["consumed"], false);
        }
        let denied = owner.purge(ID, &fingerprint, None).await.unwrap_err();
        assert_eq!(denied.code, "account_protected");
        assert_eq!(read_trash(&owner.root).unwrap(), vec![entry]);
    }
}
