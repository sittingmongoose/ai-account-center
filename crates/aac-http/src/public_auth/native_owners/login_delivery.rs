//! Central Logins step 2: deliver static logins (a Claude `setup-token` token, a provider API key)
//! to the computers the owner ticks, through each computer's agent (`login-*` verbs).
//!
//! - `GET /api/accounts/logins/delivery`: the fingerprint and expiry registry as a view
//!   (`aac_api::login_delivery`), the computers, the accounts and the static kinds. No agent call.
//! - `POST .../check`: ask every computer what it holds and record what was found.
//! - `POST .../put`: write one static login to the ticked computers (new, or a confirmed replace).
//! - `POST .../copy`: copy a delivered login from one computer to others, through this process.
//! - `POST .../remove`: remove it from the ticked computers.
//! - `POST .../trust`: pin the computer now answering for a target (rebuilt or renamed).
//!
//! **Computer identity.** Each agent keeps a random key (`login-computer.json`) and reports it
//! with its host name. "Check computers" pins every computer seen for the first time; every
//! later call compares the answer with the pin, and every write names the pinned key so the
//! agent itself refuses a request meant for another computer before touching a file.
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
use std::{future::Future, pin::Pin as Pin_};

pub(super) const PATH: &str = "/api/accounts/logins/delivery";
const REGISTRY_ROW: &str = "login-registry";
const DEADLINE_MS: u64 = 20_000;
const DAY_MS: i64 = 86_400_000;
const EXPIRING_DAYS: i64 = 30;
const MAX_LOGINS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    View,
    Check,
    Put,
    Copy,
    Remove,
    Trust,
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
        (&Method::POST | &Method::OPTIONS, "/trust") => Some(Action::Trust),
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
// The fingerprint and expiry registry. No secret field exists. It never claims more than a
// computer's own answer showed: each holder keeps the fingerprint last seen there, and a login
// whose computers disagree has no current generation until a delivery settles it.

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Registry {
    pub(super) version: u32,
    pub(super) checked_at: Option<String>,
    /// Each computer's pinned identity (enrolled when the owner checks the computers).
    #[serde(default)]
    pub(super) computers: Vec<Pin>,
    pub(super) logins: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Pin {
    pub(super) computer: String,
    pub(super) key: String,
    pub(super) host: String,
    pub(super) enrolled_at: String,
    /// Another computer that answered for this one at the last contact, if any.
    pub(super) seen: Option<credential::ComputerId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Entry {
    pub(super) login_id: String,
    pub(super) kind: Kind,
    pub(super) provider: String,
    pub(super) account: String,
    /// The current generation, when it is known (a delivery set it, or every holder agrees).
    pub(super) fingerprint: Option<String>,
    /// The current generation's end, as the owner gave it; unknown stays unknown.
    pub(super) expires_at: Option<String>,
    pub(super) created_at: String,
    pub(super) holders: Vec<Holder>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Holder {
    pub(super) computer: String,
    /// The fingerprint last seen there (none when the file could not be read).
    pub(super) fingerprint: Option<String>,
    pub(super) delivered_at: String,
    /// What the last contact found: `seen`, `unreadable`, `unchecked`, `other-account` or
    /// `wrong-computer`.
    pub(super) seen: String,
}

impl Registry {
    pub(super) fn parse(bytes: &[u8]) -> Option<Self> {
        let doc: Self = serde_json::from_slice(bytes).ok()?;
        let fp = |f: &Option<String>| f.as_deref().is_none_or(credential::valid_fingerprint);
        let valid = doc.version == 1
            && doc.logins.len() <= MAX_LOGINS
            && doc
                .computers
                .iter()
                .all(|p| credential::valid_computer_key(&p.key) && credential::valid_host(&p.host))
            && doc.logins.iter().all(|e| {
                credential::valid_login_id(&e.login_id)
                    && credential::valid_provider(&e.provider)
                    && credential::valid_account(&e.account)
                    && fp(&e.fingerprint)
                    && e.expires_at.as_deref().is_none_or(credential::valid_stamp)
                    && e.holders.iter().all(|h| fp(&h.fingerprint))
            });
        valid.then_some(doc)
    }

    fn entry(&self, id: &str) -> Option<&Entry> {
        self.logins.iter().find(|e| e.login_id == id)
    }

    fn pin(&self, computer: &str) -> Option<&Pin> {
        self.computers.iter().find(|p| p.computer == computer)
    }

    fn put_entry(&mut self, entry: Entry) {
        match self
            .logins
            .iter_mut()
            .find(|e| e.login_id == entry.login_id)
        {
            Some(existing) => *existing = entry,
            None => self.logins.push(entry),
        }
    }
}

impl Entry {
    fn holder(&self, computer: &str) -> Option<&Holder> {
        self.holders.iter().find(|h| h.computer == computer)
    }

    /// Record what a computer holds now (a delivery, or a check that read it).
    fn hold(&mut self, computer: &str, fingerprint: Option<&str>, at: &str, seen: &str) {
        let delivered_at = match self.holder(computer) {
            Some(h) if h.fingerprint.as_deref() == fingerprint && fingerprint.is_some() => {
                h.delivered_at.clone()
            }
            _ => at.to_owned(),
        };
        let holder = Holder {
            computer: computer.to_owned(),
            fingerprint: fingerprint.map(str::to_owned),
            delivered_at,
            seen: seen.to_owned(),
        };
        self.holders.retain(|h| h.computer != computer);
        self.holders.push(holder);
        self.holders.sort_by(|a, b| a.computer.cmp(&b.computer));
    }

    /// Forget a computer: only after it answered that the login is not there, or it was removed.
    fn drop_holder(&mut self, computer: &str) {
        self.holders.retain(|h| h.computer != computer);
    }

    /// Mark a computer's holder with what the last contact found, keeping what it last read.
    fn mark(&mut self, computer: &str, seen: &str) {
        if let Some(h) = self.holders.iter_mut().find(|h| h.computer == computer) {
            h.seen = seen.to_owned();
        }
    }

    fn args(&self) -> Value {
        json!({"loginId":self.login_id,"kind":self.kind,"provider":self.provider,"account":self.account})
    }

    /// The state a holder shows.
    fn state(&self, holder: &Holder) -> &'static str {
        match holder.seen.as_str() {
            "seen" if holder.fingerprint.is_some() && holder.fingerprint == self.fingerprint => {
                "ok"
            }
            "seen" => "differs",
            "unreadable" => "unreadable",
            "other-account" => "other-account",
            "wrong-computer" => "wrong-computer",
            _ => "unchecked",
        }
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

pub(super) type Pending<'a> = Pin_<Box<dyn Future<Output = LoginReply> + Send + 'a>>;

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

/// One computer's inventory, after its identity checks.
enum Inventory {
    /// It answered as the computer pinned for this target.
    Ours {
        logins: Vec<credential::Summary>,
        unreadable: Vec<String>,
        key: String,
    },
    /// It answered, but has never been enrolled.
    New(credential::ComputerId),
    /// Another computer answered (another key or host, or another operating system).
    Other(Option<credential::ComputerId>),
    /// No usable answer.
    Unreachable,
}

async fn inventory(calls: &dyn Calls, registry: &Registry, target: &Target) -> Inventory {
    let LoginReply::Value(value) = calls.call(target, "login-inventory", json!({})).await else {
        return Inventory::Unreachable;
    };
    let id = serde_json::from_value::<credential::ComputerId>(value["computer"].clone()).ok();
    if value["os"].as_str() != Some(target.os()) {
        return Inventory::Other(id);
    }
    let Some(id) =
        id.filter(|id| credential::valid_computer_key(&id.key) && credential::valid_host(&id.host))
    else {
        return Inventory::Unreachable;
    };
    match registry.pin(&target.id) {
        None => Inventory::New(id),
        Some(pin) if pin.key != id.key || pin.host != id.host => Inventory::Other(Some(id)),
        Some(pin) => {
            let logins =
                serde_json::from_value::<Vec<credential::Summary>>(value["logins"].clone());
            let unreadable = serde_json::from_value::<Vec<String>>(value["unreadable"].clone());
            match (logins, unreadable) {
                (Ok(logins), Ok(unreadable)) => Inventory::Ours {
                    logins,
                    unreadable,
                    key: pin.key.clone(),
                },
                _ => Inventory::Unreachable,
            }
        }
    }
}

/// What one pinned computer holds under one login id.
enum Held {
    Absent,
    Has(String),
    OtherAccount,
    Unreadable,
}

fn held(entry: &Entry, logins: &[credential::Summary], unreadable: &[String]) -> Held {
    if unreadable
        .iter()
        .any(|id| id == &entry.login_id || id == "?")
    {
        return Held::Unreadable;
    }
    match logins.iter().find(|s| s.login_id == entry.login_id) {
        None => Held::Absent,
        Some(s)
            if s.kind != entry.kind
                || s.provider != entry.provider
                || s.account != entry.account =>
        {
            Held::OtherAccount
        }
        Some(s) => Held::Has(s.fingerprint.clone()),
    }
}

fn result(computer: &str, state: &str) -> DeliveryResult {
    let text = match state {
        "delivered" => "Delivered",
        "unchanged" => "Already there",
        "removed" => "Removed",
        "absent" => "Was not there",
        "refused" => "Refused by the computer",
        "mismatch" => "Not sent: another account or kind is under this login there",
        "changed" => {
            "Not sent: it changed there since the page read it. Check computers, then confirm again"
        }
        "unreadable" => "Not changed: a file is there but cannot be read safely",
        "unenrolled" => "Not sent: check the computers first, so this one is known",
        "wrong-computer" => "Not sent: another computer answered for this one",
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

/// Probe one target and say what its holder must be recorded as when it cannot be written.
enum Probe {
    Ready { held: Held, key: String },
    Stop(&'static str),
}

async fn probe(
    calls: &dyn Calls,
    registry: &mut Registry,
    target: &Target,
    entry: &Entry,
) -> Probe {
    match inventory(calls, registry, target).await {
        Inventory::Ours {
            logins,
            unreadable,
            key,
        } => Probe::Ready {
            held: held(entry, &logins, &unreadable),
            key,
        },
        Inventory::New(_) => Probe::Stop("unenrolled"),
        Inventory::Other(seen) => {
            if let Some(pin) = registry
                .computers
                .iter_mut()
                .find(|p| p.computer == target.id)
            {
                pin.seen = seen;
            }
            Probe::Stop("wrong-computer")
        }
        Inventory::Unreachable => Probe::Stop("unreachable"),
    }
}

/// Write one login to one computer. `expect` is what the page showed for that computer
/// (`absent` or a fingerprint); the computer must hold exactly that, or already the new value.
async fn put_one(
    calls: &dyn Calls,
    registry: &mut Registry,
    target: &Target,
    entry: &Entry,
    secret: &str,
    expires_at: Option<&str>,
    expect: &str,
) -> &'static str {
    let fingerprint = credential::fingerprint(secret);
    let (held, key) = match probe(calls, registry, target, entry).await {
        Probe::Ready { held, key } => (held, key),
        Probe::Stop(state) => return state,
    };
    let on_disk = match held {
        Held::Unreadable => return "unreadable",
        Held::OtherAccount => return "mismatch",
        Held::Absent => "absent".to_owned(),
        Held::Has(fp) => fp,
    };
    if on_disk != expect && on_disk != fingerprint {
        return "changed";
    }
    let mut args = entry.args();
    args["secret"] = json!(secret);
    args["expiresAt"] = json!(expires_at);
    args["expect"] = json!(on_disk);
    args["computer"] = json!(key);
    match calls.call(target, "login-put", args).await {
        LoginReply::Value(value)
            if value["ok"] == true
                && value["fingerprint"] == fingerprint
                && value["os"].as_str() == Some(target.os()) =>
        {
            if value["written"] == "unchanged" {
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
// Requests. Each target carries what the page showed for it (`expect`): `absent`, or the
// fingerprint of the copy that computer holds. The registry must agree, so a page that read an
// older registry is refused; the agent compares again under its lease.

pub(super) struct Put {
    pub(super) login_id: Option<String>,
    pub(super) kind: Kind,
    pub(super) provider: String,
    pub(super) account: String,
    pub(super) secret: String,
    pub(super) expires_at: Option<String>,
    pub(super) targets: Vec<(String, String)>,
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

fn expect_word(value: &str) -> bool {
    value == "absent" || credential::valid_fingerprint(value)
}

/// `[{"computer":..,"expect":..}]`, each a known computer, once.
fn target_list(
    body: &Map<String, Value>,
    key: &str,
    known: &[Target],
) -> Result<Vec<(String, String)>, Refuse> {
    let list = body
        .get(key)
        .and_then(Value::as_array)
        .ok_or(Refuse::InvalidBody)?;
    let mut out: Vec<(String, String)> = Vec::new();
    for item in list {
        let item = item.as_object().ok_or(Refuse::InvalidBody)?;
        only(item, &["computer", "expect"])?;
        let id = word(item, "computer")?;
        let expect = word(item, "expect")?;
        if !expect_word(expect) {
            return Err(Refuse::InvalidBody);
        }
        if !known.iter().any(|t| t.id == id) {
            return Err(Refuse::UnknownComputer);
        }
        if out.iter().any(|(c, _)| c == id) {
            return Err(Refuse::InvalidBody);
        }
        out.push((id.to_owned(), expect.to_owned()));
    }
    if out.is_empty() {
        return Err(Refuse::InvalidBody);
    }
    Ok(out)
}

/// The page's view of a computer must be the registry's: the holder's recorded fingerprint, or
/// `absent` when the registry knows of no copy there.
fn agrees(entry: Option<&Entry>, computer: &str, expect: &str) -> bool {
    match entry.and_then(|e| e.holder(computer)) {
        None => expect == "absent",
        Some(h) => h.fingerprint.as_deref() == Some(expect),
    }
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
            "targets",
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
        targets: target_list(body, "targets", known)?,
        confirm: word(body, "confirm")?.to_owned(),
    })
}

fn find<'a>(known: &'a [Target], id: &str) -> Option<&'a Target> {
    known.iter().find(|t| t.id == id)
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

/// Write one value to the targets and record exactly what each computer answered. The entry's
/// current generation (fingerprint and end together) moves to the new value only when at least
/// one computer now holds it; otherwise nothing about the old value changes.
/// One value to write: its secret, its end, the targets with what the page showed on each, the
/// time, and whether the login is new.
struct Delivery<'a> {
    secret: &'a str,
    expires_at: Option<String>,
    targets: &'a [(String, String)],
    now: &'a str,
    new: bool,
}

async fn write_targets(
    calls: &dyn Calls,
    registry: &mut Registry,
    known: &[Target],
    mut entry: Entry,
    d: Delivery<'_>,
) -> DeliveryReceipt {
    let Delivery {
        secret,
        expires_at,
        targets,
        now,
        new,
    } = d;
    let fingerprint = credential::fingerprint(secret);
    let mut results = Vec::new();
    let mut landed = false;
    for (id, expect) in targets {
        let Some(target) = find(known, id) else {
            continue;
        };
        let state = put_one(
            calls,
            registry,
            target,
            &entry,
            secret,
            expires_at.as_deref(),
            expect,
        )
        .await;
        match state {
            "delivered" | "unchanged" => {
                landed = true;
                entry.hold(id, Some(&fingerprint), now, "seen");
            }
            "wrong-computer" => entry.mark(id, "wrong-computer"),
            "unreadable" => entry.hold(id, None, now, "unreadable"),
            "mismatch" if entry.holder(id).is_some() => entry.mark(id, "other-account"),
            // `changed`, `unreachable`, `refused`, `unenrolled`: the holder keeps what it last read.
            _ => {}
        }
        results.push(result(id, state));
    }
    if landed {
        entry.fingerprint = Some(fingerprint.clone());
        entry.expires_at = expires_at;
    }
    let login_id = entry.login_id.clone();
    let recorded = entry.fingerprint.clone().unwrap_or_default();
    if landed || !new {
        registry.put_entry(entry);
    }
    DeliveryReceipt {
        login_id,
        fingerprint: if landed || !new {
            recorded
        } else {
            String::new()
        },
        results,
    }
}

/// Deliver a new login (`confirm` = `deliver`, every target `absent`) or replace one (`confirm`
/// = `replace`, each target naming what the page showed for it).
pub(super) async fn deliver(
    calls: &dyn Calls,
    registry: &mut Registry,
    known: &[Target],
    put: Put,
    at: i64,
) -> Result<DeliveryReceipt, Refuse> {
    let now = stamp(at)?;
    let (entry, new) = match &put.login_id {
        None => {
            if put.confirm != "deliver" || put.targets.iter().any(|(_, e)| e != "absent") {
                return Err(Refuse::Confirm);
            }
            if registry.logins.len() >= MAX_LOGINS {
                return Err(Refuse::TooMany);
            }
            let entry = Entry {
                login_id: new_id(registry)?,
                kind: put.kind,
                provider: put.provider.clone(),
                account: put.account.clone(),
                fingerprint: None,
                expires_at: None,
                created_at: now.clone(),
                holders: vec![],
            };
            (entry, true)
        }
        Some(id) => {
            let entry = registry.entry(id).ok_or(Refuse::UnknownLogin)?.clone();
            if entry.kind != put.kind
                || entry.provider != put.provider
                || entry.account != put.account
            {
                return Err(Refuse::Identity);
            }
            if put.confirm != "replace"
                || put.targets.iter().any(|(c, e)| !agrees(Some(&entry), c, e))
            {
                return Err(Refuse::Confirm);
            }
            (entry, false)
        }
    };
    Ok(write_targets(
        calls,
        registry,
        known,
        entry,
        Delivery {
            secret: &put.secret,
            expires_at: put.expires_at,
            targets: &put.targets,
            now: &now,
            new,
        },
    )
    .await)
}

/// Copy a login from one computer to others (`confirm` = `copy`; `from` names the computer and
/// the fingerprint the page showed for it; each destination names what it showed there). The
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
    if word(body, "confirm")? != "copy" {
        return Err(Refuse::Confirm);
    }
    // Judged by type again: a registry edited by hand into another kind is refused here.
    static_kind(entry.kind.word(), &entry.provider)?;
    let from = body
        .get("from")
        .and_then(Value::as_object)
        .ok_or(Refuse::InvalidBody)?;
    only(from, &["computer", "fingerprint"])?;
    let source_id = word(from, "computer")?;
    let source_fp = word(from, "fingerprint")?;
    let source = find(known, source_id).ok_or(Refuse::UnknownComputer)?;
    let to = target_list(body, "to", known)?;
    if to.iter().any(|(c, _)| c == source_id)
        || !credential::valid_fingerprint(source_fp)
        || !agrees(Some(&entry), source_id, source_fp)
        || to.iter().any(|(c, e)| !agrees(Some(&entry), c, e))
    {
        return Err(Refuse::Confirm);
    }
    let one = |state: &str| DeliveryReceipt {
        login_id: entry.login_id.clone(),
        fingerprint: entry.fingerprint.clone().unwrap_or_default(),
        results: vec![result(source_id, state)],
    };
    let key = match probe(calls, registry, source, &entry).await {
        Probe::Ready {
            held: Held::Has(fp),
            key,
        } if fp == source_fp => key,
        Probe::Ready {
            held: Held::Unreadable,
            ..
        } => return Ok(one("unreadable")),
        Probe::Ready {
            held: Held::OtherAccount,
            ..
        } => return Ok(one("mismatch")),
        Probe::Ready { .. } => return Ok(one("changed")),
        Probe::Stop(state) => return Ok(one(state)),
    };
    let mut args = entry.args();
    args["fingerprint"] = json!(source_fp);
    args["computer"] = json!(key);
    let (secret, expires_at) = match calls.call(source, "login-copy", args).await {
        LoginReply::Value(mut value)
            if value["ok"] == true && value["os"].as_str() == Some(source.os()) =>
        {
            let expires_at = match &value["expiresAt"] {
                Value::String(at) if credential::valid_stamp(at) => Some(at.clone()),
                _ => None,
            };
            match value["secret"].take() {
                Value::String(secret)
                    if credential::fingerprint(&secret) == source_fp
                        && credential::admit_secret(entry.kind, &secret).is_ok() =>
                {
                    (secret, expires_at)
                }
                _ => return Ok(one("mismatch")),
            }
        }
        LoginReply::Value(_) => return Ok(one("mismatch")),
        LoginReply::Failed(cause) => return Ok(one(failed(cause))),
        LoginReply::NoAnswer => return Ok(one("unreachable")),
    };
    let now = stamp(at)?;
    let delivery = Delivery {
        secret: &secret,
        expires_at,
        targets: &to,
        now: &now,
        new: false,
    };
    Ok(write_targets(calls, registry, known, entry, delivery).await)
}

/// Remove a login from the targets (`confirm` = `remove`, each target naming the fingerprint the
/// page showed for it). A computer holding anything else, or a file it cannot read, is left
/// alone and stays listed. The registry forgets a computer only when it answered that the login
/// is gone, and forgets the login only when no computer is left.
pub(super) async fn remove(
    calls: &dyn Calls,
    registry: &mut Registry,
    known: &[Target],
    body: &Map<String, Value>,
) -> Result<DeliveryReceipt, Refuse> {
    only(body, &["loginId", "targets", "confirm"])?;
    let id = word(body, "loginId")?;
    let mut entry = registry.entry(id).ok_or(Refuse::UnknownLogin)?.clone();
    let targets = target_list(body, "targets", known)?;
    if word(body, "confirm")? != "remove"
        || targets
            .iter()
            .any(|(c, e)| e == "absent" || !agrees(Some(&entry), c, e))
    {
        return Err(Refuse::Confirm);
    }
    let mut results = Vec::new();
    for (computer, expect) in &targets {
        let Some(target) = find(known, computer) else {
            continue;
        };
        let state = match probe(calls, registry, target, &entry).await {
            Probe::Stop(state) => state,
            Probe::Ready { held, key } => match held {
                Held::Unreadable => "unreadable",
                Held::OtherAccount => "mismatch",
                Held::Absent => "absent",
                Held::Has(fp) if &fp != expect => "changed",
                Held::Has(fp) => {
                    let mut args = entry.args();
                    args["expect"] = json!(fp);
                    args["computer"] = json!(key);
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
            },
        };
        match state {
            "removed" | "absent" => entry.drop_holder(computer),
            "unreadable" => entry.mark(computer, "unreadable"),
            "wrong-computer" => entry.mark(computer, "wrong-computer"),
            "mismatch" => entry.mark(computer, "other-account"),
            _ => {}
        }
        results.push(result(computer, state));
    }
    let fingerprint = entry.fingerprint.clone().unwrap_or_default();
    let login_id = entry.login_id.clone();
    if entry.holders.is_empty() {
        registry.logins.retain(|e| e.login_id != login_id);
    } else {
        registry.put_entry(entry);
    }
    Ok(DeliveryReceipt {
        login_id,
        fingerprint,
        results,
    })
}

/// Ask every computer what it holds, enrol computers seen for the first time (the owner asked
/// for this check), and record exactly what each answered. The files on the computers are the
/// truth: a login the registry lost is added back with every computer that holds it, and when
/// the computers disagree on its value no generation is chosen for it.
pub(super) async fn check(calls: &dyn Calls, registry: &mut Registry, known: &[Target], at: i64) {
    let now = stamp(at).unwrap_or_default();
    let mut answers: Vec<(String, Vec<credential::Summary>, Vec<String>)> = Vec::new();
    for target in known {
        match inventory(calls, registry, target).await {
            Inventory::New(id) => {
                registry.computers.retain(|p| p.computer != target.id);
                registry.computers.push(Pin {
                    computer: target.id.clone(),
                    key: id.key,
                    host: id.host,
                    enrolled_at: now.clone(),
                    seen: None,
                });
                registry
                    .computers
                    .sort_by(|a, b| a.computer.cmp(&b.computer));
                // Enrolled now; read it again as the pinned computer.
                if let Inventory::Ours {
                    logins, unreadable, ..
                } = inventory(calls, registry, target).await
                {
                    answers.push((target.id.clone(), logins, unreadable));
                } else {
                    mark_all(registry, &target.id, "unchecked");
                }
            }
            Inventory::Ours {
                logins, unreadable, ..
            } => {
                if let Some(pin) = registry
                    .computers
                    .iter_mut()
                    .find(|p| p.computer == target.id)
                {
                    pin.seen = None;
                }
                answers.push((target.id.clone(), logins, unreadable));
            }
            Inventory::Other(seen) => {
                if let Some(pin) = registry
                    .computers
                    .iter_mut()
                    .find(|p| p.computer == target.id)
                {
                    pin.seen = seen;
                }
                mark_all(registry, &target.id, "wrong-computer");
            }
            Inventory::Unreachable => mark_all(registry, &target.id, "unchecked"),
        }
    }
    // Logins on the computers that the registry does not know: rebuild them from the files.
    for (_, logins, _) in &answers {
        for s in logins {
            if registry.entry(&s.login_id).is_none()
                && registry.logins.len() < MAX_LOGINS
                && static_kind(s.kind.word(), &s.provider).is_ok()
            {
                registry.logins.push(Entry {
                    login_id: s.login_id.clone(),
                    kind: s.kind,
                    provider: s.provider.clone(),
                    account: s.account.clone(),
                    fingerprint: None,
                    expires_at: None,
                    created_at: s.written_at.clone(),
                    holders: vec![],
                });
            }
        }
    }
    for entry in &mut registry.logins {
        let mut ends: Vec<(String, Option<String>)> = Vec::new();
        for (computer, logins, unreadable) in &answers {
            match held(entry, logins, unreadable) {
                Held::Absent => entry.drop_holder(computer),
                Held::Unreadable => entry.hold(computer, None, &now, "unreadable"),
                Held::OtherAccount => entry.hold(computer, None, &now, "other-account"),
                Held::Has(fp) => {
                    let written = logins
                        .iter()
                        .find(|s| s.login_id == entry.login_id)
                        .map(|s| (s.written_at.clone(), s.expires_at.clone()));
                    let (written_at, expires_at) = written.unwrap_or_default();
                    entry.hold(computer, Some(&fp), &written_at, "seen");
                    ends.push((fp, expires_at));
                }
            }
        }
        // With no current generation, adopt one only when every computer read agrees on it.
        if entry.fingerprint.is_none()
            && let Some((first, end)) = ends.first().cloned()
            && ends.iter().all(|(fp, e)| fp == &first && e == &end)
            && entry.holders.iter().all(|h| h.seen == "seen")
        {
            entry.fingerprint = Some(first);
            entry.expires_at = end;
        }
    }
    registry.logins.retain(|e| !e.holders.is_empty());
    registry.checked_at = Some(now);
}

fn mark_all(registry: &mut Registry, computer: &str, seen: &str) {
    for entry in &mut registry.logins {
        entry.mark(computer, seen);
    }
}

/// Trust the computer now answering for a target (it was rebuilt or renamed): `key` must be the
/// key the page showed for it, and that computer must still be the one answering.
pub(super) async fn trust(
    calls: &dyn Calls,
    registry: &mut Registry,
    known: &[Target],
    body: &Map<String, Value>,
    at: i64,
) -> Result<(), Refuse> {
    only(body, &["computer", "key", "confirm"])?;
    let target = find(known, word(body, "computer")?).ok_or(Refuse::UnknownComputer)?;
    let key = word(body, "key")?;
    if word(body, "confirm")? != "trust" || !credential::valid_computer_key(key) {
        return Err(Refuse::Confirm);
    }
    let shown = registry
        .pin(&target.id)
        .and_then(|p| p.seen.clone())
        .filter(|seen| seen.key == key)
        .ok_or(Refuse::Confirm)?;
    match inventory(calls, registry, target).await {
        Inventory::Other(Some(now_seen)) if now_seen == shown => {}
        _ => return Err(Refuse::Confirm),
    }
    registry.computers.retain(|p| p.computer != target.id);
    registry.computers.push(Pin {
        computer: target.id.clone(),
        key: shown.key,
        host: shown.host,
        enrolled_at: stamp(at)?,
        seen: None,
    });
    registry
        .computers
        .sort_by(|a, b| a.computer.cmp(&b.computer));
    // Whatever the old computer held is unknown for this one until the next check reads it.
    mark_all(registry, &target.id, "unchecked");
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The view.

fn ends(kind: Kind, expires_at: Option<&str>, at: i64) -> (String, String) {
    let Some(ms) = expires_at.and_then(aac_num::ecma::parse_instant) else {
        // A token's end is known only when the owner gave it; never guessed from the delivery.
        return match kind {
            Kind::ClaudeToken => ("End not known".into(), "unknown".into()),
            Kind::ApiKey => ("No set end".into(), "ok".into()),
        };
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
            let (ends_text, health) = ends(e.kind, e.expires_at.as_deref(), at);
            DeliveredLogin {
                login_id: e.login_id.clone(),
                kind: e.kind.word().into(),
                provider: e.provider.clone(),
                account: e.account.clone(),
                account_label: label(&e.account),
                fingerprint: e.fingerprint.clone().unwrap_or_default(),
                expires_at: e.expires_at.clone(),
                ends_text,
                health,
                holders: e
                    .holders
                    .iter()
                    .map(|h| LoginHolder {
                        computer: h.computer.clone(),
                        delivered_at: h.delivered_at.clone(),
                        fingerprint: h.fingerprint.clone().unwrap_or_default(),
                        state: e.state(h).into(),
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
            .map(|t| {
                let pin = registry.pin(&t.id);
                let seen = pin.and_then(|p| p.seen.clone());
                DeliveryComputer {
                    id: t.id.clone(),
                    label: computer_label(&t.id),
                    state: match (pin, &seen) {
                        (None, _) => "new",
                        (Some(_), Some(_)) => "other",
                        (Some(_), None) => "enrolled",
                    }
                    .into(),
                    host: seen
                        .as_ref()
                        .map(|s| s.host.clone())
                        .or_else(|| pin.map(|p| p.host.clone()))
                        .unwrap_or_default(),
                    seen_key: seen.map(|s| s.key).unwrap_or_default(),
                }
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
                    Action::Trust => {
                        trust(&calls, &mut registry, &known, &body, at).await?;
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
    const THIRD: &str = "sk-ant-oat01-TESTONLY-delivery-token-22222222222";
    const AT: i64 = 1_795_000_000_000;

    /// Real agent homes, one per computer, driven through the agent's own verbs. `os` overrides
    /// what a computer says it runs; `routes` sends a target's calls to another computer's home
    /// (a swapped alias); `down` makes a computer unreachable.
    struct Fake {
        homes: BTreeMap<String, tempfile::TempDir>,
        os: BTreeMap<String, String>,
        routes: BTreeMap<String, String>,
        down: Vec<String>,
    }

    impl Fake {
        fn new(ids: &[&str]) -> Self {
            Self {
                homes: ids
                    .iter()
                    .map(|id| (id.to_string(), tempfile::tempdir().unwrap()))
                    .collect(),
                os: BTreeMap::new(),
                routes: BTreeMap::new(),
                down: vec![],
            }
        }
        fn home(&self, id: &str) -> PathBuf {
            self.homes[id].path().to_path_buf()
        }
        fn file(&self, id: &str, login: &str) -> PathBuf {
            self.home(id)
                .join(format!(".ccs/logins/claude-{login}.json"))
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
                if self.down.contains(&target.id) {
                    return LoginReply::NoAnswer;
                }
                let home = self.routes.get(&target.id).unwrap_or(&target.id);
                args["home"] = json!(self.home(home).to_str().unwrap());
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

    fn targets(list: &[(&str, &str)]) -> Value {
        Value::Array(
            list.iter()
                .map(|(c, e)| json!({"computer": c, "expect": e}))
                .collect(),
        )
    }

    /// A new delivery: every target `absent`.
    fn put_body(secret: &str, computers: &[&str]) -> Map<String, Value> {
        let list: Vec<(&str, &str)> = computers.iter().map(|c| (*c, "absent")).collect();
        body(
            json!({"kind":"claude-token","provider":"claude","account":"claude:me",
            "secret":secret,"expiresOn":null,"targets":targets(&list),"confirm":"deliver"}),
        )
    }

    /// A replace of `id`, each target naming what the page showed for it.
    fn replace_body(id: &str, secret: &str, list: &[(&str, &str)]) -> Map<String, Value> {
        body(
            json!({"loginId":id,"kind":"claude-token","provider":"claude","account":"claude:me",
            "secret":secret,"expiresOn":null,"targets":targets(list),"confirm":"replace"}),
        )
    }

    fn registry() -> Registry {
        Registry {
            version: 1,
            ..Default::default()
        }
    }

    /// A registry after the owner checked the computers once (both enrolled).
    async fn enrolled(fake: &Fake, known: &[Target]) -> Registry {
        let mut reg = registry();
        check(fake, &mut reg, known, AT).await;
        assert_eq!(reg.computers.len(), known.len());
        reg
    }

    async fn first(fake: &Fake, reg: &mut Registry, known: &[Target], on: &[&str]) -> Entry {
        let put = parse_put(&put_body(TOKEN, on), &accounts(), known).unwrap();
        let receipt = deliver(fake, reg, known, put, AT).await.unwrap();
        assert!(receipt.results.iter().all(|r| r.state == "delivered"));
        reg.logins[0].clone()
    }

    fn fp(secret: &str) -> String {
        credential::fingerprint(secret)
    }

    fn states(receipt: &DeliveryReceipt) -> Vec<&str> {
        receipt.results.iter().map(|r| r.state.as_str()).collect()
    }

    #[test]
    fn the_routes_are_exact() {
        let at = |m: &Method, tail: &str| route(m, &format!("{PATH}{tail}"));
        assert_eq!(at(&Method::GET, ""), Some(Action::View));
        assert_eq!(at(&Method::POST, ""), None);
        assert_eq!(at(&Method::POST, "/put"), Some(Action::Put));
        assert_eq!(at(&Method::GET, "/put"), None);
        assert_eq!(at(&Method::POST, "/copy"), Some(Action::Copy));
        assert_eq!(at(&Method::POST, "/remove"), Some(Action::Remove));
        assert_eq!(at(&Method::POST, "/check"), Some(Action::Check));
        assert_eq!(at(&Method::POST, "/trust"), Some(Action::Trust));
        assert_eq!(at(&Method::POST, "/x"), None);
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
        let mut b = put_body(TOKEN, &["ubuntu"]);
        b.insert("account".into(), json!("zai:1"));
        assert_eq!(
            parse_put(&b, &accounts(), &known).err(),
            Some(Refuse::Identity)
        );
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
        let mut reg = enrolled(&fake, &known).await;
        let entry = first(&fake, &mut reg, &known, &["ubuntu"]).await;
        let receipt = copy(
            &fake,
            &mut reg,
            &known,
            &body(json!({"loginId":entry.login_id,
                "from":{"computer":"ubuntu","fingerprint":fp(TOKEN)},
                "to":targets(&[("nas1","absent")]),"confirm":"copy"})),
            AT,
        )
        .await
        .unwrap();
        assert_eq!(states(&receipt), ["delivered"]);
        check(&fake, &mut reg, &known, AT).await;
        let v = view(&reg, &known, accounts(), AT);
        assert!(v.logins[0].holders.iter().all(|h| h.state == "ok"));
        let receipt2 = remove(
            &fake,
            &mut reg,
            &known,
            &body(json!({"loginId":entry.login_id,
                "targets":targets(&[("ubuntu",&fp(TOKEN)),("nas1",&fp(TOKEN))]),"confirm":"remove"})),
        )
        .await
        .unwrap();
        assert_eq!(states(&receipt2), ["removed", "removed"]);
        assert!(reg.logins.is_empty());
        for text in [
            serde_json::to_string(&receipt).unwrap(),
            serde_json::to_string(&receipt2).unwrap(),
            serde_json::to_string(&v).unwrap(),
            serde_json::to_string(&entry).unwrap(),
            serde_json::to_string(&reg).unwrap(),
        ] {
            assert!(!text.contains("TESTONLY"), "{text}");
            assert!(!text.contains("sk-ant"), "{text}");
        }
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

    /// Red/green (review M1, both): a computer that was never checked receives nothing, and once
    /// enrolled, another computer of the same operating system answering for it is refused
    /// before any file is written there; the intended computer is not recorded as delivered.
    #[tokio::test]
    async fn another_computer_answering_is_refused_before_writing() {
        let mut fake = Fake::new(&["ubuntu", "nas1", "other"]);
        let known = known();
        let mut reg = registry();
        let put = parse_put(&put_body(TOKEN, &["nas1"]), &accounts(), &known).unwrap();
        let receipt = deliver(&fake, &mut reg, &known, put, AT).await.unwrap();
        assert_eq!(states(&receipt), ["unenrolled"]);
        assert!(reg.logins.is_empty());
        let mut reg = enrolled(&fake, &known).await;
        fake.routes.insert("nas1".into(), "other".into());
        let put = parse_put(&put_body(TOKEN, &["nas1"]), &accounts(), &known).unwrap();
        let receipt = deliver(&fake, &mut reg, &known, put, AT).await.unwrap();
        assert_eq!(states(&receipt), ["wrong-computer"]);
        assert!(!fake.home("other").join(".ccs/logins").exists());
        assert!(reg.logins.is_empty());
        let v = view(&reg, &known, accounts(), AT);
        let nas1 = v.computers.iter().find(|c| c.id == "nas1").unwrap();
        assert_eq!(nas1.state, "other");
        // The agent refuses a write naming another computer's key even when asked directly.
        let entry_args = json!({"loginId":"0a1b2c3d","kind":"claude-token","provider":"claude",
            "account":"claude:me","secret":TOKEN,"expiresAt":null,"expect":"absent",
            "computer": reg.pin("nas1").unwrap().key, "home": fake.home("other").to_str().unwrap()});
        assert_eq!(
            aac_agent::login_verb("login-put", &entry_args)
                .unwrap_err()
                .cause,
            Cause::IdentityMismatch
        );
        // Trusting it needs the key the page showed, and that computer must still answer.
        let shown = nas1.seen_key.clone();
        assert_eq!(
            trust(
                &fake,
                &mut reg,
                &known,
                &body(json!({"computer":"nas1","key":"0".repeat(32),"confirm":"trust"})),
                AT
            )
            .await,
            Err(Refuse::Confirm)
        );
        trust(
            &fake,
            &mut reg,
            &known,
            &body(json!({"computer":"nas1","key":shown,"confirm":"trust"})),
            AT,
        )
        .await
        .unwrap();
        let put = parse_put(&put_body(TOKEN, &["nas1"]), &accounts(), &known).unwrap();
        let receipt = deliver(&fake, &mut reg, &known, put, AT).await.unwrap();
        assert_eq!(states(&receipt), ["delivered"]);
    }

    /// Red/green (review Sol M2, Astra M1): a replace that reached only some computers can be
    /// finished by a copy and the stale copy removed, each confirming what that computer holds.
    #[tokio::test]
    async fn a_partial_replace_can_be_finished_or_removed() {
        let mut fake = Fake::new(&["ubuntu", "nas1"]);
        let known = known();
        let mut reg = enrolled(&fake, &known).await;
        let entry = first(&fake, &mut reg, &known, &["ubuntu", "nas1"]).await;
        let id = entry.login_id.clone();
        fake.down.push("nas1".into());
        let put = parse_put(
            &replace_body(&id, OTHER, &[("ubuntu", &fp(TOKEN)), ("nas1", &fp(TOKEN))]),
            &accounts(),
            &known,
        )
        .unwrap();
        let partial = deliver(&fake, &mut reg, &known, put, AT + 1000)
            .await
            .unwrap();
        assert_eq!(states(&partial), ["delivered", "unreachable"]);
        fake.down.clear();
        // The page shows nas1 still holding the old value, by its fingerprint.
        let v = view(&reg, &known, accounts(), AT);
        let nas1 = v.logins[0]
            .holders
            .iter()
            .find(|h| h.computer == "nas1")
            .unwrap();
        assert_eq!(
            (nas1.state.as_str(), nas1.fingerprint.as_str()),
            ("differs", fp(TOKEN).as_str())
        );
        // Confirming the new value for nas1 is refused: the page must name what is there.
        assert_eq!(
            copy(
                &fake,
                &mut reg,
                &known,
                &body(
                    json!({"loginId":id,"from":{"computer":"ubuntu","fingerprint":fp(OTHER)},
                    "to":targets(&[("nas1",&fp(OTHER))]),"confirm":"copy"})
                ),
                AT
            )
            .await
            .err(),
            Some(Refuse::Confirm)
        );
        // Removing the stale copy, confirming its own fingerprint, works (Sol's reproduction).
        let removed = remove(
            &fake,
            &mut reg,
            &known,
            &body(
                json!({"loginId":id,"targets":targets(&[("nas1",&fp(TOKEN))]),"confirm":"remove"}),
            ),
        )
        .await
        .unwrap();
        assert_eq!(states(&removed), ["removed"]);
        assert!(!fake.file("nas1", &id).exists());
        // And finishing the replace by a copy works (Astra's reproduction), from a fresh absent.
        let copied = copy(
            &fake,
            &mut reg,
            &known,
            &body(
                json!({"loginId":id,"from":{"computer":"ubuntu","fingerprint":fp(OTHER)},
                "to":targets(&[("nas1","absent")]),"confirm":"copy"}),
            ),
            AT + 3000,
        )
        .await
        .unwrap();
        assert_eq!(states(&copied), ["delivered"]);
        let v = view(&reg, &known, accounts(), AT);
        assert!(v.logins[0].holders.iter().all(|h| h.state == "ok"));
        // A copy straight over a stale holder, naming its old value, also works.
        let put = parse_put(
            &replace_body(&id, THIRD, &[("ubuntu", &fp(OTHER))]),
            &accounts(),
            &known,
        )
        .unwrap();
        deliver(&fake, &mut reg, &known, put, AT).await.unwrap();
        let copied = copy(
            &fake,
            &mut reg,
            &known,
            &body(
                json!({"loginId":id,"from":{"computer":"ubuntu","fingerprint":fp(THIRD)},
                "to":targets(&[("nas1",&fp(OTHER))]),"confirm":"copy"}),
            ),
            AT,
        )
        .await
        .unwrap();
        assert_eq!(states(&copied), ["delivered"]);
    }

    /// Red/green (review Astra M1): the agent wrote, then the dashboard stopped before saving its
    /// registry. A check records what each computer really holds and the copy can follow it.
    #[tokio::test]
    async fn a_check_recovers_a_write_the_registry_never_saved() {
        let fake = Fake::new(&["ubuntu", "nas1"]);
        let known = known();
        let mut reg = enrolled(&fake, &known).await;
        let entry = first(&fake, &mut reg, &known, &["ubuntu"]).await;
        let mut lost = reg.clone();
        let put = parse_put(
            &replace_body(&entry.login_id, OTHER, &[("ubuntu", &fp(TOKEN))]),
            &accounts(),
            &known,
        )
        .unwrap();
        deliver(&fake, &mut lost, &known, put, AT + 1000)
            .await
            .unwrap();
        drop(lost);
        check(&fake, &mut reg, &known, AT + 2000).await;
        let holder = reg.logins[0].holder("ubuntu").unwrap().clone();
        assert_eq!(holder.fingerprint.as_deref(), Some(fp(OTHER).as_str()));
        assert_eq!(reg.logins[0].state(&holder), "differs");
        let copied = copy(
            &fake,
            &mut reg,
            &known,
            &body(json!({"loginId":entry.login_id,"from":{"computer":"ubuntu","fingerprint":fp(OTHER)},
                "to":targets(&[("nas1","absent")]),"confirm":"copy"})),
            AT + 3000,
        )
        .await
        .unwrap();
        assert_eq!(states(&copied), ["delivered"]);
        assert_eq!(
            reg.logins[0].fingerprint.as_deref(),
            Some(fp(OTHER).as_str())
        );
    }

    /// Red/green (review Sol M3): a rebuilt registry lists every computer that holds the login,
    /// each with its own value, and picks no current generation when they disagree.
    #[tokio::test]
    async fn a_rebuild_lists_every_holder_and_chooses_nothing() {
        let fake = Fake::new(&["ubuntu", "nas1"]);
        let known = known();
        let mut reg = enrolled(&fake, &known).await;
        let entry = first(&fake, &mut reg, &known, &["ubuntu", "nas1"]).await;
        let put = parse_put(
            &replace_body(&entry.login_id, OTHER, &[("nas1", &fp(TOKEN))]),
            &accounts(),
            &known,
        )
        .unwrap();
        deliver(&fake, &mut reg, &known, put, AT + 1000)
            .await
            .unwrap();
        let mut lost = registry();
        check(&fake, &mut lost, &known, AT + 2000).await;
        assert_eq!(lost.logins.len(), 1);
        assert_eq!(lost.logins[0].fingerprint, None);
        let v = view(&lost, &known, accounts(), AT + 2000);
        let found: Vec<_> = v.logins[0]
            .holders
            .iter()
            .map(|h| (h.computer.as_str(), h.fingerprint.clone(), h.state.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                ("nas1", fp(OTHER), "differs"),
                ("ubuntu", fp(TOKEN), "differs")
            ]
        );
        assert_eq!(v.logins[0].fingerprint, "");
        // When every computer agrees, the rebuild adopts that value; registry round-trips.
        let fake = Fake::new(&["ubuntu", "nas1"]);
        let mut reg = enrolled(&fake, &known).await;
        let before = first(&fake, &mut reg, &known, &["ubuntu", "nas1"]).await;
        let mut lost = registry();
        check(&fake, &mut lost, &known, AT).await;
        assert_eq!(lost.logins[0].fingerprint, before.fingerprint);
        let bytes = serde_json::to_vec(&lost).unwrap();
        assert_eq!(Registry::parse(&bytes), Some(lost));
    }

    /// Red/green (review Sol M4): no end is invented; the view says it is not known.
    #[tokio::test]
    async fn a_pasted_token_does_not_get_an_invented_expiry() {
        let fake = Fake::new(&["ubuntu", "nas1"]);
        let known = known();
        let mut reg = enrolled(&fake, &known).await;
        let entry = first(&fake, &mut reg, &known, &["ubuntu"]).await;
        assert_eq!(entry.expires_at, None);
        let v = view(&reg, &known, accounts(), AT);
        assert_eq!(
            (v.logins[0].ends_text.as_str(), v.logins[0].health.as_str()),
            ("End not known", "unknown")
        );
        let on_disk = aac_agent::login_verb(
            "login-inventory",
            &json!({"home": fake.home("ubuntu").to_str().unwrap()}),
        )
        .unwrap();
        assert_eq!(on_disk["logins"][0]["expiresAt"], Value::Null);
    }

    /// Red/green (review Astra M2): a file that cannot be read safely is never taken for absent:
    /// removal leaves it and its registry entry, and a check marks it unreadable.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unreadable_file_is_not_forgotten() {
        use std::os::unix::fs::PermissionsExt;
        let fake = Fake::new(&["ubuntu", "nas1"]);
        let known = known();
        let mut reg = enrolled(&fake, &known).await;
        let entry = first(&fake, &mut reg, &known, &["ubuntu"]).await;
        let path = fake.file("ubuntu", &entry.login_id);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let receipt = remove(
            &fake,
            &mut reg,
            &known,
            &body(
                json!({"loginId":entry.login_id,"targets":targets(&[("ubuntu",&fp(TOKEN))]),
                "confirm":"remove"}),
            ),
        )
        .await
        .unwrap();
        assert_eq!(states(&receipt), ["unreadable"]);
        assert!(path.exists());
        assert_eq!(reg.logins.len(), 1);
        check(&fake, &mut reg, &known, AT).await;
        let v = view(&reg, &known, accounts(), AT);
        assert_eq!(v.logins[0].holders[0].state, "unreadable");
    }

    /// Red/green (review Astra M4): a replacement that reached no computer changes nothing about
    /// the old value, its end included.
    #[tokio::test]
    async fn a_failed_replace_keeps_the_old_value_and_its_end() {
        let mut fake = Fake::new(&["ubuntu", "nas1"]);
        let known = known();
        let mut reg = enrolled(&fake, &known).await;
        let mut b = put_body(TOKEN, &["ubuntu"]);
        b.insert("expiresOn".into(), json!("2026-11-19"));
        let put = parse_put(&b, &accounts(), &known).unwrap();
        deliver(&fake, &mut reg, &known, put, AT).await.unwrap();
        let old = reg.logins[0].clone();
        fake.down.push("ubuntu".into());
        let mut b = replace_body(&old.login_id, OTHER, &[("ubuntu", &fp(TOKEN))]);
        b.insert("expiresOn".into(), json!("2028-11-19"));
        let put = parse_put(&b, &accounts(), &known).unwrap();
        let receipt = deliver(&fake, &mut reg, &known, put, AT + 1000)
            .await
            .unwrap();
        assert_eq!(states(&receipt), ["unreachable"]);
        assert_eq!(reg.logins[0], old);
    }

    #[tokio::test]
    async fn a_replace_needs_the_shown_values_and_the_same_identity() {
        let fake = Fake::new(&["ubuntu", "nas1"]);
        let known = known();
        let mut reg = enrolled(&fake, &known).await;
        let entry = first(&fake, &mut reg, &known, &["ubuntu", "nas1"]).await;
        let id = entry.login_id.clone();
        // Naming a value the registry does not hold for that computer: refused.
        let put = parse_put(
            &replace_body(&id, OTHER, &[("ubuntu", "absent")]),
            &accounts(),
            &known,
        )
        .unwrap();
        assert_eq!(
            deliver(&fake, &mut reg, &known, put, AT).await.err(),
            Some(Refuse::Confirm)
        );
        let mut b = replace_body(&id, OTHER, &[("ubuntu", &fp(TOKEN))]);
        b.insert("confirm".into(), json!("deliver"));
        let put = parse_put(&b, &accounts(), &known).unwrap();
        assert_eq!(
            deliver(&fake, &mut reg, &known, put, AT).await.err(),
            Some(Refuse::Confirm)
        );
        // A computer whose copy changed behind the page's back is left alone.
        let mut args = entry.args();
        args["secret"] = json!(THIRD);
        args["expiresAt"] = Value::Null;
        args["expect"] = json!(fp(TOKEN));
        args["computer"] = json!(reg.pin("ubuntu").unwrap().key);
        args["home"] = json!(fake.home("ubuntu").to_str().unwrap());
        aac_agent::login_verb("login-put", &args).unwrap();
        let put = parse_put(
            &replace_body(&id, OTHER, &[("ubuntu", &fp(TOKEN)), ("nas1", &fp(TOKEN))]),
            &accounts(),
            &known,
        )
        .unwrap();
        let receipt = deliver(&fake, &mut reg, &known, put, AT).await.unwrap();
        assert_eq!(states(&receipt), ["changed", "delivered"]);
        // Another account or kind under the same id: identity mismatch.
        let mut b2 = replace_body(&id, "zai.TESTONLY.key0000", &[("nas1", &fp(OTHER))]);
        b2.insert("kind".into(), json!("api-key"));
        b2.insert("provider".into(), json!("zai"));
        b2.insert("account".into(), json!("zai:1"));
        let put = parse_put(&b2, &accounts(), &known).unwrap();
        assert_eq!(
            deliver(&fake, &mut reg, &known, put, AT).await.err(),
            Some(Refuse::Identity)
        );
    }

    /// From the Sol review: every write route refuses a missing session, a device token, another
    /// origin and plain HTTP, and never echoes the posted secret.
    #[tokio::test]
    async fn every_write_route_refuses_without_session_origin_or_secure_address() {
        use crate::public_auth::{DashboardEvents, DashboardPrincipal, HubCache};
        use axum::body::{Body, to_bytes};
        let dir = tempfile::tempdir().unwrap();
        let (native, jobs) = NativeOwners::start(
            dir.path().canonicalize().unwrap(),
            Default::default(),
            HubCache::default(),
            DashboardEvents(tokio::sync::broadcast::channel(4).0),
            Default::default(),
        )
        .unwrap();
        for (principal, same_origin, secure, expected) in [
            (DashboardPrincipal::Anonymous, true, true, 401),
            (
                DashboardPrincipal::Device {
                    id: "test-device".into(),
                    platform: "mac".into(),
                },
                true,
                true,
                403,
            ),
            (
                DashboardPrincipal::Browser {
                    username: "fixture".into(),
                },
                false,
                true,
                403,
            ),
            (
                DashboardPrincipal::Browser {
                    username: "fixture".into(),
                },
                true,
                false,
                403,
            ),
        ] {
            for action in [
                Action::Put,
                Action::Copy,
                Action::Remove,
                Action::Check,
                Action::Trust,
            ] {
                let bytes = json!({"secret":TOKEN}).to_string();
                let mut request = Request::builder()
                    .method(Method::POST)
                    .uri(PATH)
                    .header("content-type", "application/json")
                    .header("content-length", bytes.len())
                    .body(Body::from(bytes))
                    .unwrap();
                request.extensions_mut().insert(DashboardAccess {
                    auth_enabled: true,
                    principal: principal.clone(),
                    secure_transport: secure,
                    same_origin,
                    origin_present: same_origin,
                    local_access: true,
                    session_id: Some("fixture".into()),
                    credential_transport: "https",
                    direct_loopback: true,
                    lan_proxy: false,
                    client_address: None,
                });
                let answer = handle(&native, action, request).await;
                assert_eq!(answer.status().as_u16(), expected, "{action:?}");
                let bytes = to_bytes(answer.into_body(), 8192).await.unwrap();
                assert!(!String::from_utf8_lossy(&bytes).contains("TESTONLY"));
            }
        }
        jobs.stop().await;
    }

    /// From both reviews: the private agent front door refuses every login verb.
    #[tokio::test]
    async fn the_private_front_door_refuses_every_login_verb() {
        use crate::agent_transport::{Agents, Call, internal_only};
        let dir = tempfile::tempdir().unwrap();
        let agents = std::sync::Arc::new(Agents::new(dir.path().canonicalize().unwrap()));
        let bytes = json!({"computer":{"platform":"ubuntu","sshHost":null},
            "args":{}, "deadlineMs":1000, "timeoutMs":2000, "maxBytes":4096,
            "callId":"review-test"})
        .to_string();
        for verb in ["login-inventory", "login-put", "login-remove", "login-copy"] {
            let call = Call::parse(bytes.as_bytes()).unwrap();
            assert!(internal_only(verb));
            assert!(call.bounded(verb));
            assert!(!call.valid(verb));
            let answer = agents.call(verb, bytes.as_bytes()).await;
            let body = axum::body::to_bytes(answer.into_body(), 8192)
                .await
                .unwrap();
            let frame: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(frame["type"], "fallback");
            assert_eq!(frame["reason"], "shape");
        }
    }

    #[test]
    fn ends_say_when_and_how_soon() {
        assert_eq!(
            ends(Kind::ApiKey, None, AT),
            ("No set end".into(), "ok".into())
        );
        assert_eq!(ends(Kind::ClaudeToken, None, AT).1, "unknown");
        let soon = storage::iso(AT + 10 * DAY_MS + 5).unwrap();
        assert_eq!(ends(Kind::ClaudeToken, Some(&soon), AT).1, "expiring");
        let past = storage::iso(AT - 1).unwrap();
        assert_eq!(ends(Kind::ClaudeToken, Some(&past), AT).1, "ended");
        let far = storage::iso(AT + 200 * DAY_MS).unwrap();
        assert_eq!(ends(Kind::ClaudeToken, Some(&far), AT).1, "ok");
    }
}
