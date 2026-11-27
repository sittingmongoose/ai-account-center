//! Central Logins step 2: deliver static logins (a Claude `setup-token` token, a provider API key)
//! to the computers the owner ticks, through each computer's agent (`login-*` verbs).
//!
//! - `GET /api/accounts/logins/delivery`: the fingerprint and expiry registry as a view
//!   (`aac_api::login_delivery`), the computers, the accounts and the static kinds. No agent call.
//! - `POST .../check`: ask every computer what it holds and record what was found.
//! - `POST .../put`: write one static login to the ticked computers (new, or a confirmed replace).
//! - `POST .../copy`: copy a delivered login from one computer to others, through this process.
//! - `POST .../remove`: remove it from the ticked computers.
//!
//! **Write-only.** A secret arrives in one request body, goes to the agents and is dropped; no
//! reply, registry row, log line or failure carries it. The registry (`logins-registry.json`)
//! keeps the id, kind, provider, account, fingerprint, end and holders only.
//!
//! **Every write** needs the browser session, the dashboard origin and JSON
//! (`lifecycle::mutation_body`), the secure dashboard address, and an explicit `confirm` that
//! names the action and the fingerprint the page showed. A rotating sign-in is refused by its
//! type, and any identity mismatch (another account or kind under the id, another computer
//! answering, a login that changed since it was confirmed) refuses that computer's write.
use super::NativeOwners;
use crate::{
    account_reads::public_account_id,
    agent_transport::{LoginReply, Selector},
    public_auth::{DashboardAccess, Method, Reply, Request, error, lifecycle, now, storage},
};
use aac_api::login_delivery::{
    DELIVERY_VIEW_VERSION, DeliveredLogin, DeliveryAccount, DeliveryComputer, DeliveryKind,
    DeliveryReceipt, DeliveryResult, DeliveryView, LoginHolder,
};
use aac_core::Cause;
use aac_files::credential::{self, Kind, Refusal};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{future::Future, pin::Pin};

pub(super) const PATH: &str = "/api/accounts/logins/delivery";
const REGISTRY_ROW: &str = "login-registry";
const DEADLINE_MS: u64 = 20_000;
const DAY_MS: i64 = 86_400_000;
/// A Claude `setup-token` token lasts one year.
const CLAUDE_TOKEN_DAYS: i64 = 365;
const EXPIRING_DAYS: i64 = 30;
const MAX_LOGINS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    View,
    Check,
    Put,
    Copy,
    Remove,
}

pub(super) fn route(method: &Method, path: &str) -> Option<Action> {
    let path = path.strip_suffix('/').unwrap_or(path);
    let tail = path.strip_prefix(PATH)?;
    match (method, tail) {
        (&Method::GET | &Method::HEAD | &Method::OPTIONS, "") => Some(Action::View),
        (&Method::POST | &Method::OPTIONS, "/check") => Some(Action::Check),
        (&Method::POST | &Method::OPTIONS, "/put") => Some(Action::Put),
        (&Method::POST | &Method::OPTIONS, "/copy") => Some(Action::Copy),
        (&Method::POST | &Method::OPTIONS, "/remove") => Some(Action::Remove),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// Refusals: fixed words, never an echo of the input.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refuse {
    InvalidBody,
    Secure,
    Rotating,
    NotStatic,
    UnknownAccount,
    UnknownLogin,
    UnknownComputer,
    Identity,
    Confirm,
    TooMany,
    Registry,
    Unavailable,
}

impl Refuse {
    pub(super) fn reply(self) -> Reply {
        use Refuse::*;
        match self {
            InvalidBody => lifecycle::invalid_action_body(),
            Secure => error(
                403,
                "secure_transport_required",
                "Use the secure dashboard address (HTTPS or a tunnel) for keys and sign-in codes.",
            ),
            Rotating => error(
                422,
                "rotating_signin_refused",
                "Sign-ins that renew themselves stay on their own computer. Only a Claude setup-token token or an API key can be delivered. Nothing was sent.",
            ),
            NotStatic => error(
                422,
                "not_a_static_login",
                "That is not a Claude setup-token token or an API key this provider takes. Nothing was sent.",
            ),
            UnknownAccount => error(404, "unknown_account", "That account was not found."),
            UnknownLogin => error(404, "unknown_login", "That login was not found."),
            UnknownComputer => error(
                400,
                "unknown_computer",
                "Pick computers from the list. Nothing was sent.",
            ),
            Identity => error(
                409,
                "identity_mismatch",
                "That login belongs to another account or kind. Nothing was changed.",
            ),
            Confirm => error(
                409,
                "confirmation_required",
                "Confirm this change on the page first: the login changed since the page read it.",
            ),
            TooMany => error(
                409,
                "too_many_logins",
                "The most delivered logins are already kept.",
            ),
            Registry => error(
                503,
                "registry_unavailable",
                "The login list could not be read or saved safely.",
            ),
            Unavailable => error(
                503,
                "delivery_unavailable",
                "Login delivery is not available on this server.",
            ),
        }
    }
}

fn refusal(why: Refusal) -> Refuse {
    match why {
        Refusal::Rotating => Refuse::Rotating,
        Refusal::Unknown => Refuse::NotStatic,
    }
}

// ---------------------------------------------------------------------------------------------
// The fingerprint and expiry registry. No secret field exists.

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Registry {
    pub(super) version: u32,
    pub(super) checked_at: Option<String>,
    pub(super) logins: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Entry {
    pub(super) login_id: String,
    pub(super) kind: Kind,
    pub(super) provider: String,
    pub(super) account: String,
    pub(super) fingerprint: String,
    pub(super) expires_at: Option<String>,
    pub(super) created_at: String,
    pub(super) holders: Vec<Holder>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Holder {
    pub(super) computer: String,
    pub(super) fingerprint: String,
    pub(super) delivered_at: String,
    /// `ok`, `unchecked`, `missing`, `changed` or `mismatch`.
    pub(super) state: String,
}

impl Registry {
    pub(super) fn parse(bytes: &[u8]) -> Option<Self> {
        let doc: Self = serde_json::from_slice(bytes).ok()?;
        let valid = doc.version == 1
            && doc.logins.len() <= MAX_LOGINS
            && doc.logins.iter().all(|e| {
                credential::valid_login_id(&e.login_id)
                    && credential::valid_provider(&e.provider)
                    && credential::valid_account(&e.account)
                    && credential::valid_fingerprint(&e.fingerprint)
                    && e.expires_at.as_deref().is_none_or(credential::valid_stamp)
                    && e.holders
                        .iter()
                        .all(|h| credential::valid_fingerprint(&h.fingerprint))
            });
        valid.then_some(doc)
    }

    fn entry(&self, id: &str) -> Option<&Entry> {
        self.logins.iter().find(|e| e.login_id == id)
    }

    fn entry_mut(&mut self, id: &str) -> Option<&mut Entry> {
        self.logins.iter_mut().find(|e| e.login_id == id)
    }
}

impl Entry {
    fn holder_mut(&mut self, computer: &str) -> Option<&mut Holder> {
        self.holders.iter_mut().find(|h| h.computer == computer)
    }

    fn record(&mut self, computer: &str, fingerprint: &str, at: &str) {
        let holder = Holder {
            computer: computer.to_owned(),
            fingerprint: fingerprint.to_owned(),
            delivered_at: at.to_owned(),
            state: "ok".into(),
        };
        match self.holder_mut(computer) {
            Some(existing) => *existing = holder,
            None => self.holders.push(holder),
        }
        self.holders.sort_by(|a, b| a.computer.cmp(&b.computer));
    }

    fn args(&self) -> Value {
        json!({"loginId":self.login_id,"kind":self.kind,"provider":self.provider,"account":self.account})
    }
}

// ---------------------------------------------------------------------------------------------
// Agent calls, behind a seam the tests drive with real agent homes.

/// One computer a login can be delivered to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Target {
    pub(super) id: String,
    pub(super) platform: String,
    pub(super) alias: Option<String>,
}

impl Target {
    /// The agent's `std::env::consts::OS` this computer must answer with.
    fn os(&self) -> &'static str {
        match self.platform.as_str() {
            "mac" => "macos",
            "windows" => "windows",
            _ => "linux",
        }
    }
}

pub(super) type Pending<'a> = Pin<Box<dyn Future<Output = LoginReply> + Send + 'a>>;

pub(super) trait Calls: Sync {
    fn call<'a>(&'a self, target: &'a Target, verb: &'static str, args: Value) -> Pending<'a>;
}

struct Live(std::sync::Arc<crate::agent_transport::Agents>);

impl Calls for Live {
    fn call<'a>(&'a self, target: &'a Target, verb: &'static str, args: Value) -> Pending<'a> {
        Box::pin(async move {
            let selector = Selector::usage(&target.platform, target.alias.as_deref());
            self.0.call_login(verb, &selector, args, DEADLINE_MS).await
        })
    }
}

/// What one computer holds, after the identity checks.
enum Held {
    Absent,
    Same(String),
    Mismatch,
    Unreachable,
}

/// Ask a computer for its inventory and find one login there. Another computer answering (its
/// operating system is not this one's) or another identity under the id is a mismatch.
async fn held(calls: &dyn Calls, target: &Target, entry: &Value) -> Held {
    match calls.call(target, "login-inventory", json!({})).await {
        LoginReply::Value(value) => {
            if value["os"].as_str() != Some(target.os()) {
                return Held::Mismatch;
            }
            let Some(logins) = value["logins"].as_array() else {
                return Held::Unreachable;
            };
            match logins.iter().find(|l| l["loginId"] == entry["loginId"]) {
                None => Held::Absent,
                Some(found)
                    if found["kind"] == entry["kind"]
                        && found["provider"] == entry["provider"]
                        && found["account"] == entry["account"] =>
                {
                    match found["fingerprint"].as_str() {
                        Some(fp) if credential::valid_fingerprint(fp) => Held::Same(fp.to_owned()),
                        _ => Held::Unreachable,
                    }
                }
                Some(_) => Held::Mismatch,
            }
        }
        _ => Held::Unreachable,
    }
}

fn result(computer: &str, state: &str) -> DeliveryResult {
    let text = match state {
        "delivered" => "Delivered",
        "unchanged" => "Already there",
        "removed" => "Removed",
        "absent" => "Was not there",
        "refused" => "Refused by the computer",
        "mismatch" => "Not sent: another account, kind or computer answered",
        "changed" => "Not sent: it changed there since you confirmed",
        _ => "Not reached; nothing is known to have changed",
    };
    DeliveryResult {
        computer: computer.to_owned(),
        state: state.to_owned(),
        text: text.to_owned(),
    }
}

fn failed(cause: Cause) -> &'static str {
    match cause {
        Cause::IdentityMismatch => "mismatch",
        Cause::ConfirmationInvalid => "changed",
        Cause::Refused => "refused",
        _ => "unreachable",
    }
}

/// Write one login to one computer. `confirmed` is the fingerprint the owner confirmed for this
/// login (none for a new one): a computer holding any other fingerprint is left alone.
async fn put_one(
    calls: &dyn Calls,
    target: &Target,
    entry: &Entry,
    secret: &str,
    confirmed: Option<&str>,
) -> &'static str {
    let fingerprint = credential::fingerprint(secret);
    let expect = match held(calls, target, &entry.args()).await {
        Held::Unreachable => return "unreachable",
        Held::Mismatch => return "mismatch",
        Held::Absent => "absent".to_owned(),
        Held::Same(fp) if fp == fingerprint || Some(fp.as_str()) == confirmed => fp,
        Held::Same(_) => return "changed",
    };
    let mut args = entry.args();
    args["secret"] = json!(secret);
    args["expiresAt"] = json!(entry.expires_at);
    args["expect"] = json!(expect);
    match calls.call(target, "login-put", args).await {
        LoginReply::Value(value)
            if value["ok"] == true
                && value["fingerprint"] == fingerprint
                && value["os"].as_str() == Some(target.os()) =>
        {
            if value["written"] == "unchanged" || expect == fingerprint {
                "unchanged"
            } else {
                "delivered"
            }
        }
        LoginReply::Value(_) => "mismatch",
        LoginReply::Failed(cause) => failed(cause),
        LoginReply::NoAnswer => "unreachable",
    }
}

// ---------------------------------------------------------------------------------------------
// Requests.

pub(super) struct Put {
    pub(super) login_id: Option<String>,
    pub(super) kind: Kind,
    pub(super) provider: String,
    pub(super) account: String,
    pub(super) secret: String,
    pub(super) expires_at: Option<String>,
    pub(super) computers: Vec<String>,
    pub(super) confirm: String,
}

fn only(body: &Map<String, Value>, allowed: &[&str]) -> Result<(), Refuse> {
    if body.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(Refuse::InvalidBody);
    }
    Ok(())
}

fn word<'a>(body: &'a Map<String, Value>, key: &str) -> Result<&'a str, Refuse> {
    body.get(key)
        .and_then(Value::as_str)
        .ok_or(Refuse::InvalidBody)
}

fn computers(
    body: &Map<String, Value>,
    key: &str,
    known: &[Target],
) -> Result<Vec<String>, Refuse> {
    let list = body
        .get(key)
        .and_then(Value::as_array)
        .ok_or(Refuse::InvalidBody)?;
    let mut out: Vec<String> = Vec::new();
    for item in list {
        let id = item.as_str().ok_or(Refuse::InvalidBody)?;
        if !known.iter().any(|t| t.id == id) {
            return Err(Refuse::UnknownComputer);
        }
        if out.iter().any(|c| c == id) {
            return Err(Refuse::InvalidBody);
        }
        out.push(id.to_owned());
    }
    if out.is_empty() {
        return Err(Refuse::InvalidBody);
    }
    Ok(out)
}

/// `YYYY-MM-DD` from the page's date field, as midnight UTC.
fn end_date(value: Option<&Value>) -> Result<Option<String>, Refuse> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(day)) => {
            let stamp = format!("{day}T00:00:00Z");
            (credential::valid_stamp(&stamp) && aac_num::ecma::parse_instant(&stamp).is_some())
                .then_some(Some(stamp))
                .ok_or(Refuse::InvalidBody)
        }
        _ => Err(Refuse::InvalidBody),
    }
}

/// The kind (refused by type when it rotates) and its provider rule.
fn static_kind(kind: &str, provider: &str) -> Result<Kind, Refuse> {
    let kind = Kind::admit(kind).map_err(refusal)?;
    let fits = match kind {
        Kind::ClaudeToken => provider == "claude",
        Kind::ApiKey => aac_providers::provider(provider).is_some_and(|p| p.key_store),
    };
    fits.then_some(kind).ok_or(Refuse::NotStatic)
}

pub(super) fn parse_put(
    body: &Map<String, Value>,
    accounts: &[DeliveryAccount],
    known: &[Target],
) -> Result<Put, Refuse> {
    only(
        body,
        &[
            "loginId",
            "kind",
            "provider",
            "account",
            "secret",
            "expiresOn",
            "computers",
            "confirm",
        ],
    )?;
    let provider = word(body, "provider")?;
    // The type is judged first: a rotating sign-in is refused whatever else the body holds.
    let kind = static_kind(word(body, "kind")?, provider)?;
    let secret = word(body, "secret")?;
    credential::admit_secret(kind, secret).map_err(refusal)?;
    let account = word(body, "account")?;
    if !public_account_id(&json!(account)) || !credential::valid_account(account) {
        return Err(Refuse::InvalidBody);
    }
    match accounts.iter().find(|a| a.id == account) {
        None => return Err(Refuse::UnknownAccount),
        Some(row) if row.provider != provider => return Err(Refuse::Identity),
        Some(_) => {}
    }
    let login_id = match body.get("loginId") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) if credential::valid_login_id(id) => Some(id.clone()),
        _ => return Err(Refuse::InvalidBody),
    };
    Ok(Put {
        login_id,
        kind,
        provider: provider.to_owned(),
        account: account.to_owned(),
        secret: secret.to_owned(),
        expires_at: end_date(body.get("expiresOn"))?,
        computers: computers(body, "computers", known)?,
        confirm: word(body, "confirm")?.to_owned(),
    })
}

fn targets<'a>(known: &'a [Target], ids: &[String]) -> Vec<&'a Target> {
    ids.iter()
        .filter_map(|id| known.iter().find(|t| &t.id == id))
        .collect()
}

fn new_id(registry: &Registry) -> Result<String, Refuse> {
    for _ in 0..8 {
        let id = storage::random::<4>()
            .map(|bytes| storage::hex(&bytes))
            .map_err(|_| Refuse::Registry)?;
        if registry.entry(&id).is_none() {
            return Ok(id);
        }
    }
    Err(Refuse::Registry)
}

fn stamp(at: i64) -> Result<String, Refuse> {
    storage::iso(at - at.rem_euclid(1000))
        .map(|s| s.replace(".000Z", "Z"))
        .ok_or(Refuse::Registry)
}

/// Deliver a new login (`confirm` = `deliver`) or replace one (`confirm` =
/// `replace:<the fingerprint the page showed>`).
pub(super) async fn deliver(
    calls: &dyn Calls,
    registry: &mut Registry,
    known: &[Target],
    put: Put,
    at: i64,
) -> Result<DeliveryReceipt, Refuse> {
    let now = stamp(at)?;
    let fingerprint = credential::fingerprint(&put.secret);
    let (mut entry, confirmed) = match &put.login_id {
        None => {
            if put.confirm != "deliver" {
                return Err(Refuse::Confirm);
            }
            if registry.logins.len() >= MAX_LOGINS {
                return Err(Refuse::TooMany);
            }
            let expires_at = put.expires_at.clone().or_else(|| {
                (put.kind == Kind::ClaudeToken)
                    .then(|| stamp(at + CLAUDE_TOKEN_DAYS * DAY_MS).ok())
                    .flatten()
            });
            let entry = Entry {
                login_id: new_id(registry)?,
                kind: put.kind,
                provider: put.provider.clone(),
                account: put.account.clone(),
                fingerprint: fingerprint.clone(),
                expires_at,
                created_at: now.clone(),
                holders: vec![],
            };
            (entry, None)
        }
        Some(id) => {
            let entry = registry.entry(id).ok_or(Refuse::UnknownLogin)?.clone();
            if entry.kind != put.kind
                || entry.provider != put.provider
                || entry.account != put.account
            {
                return Err(Refuse::Identity);
            }
            if put.confirm != format!("replace:{}", entry.fingerprint) {
                return Err(Refuse::Confirm);
            }
            let confirmed = entry.fingerprint.clone();
            let mut entry = entry;
            if put.expires_at.is_some() {
                entry.expires_at = put.expires_at.clone();
            }
            (entry, Some(confirmed))
        }
    };
    let mut results = Vec::new();
    let mut any = false;
    for target in targets(known, &put.computers) {
        let state = put_one(calls, target, &entry, &put.secret, confirmed.as_deref()).await;
        if matches!(state, "delivered" | "unchanged") {
            any = true;
            entry.record(&target.id, &fingerprint, &now);
        }
        results.push(result(&target.id, state));
    }
    if any {
        // Computers that still hold the replaced fingerprint now differ from the registry.
        for holder in &mut entry.holders {
            if holder.fingerprint != fingerprint {
                holder.state = "changed".into();
            }
        }
        entry.fingerprint = fingerprint.clone();
    }
    let login_id = entry.login_id.clone();
    let recorded = entry.fingerprint.clone();
    match registry.entry_mut(&login_id) {
        Some(existing) => *existing = entry,
        None if any => registry.logins.push(entry),
        None => {}
    }
    Ok(DeliveryReceipt {
        login_id,
        fingerprint: if any || confirmed.is_some() {
            recorded
        } else {
            String::new()
        },
        results,
    })
}

/// Copy a delivered login from one computer to others (`confirm` = `copy:<fingerprint>`). The
/// secret passes through this process only, from one agent's answer to the others' requests.
pub(super) async fn copy(
    calls: &dyn Calls,
    registry: &mut Registry,
    known: &[Target],
    body: &Map<String, Value>,
    at: i64,
) -> Result<DeliveryReceipt, Refuse> {
    only(body, &["loginId", "from", "to", "confirm"])?;
    let id = word(body, "loginId")?;
    let entry = registry.entry(id).ok_or(Refuse::UnknownLogin)?.clone();
    if word(body, "confirm")? != format!("copy:{}", entry.fingerprint) {
        return Err(Refuse::Confirm);
    }
    // Judged by type again: a registry edited by hand into another kind is refused here.
    static_kind(entry.kind.word(), &entry.provider)?;
    let from = word(body, "from")?;
    let source = known
        .iter()
        .find(|t| t.id == from)
        .ok_or(Refuse::UnknownComputer)?;
    let to = computers(body, "to", known)?;
    if to.iter().any(|c| c == from) {
        return Err(Refuse::InvalidBody);
    }
    let mut args = entry.args();
    args["fingerprint"] = json!(entry.fingerprint);
    let secret = match calls.call(source, "login-copy", args).await {
        LoginReply::Value(mut value)
            if value["ok"] == true && value["os"].as_str() == Some(source.os()) =>
        {
            match value["secret"].take() {
                Value::String(secret)
                    if credential::fingerprint(&secret) == entry.fingerprint
                        && credential::admit_secret(entry.kind, &secret).is_ok() =>
                {
                    secret
                }
                _ => return Ok(single(&entry, from, "mismatch")),
            }
        }
        LoginReply::Value(_) => return Ok(single(&entry, from, "mismatch")),
        LoginReply::Failed(cause) => return Ok(single(&entry, from, failed(cause))),
        LoginReply::NoAnswer => return Ok(single(&entry, from, "unreachable")),
    };
    let put = Put {
        login_id: Some(entry.login_id.clone()),
        kind: entry.kind,
        provider: entry.provider.clone(),
        account: entry.account.clone(),
        secret,
        expires_at: None,
        computers: to,
        confirm: format!("replace:{}", entry.fingerprint),
    };
    deliver(calls, registry, known, put, at).await
}

fn single(entry: &Entry, computer: &str, state: &str) -> DeliveryReceipt {
    DeliveryReceipt {
        login_id: entry.login_id.clone(),
        fingerprint: entry.fingerprint.clone(),
        results: vec![result(computer, state)],
    }
}

/// Remove a login from the ticked computers (`confirm` = `remove:<fingerprint>`). A computer
/// holding another fingerprint or identity is left alone. The registry forgets the login once
/// no computer holds it.
pub(super) async fn remove(
    calls: &dyn Calls,
    registry: &mut Registry,
    known: &[Target],
    body: &Map<String, Value>,
) -> Result<DeliveryReceipt, Refuse> {
    only(body, &["loginId", "computers", "confirm"])?;
    let id = word(body, "loginId")?;
    let mut entry = registry.entry(id).ok_or(Refuse::UnknownLogin)?.clone();
    if word(body, "confirm")? != format!("remove:{}", entry.fingerprint) {
        return Err(Refuse::Confirm);
    }
    let ids = computers(body, "computers", known)?;
    let mut results = Vec::new();
    for target in targets(known, &ids) {
        let state = match held(calls, target, &entry.args()).await {
            Held::Unreachable => "unreachable",
            Held::Mismatch => "mismatch",
            Held::Absent => "absent",
            Held::Same(fp) if fp != entry.fingerprint => "changed",
            Held::Same(fp) => {
                let mut args = entry.args();
                args["expect"] = json!(fp);
                match calls.call(target, "login-remove", args).await {
                    LoginReply::Value(value)
                        if value["ok"] == true && value["os"].as_str() == Some(target.os()) =>
                    {
                        if value["removed"] == true {
                            "removed"
                        } else {
                            "absent"
                        }
                    }
                    LoginReply::Value(_) => "mismatch",
                    LoginReply::Failed(cause) => failed(cause),
                    LoginReply::NoAnswer => "unreachable",
                }
            }
        };
        if matches!(state, "removed" | "absent") {
            entry.holders.retain(|h| h.computer != target.id);
        }
        results.push(result(&target.id, state));
    }
    let fingerprint = if entry.holders.is_empty() {
        registry.logins.retain(|e| e.login_id != entry.login_id);
        String::new()
    } else {
        let fp = entry.fingerprint.clone();
        if let Some(existing) = registry.entry_mut(id) {
            *existing = entry.clone();
        }
        fp
    };
    Ok(DeliveryReceipt {
        login_id: entry.login_id,
        fingerprint,
        results,
    })
}

/// Ask every computer what it holds and record it. The files on the computers are the truth: a
/// login found there that the registry lost is added back (same id, kind, provider and account
/// on every computer that holds it), and every holder's state says what was found.
pub(super) async fn check(calls: &dyn Calls, registry: &mut Registry, known: &[Target], at: i64) {
    for target in known {
        let answer = calls.call(target, "login-inventory", json!({})).await;
        let found = match answer {
            LoginReply::Value(value) if value["os"].as_str() == Some(target.os()) => {
                let Ok(found) =
                    serde_json::from_value::<Vec<credential::Summary>>(value["logins"].clone())
                else {
                    mark(registry, &target.id, "unchecked");
                    continue;
                };
                found
            }
            LoginReply::Value(_) => {
                mark(registry, &target.id, "mismatch");
                continue;
            }
            _ => {
                mark(registry, &target.id, "unchecked");
                continue;
            }
        };
        for entry in &mut registry.logins {
            let state = match found.iter().find(|s| s.login_id == entry.login_id) {
                None => "missing",
                Some(s)
                    if s.kind != entry.kind
                        || s.provider != entry.provider
                        || s.account != entry.account =>
                {
                    "mismatch"
                }
                Some(s) if s.fingerprint != entry.fingerprint => "changed",
                Some(_) => "ok",
            };
            match entry.holder_mut(&target.id) {
                Some(holder) => holder.state = state.into(),
                None if state == "ok" => {
                    let delivered = found
                        .iter()
                        .find(|s| s.login_id == entry.login_id)
                        .map(|s| s.written_at.clone())
                        .unwrap_or_default();
                    let fingerprint = entry.fingerprint.clone();
                    entry.record(&target.id, &fingerprint, &delivered);
                }
                None => {}
            }
        }
        for summary in found {
            if registry.entry(&summary.login_id).is_none()
                && registry.logins.len() < MAX_LOGINS
                && static_kind(summary.kind.word(), &summary.provider).is_ok()
            {
                registry.logins.push(Entry {
                    login_id: summary.login_id.clone(),
                    kind: summary.kind,
                    provider: summary.provider.clone(),
                    account: summary.account.clone(),
                    fingerprint: summary.fingerprint.clone(),
                    expires_at: summary.expires_at.clone(),
                    created_at: summary.written_at.clone(),
                    holders: vec![Holder {
                        computer: target.id.clone(),
                        fingerprint: summary.fingerprint.clone(),
                        delivered_at: summary.written_at.clone(),
                        state: "ok".into(),
                    }],
                });
            }
        }
    }
    registry.checked_at = stamp(at).ok();
}

fn mark(registry: &mut Registry, computer: &str, state: &str) {
    for entry in &mut registry.logins {
        if let Some(holder) = entry.holder_mut(computer) {
            holder.state = state.into();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The view.

fn ends(expires_at: Option<&str>, at: i64) -> (String, String) {
    let Some(ms) = expires_at.and_then(aac_num::ecma::parse_instant) else {
        return ("No set end".into(), "ok".into());
    };
    let days = (ms - at).div_euclid(DAY_MS);
    if ms <= at {
        ("Ended".into(), "ended".into())
    } else if days <= EXPIRING_DAYS {
        (
            format!("in {} day{}", days.max(1), if days == 1 { "" } else { "s" }),
            "expiring".into(),
        )
    } else {
        (
            expires_at
                .unwrap_or_default()
                .get(..10)
                .unwrap_or_default()
                .to_owned(),
            "ok".into(),
        )
    }
}

fn computer_label(id: &str) -> String {
    aac_status::computers::registry()
        .get(id)
        .or_else(|| aac_status::computers::computer(id))
        .and_then(|row| row["label"].as_str())
        .map_or_else(|| id.to_owned(), str::to_owned)
}

/// The accounts a static login may belong to: Claude's (tokens) and those of providers whose
/// keys AAC stores, as the Accounts page names them. Hidden accounts are left out.
pub(super) fn accounts(rows: &[Value]) -> Vec<DeliveryAccount> {
    let mut out: Vec<DeliveryAccount> = rows
        .iter()
        .filter(|row| row.get("hidden") != Some(&Value::Bool(true)))
        .filter_map(|row| {
            let id = row["id"].as_str()?;
            let provider = row["provider"].as_str()?;
            let admitted = provider == "claude"
                || aac_providers::provider(provider).is_some_and(|p| p.key_store);
            if !admitted || !public_account_id(&row["id"]) || !credential::valid_account(id) {
                return None;
            }
            let label = row["email"]
                .as_str()
                .or_else(|| row["label"].as_str())
                .filter(|s| (1..=120).contains(&s.len()) && !s.contains(['\n', '\r']))
                .unwrap_or(id);
            Some(DeliveryAccount {
                id: id.to_owned(),
                provider: provider.to_owned(),
                label: label.to_owned(),
            })
        })
        .collect();
    out.sort_by(|a, b| (&a.provider, &a.label).cmp(&(&b.provider, &b.label)));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

pub(super) fn view(
    registry: &Registry,
    known: &[Target],
    accounts: Vec<DeliveryAccount>,
    at: i64,
) -> DeliveryView {
    let mut key_providers: Vec<String> = ["kimi-code", "zai", "opencode-go", "qwen", "cursor"]
        .into_iter()
        .filter(|p| aac_providers::provider(p).is_some_and(|row| row.key_store))
        .map(str::to_owned)
        .collect();
    key_providers.sort();
    let label = |id: &str| {
        accounts.iter().find(|a| a.id == id).map_or_else(
            || "Not on the Accounts page".to_owned(),
            |a| a.label.clone(),
        )
    };
    let logins = registry
        .logins
        .iter()
        .map(|e| {
            let (ends_text, health) = ends(e.expires_at.as_deref(), at);
            DeliveredLogin {
                login_id: e.login_id.clone(),
                kind: e.kind.word().into(),
                provider: e.provider.clone(),
                account: e.account.clone(),
                account_label: label(&e.account),
                fingerprint: e.fingerprint.clone(),
                expires_at: e.expires_at.clone(),
                ends_text,
                health,
                holders: e
                    .holders
                    .iter()
                    .map(|h| LoginHolder {
                        computer: h.computer.clone(),
                        delivered_at: h.delivered_at.clone(),
                        state: if h.fingerprint != e.fingerprint && h.state == "ok" {
                            "changed".into()
                        } else {
                            h.state.clone()
                        },
                    })
                    .collect(),
            }
        })
        .collect();
    DeliveryView {
        version: DELIVERY_VIEW_VERSION,
        generated_at: storage::iso(at).unwrap_or_default(),
        checked_at: registry.checked_at.clone(),
        computers: known
            .iter()
            .map(|t| DeliveryComputer {
                id: t.id.clone(),
                label: computer_label(&t.id),
            })
            .collect(),
        accounts,
        kinds: vec![
            DeliveryKind {
                kind: "claude-token".into(),
                label: "Claude setup-token token".into(),
                providers: vec!["claude".into()],
            },
            DeliveryKind {
                kind: "api-key".into(),
                label: "API key".into(),
                providers: key_providers,
            },
        ],
        logins,
    }
}

// ---------------------------------------------------------------------------------------------
// The HTTP handler.

fn read_registry(native: &NativeOwners) -> Result<(aac_files::OwnedStore, Registry), Refuse> {
    let rows = aac_files::registry::rows().map_err(|_| Refuse::Registry)?;
    let store = native
        .owners
        .claim(rows, REGISTRY_ROW, native.agents.root(), None)
        .map_err(|_| Refuse::Unavailable)?;
    let registry = match store.read() {
        Ok(bytes) => Registry::parse(&bytes).ok_or(Refuse::Registry)?,
        // A missing file is an empty registry (the first delivery makes it).
        Err(e) if e.cause == Cause::NotInstalled => Registry {
            version: 1,
            ..Registry::default()
        },
        Err(_) => return Err(Refuse::Registry),
    };
    Ok((store, registry))
}

/// The registry without its writer lease, for the view: a read during a write sees the last saved
/// document. Absent is empty; a file that is a link, loose, foreign or damaged is refused.
fn load_registry(root: &std::path::Path) -> Result<Registry, Refuse> {
    use aac_files::read::{self, Mode, Owner, Read, ReadPolicy};
    let row = aac_files::registry::row(REGISTRY_ROW).map_err(|_| Refuse::Registry)?;
    let policy = ReadPolicy::capped(row.max_bytes)
        .mode(Mode::Private)
        .owner(Owner::Mine);
    match read::read(&root.join(&row.path), &policy) {
        Read::Absent => Ok(Registry {
            version: 1,
            ..Registry::default()
        }),
        Read::Bytes(bytes, _) => Registry::parse(&bytes).ok_or(Refuse::Registry),
        _ => Err(Refuse::Registry),
    }
}

fn save_registry(store: &aac_files::OwnedStore, registry: &Registry) -> Result<(), Refuse> {
    let mut bytes = serde_json::to_vec(registry).map_err(|_| Refuse::Registry)?;
    bytes.push(b'\n');
    store.write(&bytes).map_err(|_| Refuse::Registry)
}

async fn known_targets(native: &NativeOwners) -> Vec<Target> {
    native
        .agents
        .usage_hosts()
        .await
        .into_iter()
        .map(|host| Target {
            id: host.id,
            platform: host.platform.to_owned(),
            alias: host.alias,
        })
        .collect()
}

async fn account_rows(native: &NativeOwners) -> Vec<DeliveryAccount> {
    let Some(usage) = &native.usage else {
        return vec![];
    };
    let cache = usage.cache.clone();
    tokio::task::spawn_blocking(move || {
        let at = now();
        let rows = storage::iso(at)
            .and_then(|stamp| cache.merged_accounts(&stamp, at).ok())
            .unwrap_or_default();
        accounts(&rows)
    })
    .await
    .unwrap_or_default()
}

fn reply<T: Serialize>(status: u16, value: &T) -> Reply {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    super::no_store(crate::public_auth::json_bytes(status, bytes))
}

pub(super) async fn handle(native: &NativeOwners, action: Action, request: Request) -> Reply {
    if request.method() == Method::OPTIONS {
        return match action {
            Action::View => lifecycle::options("GET, HEAD", "9"),
            _ => lifecycle::options("POST", "4"),
        };
    }
    if action == Action::View {
        if let Err(reply) = lifecycle::admit(&request) {
            return reply;
        }
        if request.uri().query().is_some() {
            return lifecycle::unexpected_query();
        }
        let known = known_targets(native).await;
        let accounts = account_rows(native).await;
        let root = native.agents.root().to_owned();
        let registry = tokio::task::spawn_blocking(move || load_registry(&root))
            .await
            .unwrap_or(Err(Refuse::Registry));
        return match registry {
            Ok(registry) => reply(200, &view(&registry, &known, accounts, now())),
            Err(refuse) => refuse.reply(),
        };
    }
    let secure = request
        .extensions()
        .get::<DashboardAccess>()
        .is_some_and(|access| access.secure_transport);
    let body = match lifecycle::mutation_body(request).await {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    if !secure {
        return Refuse::Secure.reply();
    }
    let known = known_targets(native).await;
    let accounts = account_rows(native).await;
    // Parse before queueing: a refused body never waits, and the secret is judged first.
    let put = if action == Action::Put {
        match parse_put(&body, &accounts, &known) {
            Ok(put) => Some(put),
            Err(refuse) => return refuse.reply(),
        }
    } else {
        None
    };
    if action == Action::Check && !body.is_empty() {
        return Refuse::InvalidBody.reply();
    }
    let native = native.clone();
    let answer = tokio::spawn(async move {
        native
            .queues
            .run("logins", async {
                let calls = Live(native.agents.clone());
                let native2 = native.clone();
                let (store, mut registry) =
                    tokio::task::spawn_blocking(move || read_registry(&native2))
                        .await
                        .map_err(|_| Refuse::Registry)??;
                let at = now();
                let receipt = match action {
                    Action::Put => match put {
                        Some(put) => Some(deliver(&calls, &mut registry, &known, put, at).await?),
                        None => return Err(Refuse::InvalidBody),
                    },
                    Action::Copy => Some(copy(&calls, &mut registry, &known, &body, at).await?),
                    Action::Remove => Some(remove(&calls, &mut registry, &known, &body).await?),
                    Action::Check => {
                        check(&calls, &mut registry, &known, at).await;
                        None
                    }
                    Action::View => None,
                };
                let saved = registry.clone();
                tokio::task::spawn_blocking(move || save_registry(&store, &saved))
                    .await
                    .map_err(|_| Refuse::Registry)??;
                native.changed();
                Ok(match receipt {
                    Some(receipt) => serde_json::to_value(receipt).unwrap_or(Value::Null),
                    None => serde_json::to_value(view(&registry, &known, accounts, at))
                        .unwrap_or(Value::Null),
                })
            })
            .await
            .map_err(|_| Refuse::Registry)?
    })
    .await;
    match answer {
        Ok(Ok(value)) => reply(200, &value),
        Ok(Err(refuse)) => refuse.reply(),
        Err(_) => Refuse::Registry.reply(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::{collections::BTreeMap, path::PathBuf};

    const TOKEN: &str = "sk-ant-oat01-TESTONLY-delivery-token-00000000000";
    const OTHER: &str = "sk-ant-oat01-TESTONLY-delivery-token-11111111111";
    const AT: i64 = 1_795_000_000_000;

    /// Real agent homes, one per computer, driven through the agent's own verbs. `os` overrides
    /// what a computer says it runs (another computer answering).
    struct Fake {
        homes: BTreeMap<String, tempfile::TempDir>,
        os: BTreeMap<String, String>,
        down: Vec<String>,
        sent: std::sync::Mutex<Vec<String>>,
    }

    impl Fake {
        fn new(ids: &[&str]) -> Self {
            Self {
                homes: ids
                    .iter()
                    .map(|id| (id.to_string(), tempfile::tempdir().unwrap()))
                    .collect(),
                os: BTreeMap::new(),
                down: vec![],
                sent: Default::default(),
            }
        }
        fn home(&self, id: &str) -> PathBuf {
            self.homes[id].path().to_path_buf()
        }
    }

    impl Calls for Fake {
        fn call<'a>(
            &'a self,
            target: &'a Target,
            verb: &'static str,
            mut args: Value,
        ) -> Pending<'a> {
            Box::pin(async move {
                self.sent.lock().unwrap().push(args.to_string());
                if self.down.contains(&target.id) {
                    return LoginReply::NoAnswer;
                }
                args["home"] = json!(self.home(&target.id).to_str().unwrap());
                if verb == "login-put" {
                    args["nowMs"] = json!(AT);
                }
                match aac_agent::login_verb(verb, &args) {
                    Ok(mut value) => {
                        if let Some(os) = self.os.get(&target.id) {
                            value["os"] = json!(os);
                        }
                        LoginReply::Value(value)
                    }
                    Err(failure) => LoginReply::Failed(failure.cause),
                }
            })
        }
    }

    fn known() -> Vec<Target> {
        vec![
            Target {
                id: "ubuntu".into(),
                platform: "ubuntu".into(),
                alias: None,
            },
            Target {
                id: "nas1".into(),
                platform: "ubuntu".into(),
                alias: Some("nas1".into()),
            },
        ]
    }

    fn accounts() -> Vec<DeliveryAccount> {
        super::accounts(&[
            json!({"id":"claude:me","provider":"claude","email":"me@example.com"}),
            json!({"id":"zai:1","provider":"zai","label":"Z"}),
            json!({"id":"codex:1","provider":"codex","email":"c@example.com"}),
        ])
    }

    fn body(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    fn put_body(secret: &str, computers: &[&str]) -> Map<String, Value> {
        body(
            json!({"kind":"claude-token","provider":"claude","account":"claude:me",
            "secret":secret,"expiresOn":null,"computers":computers,"confirm":"deliver"}),
        )
    }

    fn registry() -> Registry {
        Registry {
            version: 1,
            ..Default::default()
        }
    }

    #[test]
    fn the_routes_are_exact() {
        assert_eq!(route(&Method::GET, PATH), Some(Action::View));
        assert_eq!(route(&Method::POST, PATH), None);
        assert_eq!(
            route(&Method::POST, "/api/accounts/logins/delivery/put"),
            Some(Action::Put)
        );
        assert_eq!(
            route(&Method::GET, "/api/accounts/logins/delivery/put"),
            None
        );
        assert_eq!(
            route(&Method::POST, "/api/accounts/logins/delivery/copy"),
            Some(Action::Copy)
        );
        assert_eq!(
            route(&Method::POST, "/api/accounts/logins/delivery/remove"),
            Some(Action::Remove)
        );
        assert_eq!(
            route(&Method::POST, "/api/accounts/logins/delivery/check"),
            Some(Action::Check)
        );
        assert_eq!(
            route(&Method::POST, "/api/accounts/logins/delivery/x"),
            None
        );
    }

    /// Red/green: a rotating sign-in is refused by its type before anything is sent; so is a
    /// rotating shape pasted as a static kind, and a kind on the wrong provider.
    #[test]
    fn rotating_sign_ins_are_refused_by_type() {
        let known = known();
        for kind in credential::ROTATING_KINDS {
            let mut b = put_body(TOKEN, &["ubuntu"]);
            b.insert("kind".into(), json!(kind));
            assert_eq!(
                parse_put(&b, &accounts(), &known).err(),
                Some(Refuse::Rotating),
                "{kind}"
            );
        }
        for secret in [
            "sk-ant-ort01-TESTONLY-refresh-000",
            r#"{"refresh_token":"x"}"#,
        ] {
            let b = put_body(secret, &["ubuntu"]);
            assert_eq!(
                parse_put(&b, &accounts(), &known).err(),
                Some(Refuse::Rotating)
            );
        }
        let mut b = put_body("zai.TESTONLY.key0000", &["ubuntu"]);
        b.insert("kind".into(), json!("api-key"));
        b.insert("provider".into(), json!("codex"));
        b.insert("account".into(), json!("codex:1"));
        assert_eq!(
            parse_put(&b, &accounts(), &known).err(),
            Some(Refuse::NotStatic)
        );
        // The account must be this provider's.
        let mut b = put_body(TOKEN, &["ubuntu"]);
        b.insert("account".into(), json!("zai:1"));
        assert_eq!(
            parse_put(&b, &accounts(), &known).err(),
            Some(Refuse::Identity)
        );
        // Unknown computers and extra keys are refused.
        assert_eq!(
            parse_put(&put_body(TOKEN, &["elsewhere"]), &accounts(), &known).err(),
            Some(Refuse::UnknownComputer)
        );
        let mut b = put_body(TOKEN, &["ubuntu"]);
        b.insert("home".into(), json!("/tmp"));
        assert_eq!(
            parse_put(&b, &accounts(), &known).err(),
            Some(Refuse::InvalidBody)
        );
    }

    /// Red/green: no reply, registry or view ever carries the secret; only fingerprints.
    #[tokio::test]
    async fn deliver_copy_check_remove_never_echo_the_secret() {
        let fake = Fake::new(&["ubuntu", "nas1"]);
        let known = known();
        let mut reg = registry();
        let put = parse_put(&put_body(TOKEN, &["ubuntu"]), &accounts(), &known).unwrap();
        let receipt = deliver(&fake, &mut reg, &known, put, AT).await.unwrap();
        assert_eq!(receipt.results[0].state, "delivered");
        assert_eq!(receipt.fingerprint, credential::fingerprint(TOKEN));
        let entry = reg.logins[0].clone();
        assert_eq!(entry.expires_at.as_deref(), Some("2027-11-18T11:06:40Z"));
        // Copy to nas1 through this process.
        let receipt = copy(
            &fake,
            &mut reg,
            &known,
            &body(
                json!({"loginId":entry.login_id,"from":"ubuntu","to":["nas1"],
                "confirm":format!("copy:{}",entry.fingerprint)}),
            ),
            AT,
        )
        .await
        .unwrap();
        assert_eq!(receipt.results[0].state, "delivered");
        assert_eq!(reg.logins[0].holders.len(), 2);
        check(&fake, &mut reg, &known, AT).await;
        let v = view(&reg, &known, accounts(), AT);
        assert!(v.logins[0].holders.iter().all(|h| h.state == "ok"));
        let receipt2 = remove(
            &fake,
            &mut reg,
            &known,
            &body(
                json!({"loginId":entry.login_id,"computers":["ubuntu","nas1"],
                "confirm":format!("remove:{}",entry.fingerprint)}),
            ),
        )
        .await
        .unwrap();
        assert!(receipt2.results.iter().all(|r| r.state == "removed"));
        assert!(reg.logins.is_empty());
        for text in [
            serde_json::to_string(&receipt).unwrap(),
            serde_json::to_string(&receipt2).unwrap(),
            serde_json::to_string(&v).unwrap(),
            serde_json::to_string(&entry).unwrap(),
        ] {
            assert!(!text.contains("TESTONLY"), "{text}");
            assert!(!text.contains("sk-ant"), "{text}");
        }
        // Every refusal's bytes are fixed words.
        for refuse in [
            Refuse::Rotating,
            Refuse::Identity,
            Refuse::Confirm,
            Refuse::NotStatic,
        ] {
            let bytes = axum::body::to_bytes(refuse.reply().into_body(), 4096)
                .await
                .unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains("TESTONLY"));
        }
    }

    #[tokio::test]
    async fn a_replace_needs_the_confirmed_fingerprint_and_the_same_identity() {
        let fake = Fake::new(&["ubuntu", "nas1"]);
        let known = known();
        let mut reg = registry();
        let put = parse_put(&put_body(TOKEN, &["ubuntu", "nas1"]), &accounts(), &known).unwrap();
        deliver(&fake, &mut reg, &known, put, AT).await.unwrap();
        let id = reg.logins[0].login_id.clone();
        let fp = reg.logins[0].fingerprint.clone();
        let mut b = put_body(OTHER, &["ubuntu"]);
        b.insert("loginId".into(), json!(id));
        // No confirmation of the shown fingerprint: refused.
        let put = parse_put(&b, &accounts(), &known).unwrap();
        assert_eq!(
            deliver(&fake, &mut reg, &known, put, AT).await.err(),
            Some(Refuse::Confirm)
        );
        b.insert("confirm".into(), json!(format!("replace:{fp}")));
        let put = parse_put(&b, &accounts(), &known).unwrap();
        let receipt = deliver(&fake, &mut reg, &known, put, AT).await.unwrap();
        assert_eq!(receipt.results[0].state, "delivered");
        // nas1 still holds the old token: the registry says so.
        let v = view(&reg, &known, accounts(), AT);
        let nas1 = v.logins[0]
            .holders
            .iter()
            .find(|h| h.computer == "nas1")
            .unwrap();
        assert_eq!(nas1.state, "changed");
        // Another account under the same id: identity mismatch.
        let mut b2 = b.clone();
        b2.insert("kind".into(), json!("api-key"));
        b2.insert("provider".into(), json!("zai"));
        b2.insert("account".into(), json!("zai:1"));
        b2.insert("secret".into(), json!("zai.TESTONLY.key0000"));
        let put = parse_put(&b2, &accounts(), &known).unwrap();
        assert_eq!(
            deliver(&fake, &mut reg, &known, put, AT).await.err(),
            Some(Refuse::Identity)
        );
    }

    #[tokio::test]
    async fn another_computer_answering_or_a_changed_login_is_left_alone() {
        let mut fake = Fake::new(&["ubuntu", "nas1"]);
        fake.os.insert("nas1".into(), "macos".into());
        fake.down.push("ubuntu".into());
        let known = known();
        let mut reg = registry();
        let put = parse_put(&put_body(TOKEN, &["ubuntu", "nas1"]), &accounts(), &known).unwrap();
        let receipt = deliver(&fake, &mut reg, &known, put, AT).await.unwrap();
        let states: Vec<_> = receipt.results.iter().map(|r| r.state.as_str()).collect();
        assert_eq!(states, ["unreachable", "mismatch"]);
        assert!(reg.logins.is_empty(), "nothing delivered, nothing recorded");
        assert!(!fake.home("nas1").join(".ccs/logins").exists());
        // A login that changed on a computer since it was confirmed is not overwritten.
        let fake = Fake::new(&["ubuntu", "nas1"]);
        let put = parse_put(&put_body(TOKEN, &["ubuntu"]), &accounts(), &known).unwrap();
        deliver(&fake, &mut reg, &known, put, AT).await.unwrap();
        let id = reg.logins[0].login_id.clone();
        let entry = reg.logins[0].clone();
        // Someone else writes a different token under the same id on ubuntu.
        let mut args = entry.args();
        args["secret"] = json!(OTHER);
        args["expiresAt"] = Value::Null;
        args["expect"] = json!(entry.fingerprint);
        args["home"] = json!(fake.home("ubuntu").to_str().unwrap());
        aac_agent::login_verb("login-put", &args).unwrap();
        let third = "sk-ant-oat01-TESTONLY-delivery-token-22222222222";
        let mut b = put_body(third, &["ubuntu"]);
        b.insert("loginId".into(), json!(id));
        b.insert(
            "confirm".into(),
            json!(format!("replace:{}", entry.fingerprint)),
        );
        let put = parse_put(&b, &accounts(), &known).unwrap();
        let receipt = deliver(&fake, &mut reg, &known, put, AT).await.unwrap();
        assert_eq!(receipt.results[0].state, "changed");
    }

    #[tokio::test]
    async fn a_check_rebuilds_a_lost_registry_from_the_computers() {
        let fake = Fake::new(&["ubuntu", "nas1"]);
        let known = known();
        let mut reg = registry();
        let put = parse_put(&put_body(TOKEN, &["ubuntu", "nas1"]), &accounts(), &known).unwrap();
        deliver(&fake, &mut reg, &known, put, AT).await.unwrap();
        let before = reg.logins[0].clone();
        let mut lost = registry();
        check(&fake, &mut lost, &known, AT).await;
        assert_eq!(lost.logins.len(), 1);
        assert_eq!(lost.logins[0].fingerprint, before.fingerprint);
        assert_eq!(lost.logins[0].holders.len(), 2);
        assert!(lost.checked_at.is_some());
        // The registry document round-trips through its parser.
        let bytes = serde_json::to_vec(&lost).unwrap();
        assert_eq!(Registry::parse(&bytes), Some(lost));
    }

    #[test]
    fn ends_say_when_and_how_soon() {
        assert_eq!(ends(None, AT), ("No set end".into(), "ok".into()));
        let soon = storage::iso(AT + 10 * DAY_MS + 5).unwrap();
        assert_eq!(ends(Some(&soon), AT).1, "expiring");
        let past = storage::iso(AT - 1).unwrap();
        assert_eq!(ends(Some(&past), AT).1, "ended");
        let far = storage::iso(AT + 200 * DAY_MS).unwrap();
        assert_eq!(ends(Some(&far), AT).1, "ok");
    }
}
