//! Guarded reset claims. Inputs are PRIVATE collector snapshots, never browser bodies.
//! The executor seam must prove identity on the credential host before returning a snapshot.
//! No timer, switch handler or inventory read can dispatch a write.
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;

pub type Result<T> = std::result::Result<T, &'static str>;
fn hash(v: &str) -> String {
    Sha256::digest(v.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn encode<T: Serialize>(v: &T) -> Result<String> {
    serde_json::to_string(v).map_err(|_| "shape")
}
fn sql<T>(v: rusqlite::Result<T>) -> Result<T> {
    v.map_err(|_| "storage-unavailable")
}
fn opaque(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub use crate::reset_inventory::{Grant, Identity, Snapshot};
impl Snapshot {
    fn validate(&self, now: i64) -> Result<()> {
        if self.account_label.len() > 254 || self.account_label.chars().any(|c| c.is_control()) {
            return Err("shape");
        }
        if self.observed_at_ms > now || now - self.observed_at_ms > 30_000 {
            return Err("stale-inventory");
        }
        if !matches!(self.identity.provider.as_str(), "claude" | "codex" | "zai" | "qwen") {
            return Err("shape");
        }
        if self.grants.len() > 256
            || self.identity.account.is_empty()
            || self.identity.profile.is_empty()
            || self.identity.host.is_empty()
        {
            return Err("shape");
        }
        if self.windows.keys().any(|k| {
            k.is_empty()
                || k.len() > 64
                || !k
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        }) {
            return Err("shape");
        }
        if self
            .windows
            .values()
            .any(|v| !v.is_finite() || !(0.0..=100.0).contains(v))
        {
            return Err("shape");
        }
        let mut ids = std::collections::HashSet::new();
        for g in &self.grants {
            if g.left > 1000 || g.kind.len() > 64 || g.clears.len() > 16 || g.clears.iter().any(|s| s.len() > 64) {
                return Err("shape");
            }
            if let Some(id) = &g.id
                && (id.is_empty()
                    || id.len() > 256
                    || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    || !ids.insert(id))
            {
                return Err("shape");
            }
        }
        Ok(())
    }
}
fn kind(provider: &str, g: &Grant) -> Option<(&'static str, &'static str)> {
    match (provider, g.kind.as_str()) {
        ("claude", "session") => Some(("5-hour reset", "Refills your 5-hour allowance.")),
        ("claude", "weekly") => Some(("Weekly reset", "Refills your weekly allowance.")),
        ("claude", "full") => Some(("Full reset", "Refills your 5-hour and weekly allowances.")),
        ("claude", "fable") => Some((
            "Fable reset",
            "Refills Fable and gives back its weekly usage; it does not reset your whole week.",
        )),
        ("codex", "banked") => Some(("Banked reset", "Refills the eligible Codex usage windows.")),
        _ => None,
    }
}
fn blocked(s: &Snapshot, g: &Grant, now: i64) -> Option<&'static str> {
    if !matches!(s.identity.provider.as_str(), "claude" | "codex") {
        return Some("provider-read-only");
    }
    if kind(&s.identity.provider, g).is_none() {
        return Some("unknown-grant-kind");
    }
    if g.id.is_none() {
        return Some("exact-grant-unavailable");
    }
    if s.identity.provider == "claude"
        && g.id.as_ref().is_some_and(|id| {
            id.len() > 40
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        })
    {
        return Some("unsupported-grant-id");
    }
    if !s.executor_ready {
        return Some("credential-host-unavailable");
    }
    if s.identity.provider == "claude" && s.identity.organization.is_empty() {
        return Some("identity-unconfirmed");
    }
    if !g.usable || g.left == 0 || g.expires_at_ms.is_some_and(|v| v <= now) {
        return Some("grant-unavailable");
    }
    if g.clears.is_empty() || g.clears.iter().any(|k| !s.windows.contains_key(k)) {
        return Some("refill-windows-unconfirmed");
    }
    None
}
fn public(s: &Snapshot, g: &Grant, key: &str, now: i64) -> Value {
    let (label, text) =
        kind(&s.identity.provider, g).unwrap_or(("Other reset", "AAC does not know what this refills yet."));
    let known = kind(&s.identity.provider, g).is_some();
    // An unmapped kind keeps the provider's own (sanitised) words so it can be shown as the provider names it.
    let words = if known { None } else { g.words.as_deref() };
    json!({"grantKey":key,"kind":if known{g.kind.as_str()}else{"unknown"},"label":label,"refillsText":text,"providerWords":words,"resetsLeft":g.left,"expiresAtMs":g.expires_at_ms,"claimable":blocked(s,g,now).is_none(),"claimBlockedReason":blocked(s,g,now)})
}
fn fingerprint(s: &Snapshot, g: &Grant) -> Result<String> {
    // Sample timestamp deliberately excluded: a fresh read of unchanged state must match.
    Ok(hash(&encode(&(
        s.identity.clone(),
        g.clone(),
        &s.windows,
        s.executor_ready,
        &s.source_binding,
        &s.account_label,
    ))?))
}

/// Entropy comes only from the trusted Node crypto adapter, never request parameters.
/// Each call must supply fresh 32-byte CSPRNG hex. It is not written to audit history.
pub struct Store {
    db: Connection,
}
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Ok(meta) = std::fs::symlink_metadata(path)
            && (meta.file_type().is_symlink() || !meta.is_file())
        {
            return Err("unsafe-storage");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(parent) = path.parent() {
                let meta = std::fs::symlink_metadata(parent).map_err(|_| "unsafe-storage")?;
                if meta.file_type().is_symlink() || meta.permissions().mode() & 0o077 != 0 {
                    return Err("unsafe-storage");
                }
            }
        }
        // Caller creates a dedicated owner-only directory under getCcsDir(), not a share.
        let db = sql(Connection::open(path))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|_| "storage-unavailable")?;
        }
        sql(db.busy_timeout(std::time::Duration::from_secs(5)))?;
        sql(db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS aliases(account TEXT PRIMARY KEY, lock_key TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS targets(key TEXT PRIMARY KEY, account TEXT NOT NULL, snapshot TEXT NOT NULL, grant_json TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS confirmations(token TEXT PRIMARY KEY, caller TEXT NOT NULL, account TEXT NOT NULL, target TEXT NOT NULL, fingerprint TEXT NOT NULL, expires INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS attempts(key TEXT PRIMARY KEY, account TEXT NOT NULL, target TEXT NOT NULL, before_json TEXT NOT NULL, grant_json TEXT NOT NULL, outcome TEXT NOT NULL, pending INTEGER NOT NULL, created INTEGER NOT NULL, lock_key TEXT NOT NULL, provider_success INTEGER NOT NULL DEFAULT 0);
            CREATE UNIQUE INDEX IF NOT EXISTS pending_account ON attempts(lock_key) WHERE pending=1;
            CREATE TABLE IF NOT EXISTS holds(account TEXT PRIMARY KEY, until_ms INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS history(seq INTEGER PRIMARY KEY, at_ms INTEGER NOT NULL, account_hash TEXT NOT NULL, event TEXT NOT NULL, target_hash TEXT NOT NULL, outcome TEXT NOT NULL, details TEXT NOT NULL DEFAULT '{}');
            CREATE TABLE IF NOT EXISTS budgets(account TEXT NOT NULL, at_ms INTEGER NOT NULL);
            CREATE TRIGGER IF NOT EXISTS history_no_delete BEFORE DELETE ON history BEGIN SELECT RAISE(ABORT,'permanent audit'); END;
            CREATE TRIGGER IF NOT EXISTS history_no_update BEFORE UPDATE ON history BEGIN SELECT RAISE(ABORT,'permanent audit'); END;"))?;
        Ok(Self { db })
    }
    fn audit(
        tx: &Connection,
        now: i64,
        account: &str,
        event: &str,
        target: &str,
        outcome: &str,
        details: Value,
    ) -> Result<()> {
        sql(tx.execute(
            "INSERT INTO history(at_ms,account_hash,event,target_hash,outcome,details) VALUES(?,?,?,?,?,?)",
            params![now, hash(account), event, hash(target), outcome, encode(&details)?],
        ))?;
        Ok(())
    }
    pub fn inventory(&mut self, account: &str, s: &Snapshot, now: i64, entropy: &str) -> Result<Value> {
        s.validate(now)?;
        if !opaque(entropy) {
            return Err("entropy");
        }
        let tx = sql(self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate))?;
        sql(tx.execute(
            "INSERT INTO aliases VALUES(?,?) ON CONFLICT(account) DO UPDATE SET lock_key=excluded.lock_key",
            params![account, Self::canonical(s)?],
        ))?;
        // Keep old targets so an open confirmation cannot silently select another grant.
        let mut rows = vec![];
        for (i, g) in s.grants.iter().enumerate() {
            let key = hash(&format!("{entropy}:{i}"));
            sql(tx.execute(
                "INSERT INTO targets VALUES(?,?,?,?)",
                params![key, account, encode(s)?, encode(g)?],
            ))?;
            rows.push(public(s, g, &key, now));
        }
        sql(tx.commit())?;
        Ok(
            json!({"status":if s.identity.provider=="qwen"{"unsupported"}else{"ok"},"observedAtMs":s.observed_at_ms,"sourceAgeMs":now-s.observed_at_ms,"canonicalAccountKey":Self::canonical(s)?,"grants":rows}),
        )
    }
    fn canonical(s: &Snapshot) -> Result<String> {
        Ok(hash(&encode(&(
            &s.identity.provider,
            &s.identity.account,
            &s.identity.organization,
        ))?))
    }
    fn account_canonical(&self, account: &str) -> Result<Option<String>> {
        let key: Option<String> = sql(self
            .db
            .query_row("SELECT lock_key FROM aliases WHERE account=?", [account], |r| r.get(0))
            .optional())?;
        if key.is_some() {
            return Ok(key);
        }
        // Pre-alias-schema attempts and inventory targets remain discoverable after upgrade.
        let snapshot: Option<String> = sql(self
            .db
            .query_row(
                "SELECT before_json FROM attempts WHERE account=? ORDER BY created DESC LIMIT 1",
                [account],
                |r| r.get(0),
            )
            .optional())?;
        let snapshot = match snapshot {
            Some(s) => Some(s),
            None => sql(self
                .db
                .query_row(
                    "SELECT snapshot FROM targets WHERE account=? ORDER BY rowid DESC LIMIT 1",
                    [account],
                    |r| r.get(0),
                )
                .optional())?,
        };
        snapshot
            .map(|s| Self::canonical(&serde_json::from_str(&s).map_err(|_| "shape")?))
            .transpose()
    }
    fn target(&self, account: &str, key: &str) -> Result<(Snapshot, Grant)> {
        let (s, g): (String, String) = sql(self.db.query_row(
            "SELECT snapshot,grant_json FROM targets WHERE key=? AND account=?",
            params![key, account],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ))?;
        Ok((
            serde_json::from_str(&s).map_err(|_| "shape")?,
            serde_json::from_str(&g).map_err(|_| "shape")?,
        ))
    }
    fn selected(&self, account: &str, key: &str, fresh: &Snapshot, now: i64) -> Result<Grant> {
        fresh.validate(now)?;
        let (old, g) = self.target(account, key)?;
        if old.identity != fresh.identity {
            return Err("identity-changed");
        }
        let found = fresh
            .grants
            .iter()
            .find(|n| n.id == g.id && n.id.is_some())
            .ok_or("grant-changed")?;
        if fingerprint(&old, &g)? != fingerprint(fresh, found)? {
            return Err("grant-changed");
        }
        if let Some(reason) = blocked(fresh, found, now) {
            return Err(reason);
        }
        Ok(found.clone())
    }
    fn caller(caller: &str) -> Result<()> {
        if caller == "local" || caller.strip_prefix("session:").is_some_and(opaque) {
            Ok(())
        } else {
            Err("browser-session-required")
        }
    }
    /// Refuse known holds/limits before the adapter performs any provider read.
    /// The main prepare/begin transaction still rechecks them after the fresh read.
    pub fn gate(&self, account: &str, key: &str, now: i64, prepare: bool) -> Result<()> {
        let (snapshot, _) = self.target(account, key)?;
        let canonical = hash(&encode(&(
            &snapshot.identity.provider,
            &snapshot.identity.account,
            &snapshot.identity.organization,
        ))?);
        let hold: Option<i64> = sql(self
            .db
            .query_row("SELECT until_ms FROM holds WHERE account=?", [&canonical], |r| r.get(0))
            .optional())?;
        if hold.is_some_and(|v| v > now) {
            return Err("cooldown");
        }
        let pending: i64 = sql(self.db.query_row(
            "SELECT count(*) FROM attempts WHERE lock_key=? AND pending=1",
            [&canonical],
            |r| r.get(0),
        ))?;
        if pending > 0 {
            return Err("check-again-required");
        }
        if prepare {
            let n: i64 = sql(self.db.query_row(
                "SELECT count(*) FROM budgets WHERE account=? AND at_ms>?",
                params![canonical, now - 60_000],
                |r| r.get(0),
            ))?;
            if n >= 6 {
                return Err("rate-limited");
            }
        } else {
            for (window, max) in [(60_000, 3), (3_600_000, 10)] {
                let n: i64 = sql(self.db.query_row(
                    "SELECT count(*) FROM attempts WHERE lock_key=? AND created>?",
                    params![canonical, now - window],
                    |r| r.get(0),
                ))?;
                if n >= max {
                    return Err("rate-limited");
                }
            }
        }
        Ok(())
    }
    pub fn prepare(
        &mut self,
        account: &str,
        key: &str,
        caller: &str,
        fresh: &Snapshot,
        now: i64,
        token: &str,
    ) -> Result<Value> {
        Self::caller(caller)?;
        if !opaque(token) {
            return Err("entropy");
        }
        let g = self.selected(account, key, fresh, now)?;
        let fp = fingerprint(fresh, &g)?;
        let tx = sql(self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate))?;
        let lock_key = hash(&encode(&(
            &fresh.identity.provider,
            &fresh.identity.account,
            &fresh.identity.organization,
        ))?);
        let count: i64 = sql(tx.query_row(
            "SELECT count(*) FROM budgets WHERE account=? AND at_ms>?",
            params![lock_key, now - 60_000],
            |r| r.get(0),
        ))?;
        if count >= 6 {
            return Err("rate-limited");
        }
        let pending: i64 = sql(tx.query_row(
            "SELECT count(*) FROM attempts WHERE account=? AND pending=1",
            [account],
            |r| r.get(0),
        ))?;
        if pending > 0 {
            return Err("check-again-required");
        }
        sql(tx.execute("DELETE FROM confirmations WHERE expires<=?", [now]))?;
        sql(tx.execute(
            "INSERT INTO confirmations VALUES(?,?,?,?,?,?)",
            params![hash(token), hash(caller), account, key, fp, now + 120_000],
        ))?;
        sql(tx.execute("INSERT INTO budgets VALUES(?,?)", params![lock_key, now]))?;
        Self::audit(
            &tx,
            now,
            account,
            "prepare",
            key,
            "prepared",
            json!({"provider":fresh.identity.provider,"kind":g.kind,"grantHash":hash(g.id.as_deref().unwrap_or("")),"callerHash":hash(caller),"callerKind":if caller=="local"{"local"}else{"session"},"identityHash":hash(&encode(&fresh.identity)?),"beforeCount":g.left}),
        )?;
        sql(tx.commit())?;
        Ok(
            json!({"account":{"label":fresh.account_label},"confirmationToken":token,"expiresAtMs":now+120_000,"grant":public(fresh,&g,key,now),"costText":"Spends one selected reset. This cannot be undone."}),
        )
    }
    /// Atomically consumes confirmation and durably reserves the account BEFORE any I/O.
    /// A crashed/ambiguous attempt stays pending forever until a read proves its result.
    #[expect(clippy::too_many_arguments, reason = "Preserve the existing explicit claim boundary")]
    pub fn begin(
        &mut self,
        account: &str,
        key: &str,
        caller: &str,
        token: &str,
        fresh: &Snapshot,
        now: i64,
        attempt: &str,
    ) -> Result<Write> {
        Self::caller(caller)?;
        if !opaque(attempt) {
            return Err("entropy");
        }
        // Burn even a stale/wrong-caller token. Commit deletion independently of validation.
        let c: Option<(String, String, String, String, i64)> = sql(self
            .db
            .query_row(
                "SELECT caller,account,target,fingerprint,expires FROM confirmations WHERE token=?",
                [hash(token)],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional())?;
        if sql(self
            .db
            .execute("DELETE FROM confirmations WHERE token=?", [hash(token)]))?
            != 1
        {
            return Err("confirmation-invalid");
        }
        let (who, acct, target, fp, expiry) = c.ok_or("confirmation-invalid")?;
        if who != hash(caller) || acct != account || target != key || expiry <= now {
            return Err("confirmation-invalid");
        }
        let g = self.selected(account, key, fresh, now)?;
        if fp != fingerprint(fresh, &g)? {
            return Err("grant-changed");
        }
        let write = Write::new(fresh, &g, attempt)?;
        let lock_key = hash(&encode(&(
            &fresh.identity.provider,
            &fresh.identity.account,
            &fresh.identity.organization,
        ))?);
        let tx = sql(self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate))?;
        let hold: Option<i64> = sql(tx
            .query_row("SELECT until_ms FROM holds WHERE account=?", [&lock_key], |r| r.get(0))
            .optional())?;
        if hold.is_some_and(|v| v > now) {
            return Err("cooldown");
        }
        for (window, max) in [(60_000, 3), (3_600_000, 10)] {
            let n: i64 = sql(tx.query_row(
                "SELECT count(*) FROM attempts WHERE lock_key=? AND created>?",
                params![lock_key, now - window],
                |r| r.get(0),
            ))?;
            if n >= max {
                return Err("rate-limited");
            }
        }
        sql(tx.execute(
            "INSERT INTO attempts VALUES(?,?,?,?,?,'claimed-unconfirmed',1,?,?,0)",
            params![attempt, account, key, encode(fresh)?, encode(&g)?, now, lock_key],
        ))?;
        Self::audit(
            &tx,
            now,
            account,
            "attempt",
            key,
            "claimed-unconfirmed",
            json!({"provider":fresh.identity.provider,"kind":g.kind,"grantHash":hash(g.id.as_deref().unwrap_or("")),"callerHash":hash(caller),"callerKind":if caller=="local"{"local"}else{"session"},"identityHash":hash(&encode(&fresh.identity)?),"idempotencyHash":hash(attempt),"beforeCount":g.left,"beforeWindows":fresh.windows}),
        )?;
        sql(tx.commit())?;
        Ok(write)
    }
    fn committed_result(tx: &Connection, account: &str, attempt: &str) -> Result<Value> {
        let (g, stored, previous_success): (String, String, bool) = sql(tx.query_row(
            "SELECT grant_json,outcome,provider_success FROM attempts WHERE key=? AND account=?",
            params![attempt, account],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ))?;
        let details: Option<String> = sql(tx
            .query_row(
                "SELECT details FROM history WHERE event='verification' AND target_hash=? ORDER BY seq DESC LIMIT 1",
                [hash(attempt)],
                |r| r.get(0),
            )
            .optional())?;
        let details: Value = details
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or(Value::Null);
        let grant: Grant = serde_json::from_str(&g).map_err(|_| "shape")?;
        let windows: Vec<Value> = if stored == "claimed" {
            grant
                .clears
                .iter()
                .filter_map(|k| {
                    details["afterWindows"][k]
                        .as_f64()
                        .map(|v| json!({"id":k,"usedPercent":v}))
                })
                .collect()
        } else {
            vec![]
        };
        Ok(
            json!({"attemptKey":attempt,"outcome":stored,"verification":if stored=="claimed"{"confirmed"}else{"unconfirmed"},"providerSuccess":previous_success,"windows":windows}),
        )
    }
    /// Called after provider send; errors and unknown bodies MUST map to Ambiguous.
    pub fn finish(
        &mut self,
        account: &str,
        attempt: &str,
        outcome: Outcome,
        after: Option<&Snapshot>,
        now: i64,
        retry_after_ms: Option<i64>,
    ) -> Result<Value> {
        let tx = sql(self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate))?;
        let (before, g, pending, previous_success): (String, String, bool, bool) = sql(tx.query_row(
            "SELECT before_json,grant_json,pending,provider_success FROM attempts WHERE key=? AND account=?",
            params![attempt, account],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ))?;
        if !pending {
            return Self::committed_result(&tx, account, attempt);
        }
        let before: Snapshot = serde_json::from_str(&before).map_err(|_| "shape")?;
        let g: Grant = serde_json::from_str(&g).map_err(|_| "shape")?;
        let verified = matches!(outcome, Outcome::Reset | Outcome::Ambiguous | Outcome::PriorSuccess)
            && after.is_some_and(|a| verify(&before, &g, a, now));
        let (result, pending) = if verified {
            ("claimed", false)
        } else {
            match outcome {
                Outcome::Reset | Outcome::Ambiguous => ("claimed-unconfirmed", true),
                Outcome::Nothing => ("nothing-to-reset", false),
                Outcome::NoCredit => ("no-credit", false),
                Outcome::AlreadyUsed => ("already-used", false),
                Outcome::PriorSuccess => ("claimed-unconfirmed", true),
                Outcome::Cooldown => ("cooldown", false),
                Outcome::RateLimited => ("rate-limited", false),
                Outcome::SignedOut => ("signed-out", false),
                Outcome::IdentityChanged => ("identity-changed", false),
                Outcome::Unavailable => ("unavailable", false),
            }
        };
        let provider_success = previous_success || matches!(outcome, Outcome::Reset | Outcome::PriorSuccess);
        let changed = sql(tx.execute(
            "UPDATE attempts SET outcome=?,pending=?,provider_success=? WHERE key=? AND account=? AND pending=1",
            params![result, pending, provider_success, attempt, account],
        ))?;
        if changed == 0 {
            return Self::committed_result(&tx, account, attempt);
        }
        if matches!(outcome, Outcome::Cooldown | Outcome::RateLimited) {
            let delay = retry_after_ms.unwrap_or(60_000).clamp(1_000, 86_400_000);
            sql(tx.execute("INSERT INTO holds VALUES(?,?) ON CONFLICT(account) DO UPDATE SET until_ms=max(until_ms,excluded.until_ms)",params![hash(&encode(&(&before.identity.provider,&before.identity.account,&before.identity.organization))?),now+delay]))?;
        }
        Self::audit(
            &tx,
            now,
            account,
            "verification",
            attempt,
            result,
            json!({"provider":before.identity.provider,"kind":g.kind,"grantHash":hash(g.id.as_deref().unwrap_or("")),"idempotencyHash":hash(attempt),"providerSuccess":provider_success,"providerOutcome":outcome.word(),"verified":verified,"beforeCount":g.left,"afterCount":after.and_then(|a|a.grants.iter().find(|n|n.id==g.id).map(|n|n.left)),"beforeWindows":before.windows,"afterWindows":after.map(|a|&a.windows)}),
        )?;
        sql(tx.commit())?;
        // The refilled windows as the verification read saw them; only a confirmed claim reports them.
        let windows: Vec<Value> = if verified {
            after
                .map(|a| {
                    g.clears
                        .iter()
                        .filter_map(|k| a.windows.get(k).map(|v| json!({"id":k,"usedPercent":v})))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            vec![]
        };
        Ok(
            json!({"attemptKey":attempt,"outcome":result,"verification":if verified{"confirmed"}else{"unconfirmed"},"providerSuccess":provider_success,"windows":windows}),
        )
    }
    /// The route account's unresolved attempt, if any: the browser needs its key to check it again after a reload or
    /// from another signed-in session. Reading it sends nothing and consumes nothing.
    pub fn pending(&self, account: &str) -> Result<Value> {
        let key: Option<String> = sql(self
            .db
            .query_row(
                "SELECT key FROM attempts WHERE (lock_key=? OR (account=? AND ? IS NULL)) AND pending=1 ORDER BY created DESC LIMIT 1",
                params![self.account_canonical(account)?,account,self.account_canonical(account)?],
                |r| r.get(0),
            )
            .optional())?;
        Ok(json!({ "attemptKey": key }))
    }
    /// Read-only at the provider: this method has no Write return path.
    pub fn check(&mut self, account: &str, attempt: &str, after: &Snapshot, now: i64) -> Result<Value> {
        let canonical = self.account_canonical(account)?;
        let owner: String = sql(self.db.query_row(
            "SELECT account FROM attempts WHERE key=? AND (lock_key=? OR (account=? AND ? IS NULL))",
            params![attempt, canonical, account, canonical],
            |r| r.get(0),
        ))?;
        // verify() still compares the full original identity and source binding.
        self.finish(&owner, attempt, Outcome::Ambiguous, Some(after), now, None)
    }
    pub fn history(&self, account: &str) -> Result<Value> {
        let Some(canonical) = self.account_canonical(account)? else {
            return Ok(json!([]));
        };
        // Route names can be reused for another login. Audit ownership comes from the
        // immutable target snapshot (prepare/attempt) or reserved attempt (verification).
        let mut targets = std::collections::HashSet::new();
        let mut stmt = sql(self.db.prepare("SELECT key,snapshot FROM targets"))?;
        let rows = sql(stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))))?;
        for row in rows {
            let (key, snapshot) = sql(row)?;
            let snapshot: Snapshot = serde_json::from_str(&snapshot).map_err(|_| "shape")?;
            if Self::canonical(&snapshot)? == canonical {
                targets.insert(hash(&key));
            }
        }
        let mut attempts = std::collections::HashSet::new();
        let mut stmt = sql(self.db.prepare("SELECT key,target FROM attempts WHERE lock_key=?"))?;
        let rows = sql(stmt.query_map([canonical], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))))?;
        for row in rows {
            let (key, target) = sql(row)?;
            attempts.insert(hash(&key));
            targets.insert(hash(&target));
        }
        let mut stmt = sql(self
            .db
            .prepare("SELECT at_ms,event,target_hash,outcome,details FROM history ORDER BY seq"))?;
        let rows = sql(stmt.query_map([], |r| {
            Ok(json!({"atMs":r.get::<_,i64>(0)?,"event":r.get::<_,String>(1)?,"targetHash":r.get::<_,String>(2)?,"outcome":r.get::<_,String>(3)?,"details":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(Value::Null)}))
        }))?;
        let mut events = vec![];
        for row in rows {
            let event = sql(row)?;
            let key = event["targetHash"].as_str().unwrap_or("");
            let owned = match event["event"].as_str() {
                Some("prepare" | "attempt") => targets.contains(key),
                Some("verification") => attempts.contains(key),
                _ => false,
            };
            if owned {
                events.push(event);
            }
        }
        Ok(Value::Array(events))
    }
}
#[derive(Clone, Copy)]
pub enum Outcome {
    Reset,
    Ambiguous,
    Nothing,
    NoCredit,
    AlreadyUsed,
    PriorSuccess,
    Cooldown,
    RateLimited,
    SignedOut,
    IdentityChanged,
    Unavailable,
}
impl Outcome {
    fn word(self) -> &'static str {
        match self {
            Self::Reset => "reset",
            Self::Ambiguous => "unconfirmed",
            Self::Nothing => "nothing-to-reset",
            Self::NoCredit => "no-credit",
            Self::AlreadyUsed => "already-used",
            Self::PriorSuccess => "already-redeemed",
            Self::Cooldown => "cooldown",
            Self::RateLimited => "rate-limited",
            Self::SignedOut => "signed-out",
            Self::IdentityChanged => "identity-changed",
            Self::Unavailable => "unavailable",
        }
    }
}
/// Private transport request. Never serialize into an HTTP response or log.
#[derive(Debug)]
pub struct Write {
    pub method: &'static str,
    pub body: Value,
    pub identity: Identity,
    pub source_binding: String,
}
impl Write {
    fn new(s: &Snapshot, g: &Grant, key: &str) -> Result<Self> {
        let id = g.id.as_deref().ok_or("exact-grant-unavailable")?;
        let (method, body) = match s.identity.provider.as_str() {
            "claude" => {
                if id.len() > 40
                    || !id
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
                {
                    return Err("exact-grant-unavailable");
                }
                (
                    "claude.reset_rate_limits",
                    json!({"program":"cedar_ember","grant_id":id,"request_id":key}),
                )
            }
            "codex" => (
                "account/rateLimitResetCredit/consume",
                json!({"creditId":id,"idempotencyKey":key}),
            ),
            _ => return Err("provider-read-only"),
        };
        Ok(Self {
            method,
            body,
            identity: s.identity.clone(),
            source_binding: s.source_binding.clone(),
        })
    }
}
fn verify(before: &Snapshot, g: &Grant, after: &Snapshot, now: i64) -> bool {
    if after.validate(now).is_err()
        || after.identity != before.identity
        || after.observed_at_ms <= before.observed_at_ms
    {
        return false;
    }
    // Missing grant is not sufficient proof: require an explicit chosen-grant decrement.
    let Some(new) = after.grants.iter().find(|n| n.id == g.id) else {
        return false;
    };
    new.left.checked_add(1) == Some(g.left)
        && !g.clears.is_empty()
        && g.clears
            .iter()
            .all(|k| match (before.windows.get(k), after.windows.get(k)) {
                (Some(b), Some(a)) => a <= b,
                _ => false,
            })
        && g.clears
            .iter()
            .any(|k| match (before.windows.get(k), after.windows.get(k)) {
                (Some(b), Some(a)) => a < b,
                _ => false,
            })
}

/// Native ABI used only by the internal adapter. Errors are fixed codes, never raw provider bodies.
pub fn dispatch(args: &str) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Input {
        db_path: String,
        op: String,
        account: String,
        now_ms: i64,
        #[serde(default)]
        key: String,
        #[serde(default)]
        caller: String,
        #[serde(default)]
        token: String,
        #[serde(default)]
        entropy: String,
        snapshot: Option<Snapshot>,
        outcome: Option<String>,
        retry_after_ms: Option<i64>,
    }
    let a: Input = serde_json::from_str(args).map_err(|_| "shape")?;
    // Enforce TTL when the libuv task actually runs, not when it was queued.
    let _request_clock = a.now_ms;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "clock")?
        .as_millis() as i64;
    let mut s = Store::open(Path::new(&a.db_path))?;
    let snapshot = || a.snapshot.as_ref().ok_or("inventory-unavailable");
    match a.op.as_str() {
        "prepareGate" | "claimGate" => {
            s.gate(&a.account, &a.key, now_ms, a.op == "prepareGate")?;
            Ok(json!({"ok":true}))
        }
        "inventory" => s.inventory(&a.account, snapshot()?, now_ms, &a.entropy),
        "prepare" => s.prepare(&a.account, &a.key, &a.caller, snapshot()?, now_ms, &a.entropy),
        "begin" => {
            let w = s.begin(&a.account, &a.key, &a.caller, &a.token, snapshot()?, now_ms, &a.entropy)?;
            Ok(
                json!({"method":w.method,"body":w.body,"identity":w.identity,"sourceBinding":w.source_binding,"attemptKey":a.entropy,"routeAccount":a.account}),
            )
        }
        "finish" => {
            let o = match a.outcome.as_deref() {
                Some("reset") => Outcome::Reset,
                Some("nothingToReset") => Outcome::Nothing,
                Some("noCredit") => Outcome::NoCredit,
                Some("alreadyRedeemed") => Outcome::PriorSuccess,
                Some("already-used") => Outcome::AlreadyUsed,
                Some("cooldown") => Outcome::Cooldown,
                Some("rate-limited") => Outcome::RateLimited,
                Some("signed-out") => Outcome::SignedOut,
                Some("identity-changed") => Outcome::IdentityChanged,
                Some("unavailable") => Outcome::Unavailable,
                _ => Outcome::Ambiguous,
            };
            s.finish(&a.account, &a.key, o, a.snapshot.as_ref(), now_ms, a.retry_after_ms)
        }
        "check" => s.check(&a.account, &a.key, snapshot()?, now_ms),
        "history" => s.history(&a.account),
        "pending" => s.pending(&a.account),
        _ => Err("unknown-method"),
    }
}
