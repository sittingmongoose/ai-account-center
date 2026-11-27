//! Central Logins step 2: the agent verbs that deliver static logins on this computer.
//!
//! - `login-inventory` lists what this computer holds: id, kind, provider, account, fingerprint,
//!   end and when it arrived. Never a secret.
//! - `login-put` writes one static login, compare before replace (`expect` is `absent` or the
//!   fingerprint the dashboard showed when the owner confirmed).
//! - `login-remove` removes one, only when its identity and fingerprint are the confirmed ones.
//! - `login-copy` hands one static login to the dashboard so it can deliver it to another
//!   computer; it answers only for the exact identity and fingerprint asked for.
//!
//! Every verb refuses a rotating sign-in **by its type** (`aac_files::credential::Kind`): a Claude
//! or Codex sign-in, an Antigravity, Muse or Cursor sign-in, a pasted web session. Only the
//! `copy` answer carries a secret, and only to the dashboard process, which never returns it.
use crate::proof;
use aac_core::{Cause, Failure};
use aac_files::credential::{self, CredentialFile, Kind, Record, Refusal, Summary, Written};
use serde_json::{Value, json};
use std::path::PathBuf;

const REFUSED_ROTATING: &str = "rotating sign-ins are never delivered";
const REFUSED_KIND: &str = "not a static login";
const IDENTITY: &str = "login identity differs";
const CHANGED: &str = "login changed since it was confirmed";

fn refused(detail: &'static str) -> Failure {
    Failure::new(Cause::Refused, detail)
}

fn refusal(why: Refusal) -> Failure {
    match why {
        Refusal::Rotating => refused(REFUSED_ROTATING),
        Refusal::Unknown => refused(REFUSED_KIND),
    }
}

/// Only these keys, each a string (or `expiresAt` null, `nowMs` a number).
fn keys(args: &Value, allowed: &[&str], required: &[&str]) -> Result<(), Failure> {
    let object = args
        .as_object()
        .ok_or_else(|| refused("invalid login request"))?;
    if !proof::proven(args)
        || object.keys().any(|k| !allowed.contains(&k.as_str()))
        || required.iter().any(|k| !object.contains_key(*k))
    {
        return Err(refused("invalid login request"));
    }
    Ok(())
}

fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str, Failure> {
    args[key]
        .as_str()
        .ok_or_else(|| refused("invalid login request"))
}

/// The computer's AAC private folder: `home/.ccs` (tests), else `CCS_HOME`, else `$HOME/.ccs`.
fn root(args: &Value) -> Result<PathBuf, Failure> {
    let root = if let Some(home) = args.get("home") {
        PathBuf::from(home.as_str().ok_or_else(|| refused("invalid home"))?).join(".ccs")
    } else if let Some(ccs) = std::env::var_os("CCS_HOME") {
        PathBuf::from(ccs)
    } else {
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(|home| PathBuf::from(home).join(".ccs"))
            .ok_or_else(|| refused("no home folder"))?
    };
    if !root.is_absolute() {
        return Err(refused("invalid home"));
    }
    Ok(root)
}

/// The kind, refused by type when it rotates, and its provider rule: a Claude token is Claude's,
/// an API key belongs to a provider whose keys AAC stores.
fn kind_and_provider(args: &Value) -> Result<(Kind, String), Failure> {
    let kind = Kind::admit(text(args, "kind")?).map_err(refusal)?;
    let provider = text(args, "provider")?;
    let fits = match kind {
        Kind::ClaudeToken => provider == "claude",
        Kind::ApiKey => aac_providers::provider(provider).is_some_and(|p| p.key_store),
    };
    if !fits {
        return Err(refused(REFUSED_KIND));
    }
    Ok((kind, provider.to_owned()))
}

/// The identity a request names: id, kind, provider and account.
fn identity(args: &Value) -> Result<(String, Kind, String, String), Failure> {
    let (kind, provider) = kind_and_provider(args)?;
    let login_id = text(args, "loginId")?;
    let account = text(args, "account")?;
    if !credential::valid_login_id(login_id) || !credential::valid_account(account) {
        return Err(refused("invalid login request"));
    }
    Ok((login_id.to_owned(), kind, provider, account.to_owned()))
}

fn same(summary: &Summary, (id, kind, provider, account): &(String, Kind, String, String)) -> bool {
    &summary.login_id == id
        && summary.kind == *kind
        && &summary.provider == provider
        && &summary.account == account
}

fn now_stamp(args: &Value) -> Result<String, Failure> {
    let millis = match args.get("nowMs") {
        Some(now) => now.as_i64().ok_or_else(|| refused("invalid clock"))?,
        None => i64::try_from(aac_core::UtcMs::now()?.get()).map_err(|_| refused("clock"))?,
    };
    chrono::DateTime::from_timestamp_millis(millis - millis.rem_euclid(1000))
        .map(|at| at.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .ok_or_else(|| refused("invalid clock"))
}

/// The login file's lease, waiting up to two seconds while another writer (or a child process that
/// briefly inherited the lease between fork and exec) holds it. Any other failure is immediate.
fn claim(
    root: &std::path::Path,
    provider: &str,
    login_id: &str,
) -> Result<CredentialFile, Failure> {
    let start = std::time::Instant::now();
    loop {
        match CredentialFile::claim_as_agent(root, provider, login_id) {
            Err(e)
                if e.cause == Cause::Busy
                    && start.elapsed() < std::time::Duration::from_secs(2) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            other => return other,
        }
    }
}

fn os() -> &'static str {
    std::env::consts::OS
}

pub(crate) fn run(verb: &str, args: &Value) -> Result<Value, Failure> {
    match verb {
        "login-inventory" => inventory(args),
        "login-put" => put(args),
        "login-remove" => remove(args),
        "login-copy" => copy(args),
        _ => Err(refused("unsupported agent verb")),
    }
}

fn inventory(args: &Value) -> Result<Value, Failure> {
    keys(args, &["home"], &[])?;
    let listed = credential::list(&root(args)?)?;
    let mut logins = Vec::new();
    let mut unreadable = 0u32;
    for (_, summary) in listed {
        match summary {
            Ok(summary) => logins.push(summary),
            Err(_) => unreadable += 1,
        }
    }
    Ok(json!({"os": os(), "logins": logins, "unreadable": unreadable}))
}

fn put(args: &Value) -> Result<Value, Failure> {
    keys(
        args,
        &[
            "loginId",
            "kind",
            "provider",
            "account",
            "secret",
            "expiresAt",
            "expect",
            "home",
            "nowMs",
        ],
        &[
            "loginId",
            "kind",
            "provider",
            "account",
            "secret",
            "expiresAt",
            "expect",
        ],
    )?;
    let wanted = identity(args)?;
    let secret = text(args, "secret")?;
    credential::admit_secret(wanted.1, secret).map_err(refusal)?;
    let expires_at = match &args["expiresAt"] {
        Value::Null => None,
        Value::String(at) if credential::valid_stamp(at) => Some(at.clone()),
        _ => return Err(refused("invalid login end")),
    };
    let expect = text(args, "expect")?;
    if expect != "absent" && !credential::valid_fingerprint(expect) {
        return Err(refused("invalid login request"));
    }
    let fingerprint = credential::fingerprint(secret);
    let record = Record::new(
        Summary {
            login_id: wanted.0.clone(),
            kind: wanted.1,
            provider: wanted.2.clone(),
            account: wanted.3.clone(),
            fingerprint: fingerprint.clone(),
            expires_at,
            written_at: now_stamp(args)?,
        },
        secret.to_owned(),
    )
    .map_err(refusal)?;
    let file = claim(&root(args)?, &wanted.2, &wanted.0)?;
    let written = file.replace(&record, |current| match (current, expect) {
        (None, "absent") => Ok(()),
        (None, _) => Err(Failure::new(Cause::ConfirmationInvalid, CHANGED)),
        (Some(current), _) if !same(&current.summary, &wanted) => {
            Err(Failure::new(Cause::IdentityMismatch, IDENTITY))
        }
        // A repeat of the same delivery (its answer was lost) is not a change.
        (Some(current), "absent") if current.summary.fingerprint == fingerprint => Ok(()),
        (Some(current), expect) if current.summary.fingerprint == expect => Ok(()),
        (Some(_), _) => Err(Failure::new(Cause::ConfirmationInvalid, CHANGED)),
    })?;
    let written = match written {
        Written::Created => "created",
        Written::Replaced => "replaced",
        Written::Unchanged => "unchanged",
    };
    Ok(json!({"ok": true, "fingerprint": fingerprint, "written": written, "os": os()}))
}

fn remove(args: &Value) -> Result<Value, Failure> {
    keys(
        args,
        &["loginId", "kind", "provider", "account", "expect", "home"],
        &["loginId", "kind", "provider", "account", "expect"],
    )?;
    let wanted = identity(args)?;
    let expect = text(args, "expect")?;
    if !credential::valid_fingerprint(expect) {
        return Err(refused("invalid login request"));
    }
    let file = claim(&root(args)?, &wanted.2, &wanted.0)?;
    let removed = file.remove(|current| {
        if !same(&current.summary, &wanted) {
            Err(Failure::new(Cause::IdentityMismatch, IDENTITY))
        } else if current.summary.fingerprint != expect {
            Err(Failure::new(Cause::ConfirmationInvalid, CHANGED))
        } else {
            Ok(())
        }
    })?;
    Ok(json!({"ok": true, "removed": removed, "os": os()}))
}

fn copy(args: &Value) -> Result<Value, Failure> {
    keys(
        args,
        &[
            "loginId",
            "kind",
            "provider",
            "account",
            "fingerprint",
            "home",
        ],
        &["loginId", "kind", "provider", "account", "fingerprint"],
    )?;
    let wanted = identity(args)?;
    let fingerprint = text(args, "fingerprint")?;
    if !credential::valid_fingerprint(fingerprint) {
        return Err(refused("invalid login request"));
    }
    let file = claim(&root(args)?, &wanted.2, &wanted.0)?;
    let record = file
        .read()?
        .ok_or_else(|| Failure::new(Cause::ConfirmationInvalid, CHANGED))?;
    if !same(&record.summary, &wanted) {
        return Err(Failure::new(Cause::IdentityMismatch, IDENTITY));
    }
    if record.summary.fingerprint != fingerprint {
        return Err(Failure::new(Cause::ConfirmationInvalid, CHANGED));
    }
    // The static kind and shape are checked again on the way out: a file edited by hand into a
    // rotating shape never leaves this computer.
    credential::admit_secret(record.summary.kind, record.secret()).map_err(refusal)?;
    Ok(json!({
        "ok": true,
        "expiresAt": record.summary.expires_at,
        "secret": record.secret(),
        "os": os(),
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const TOKEN: &str = "sk-ant-oat01-TESTONLY-agent-token-0000000000000";
    const OTHER: &str = "sk-ant-oat01-TESTONLY-agent-token-1111111111111";

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn put_args(home: &tempfile::TempDir, secret: &str, expect: &str) -> Value {
        json!({"loginId":"0a1b2c3d","kind":"claude-token","provider":"claude",
            "account":"claude:test","secret":secret,"expiresAt":"2027-11-27T00:00:00Z",
            "expect":expect,"home":home.path().to_str().unwrap(),"nowMs":1_795_000_000_000i64})
    }

    fn ident(home: &tempfile::TempDir) -> Value {
        json!({"loginId":"0a1b2c3d","kind":"claude-token","provider":"claude",
            "account":"claude:test","home":home.path().to_str().unwrap()})
    }

    /// Red/green: every rotating kind is refused by its type, before any file is touched.
    #[test]
    fn every_verb_refuses_rotating_sign_ins_by_type() {
        let home = home();
        for kind in credential::ROTATING_KINDS {
            let mut args = put_args(&home, TOKEN, "absent");
            args["kind"] = json!(kind);
            let error = run("login-put", &args).unwrap_err();
            assert_eq!(error.cause, Cause::Refused);
            assert_eq!(error.detail.as_str(), REFUSED_ROTATING, "{kind}");
            let mut args = ident(&home);
            args["kind"] = json!(kind);
            args["fingerprint"] = json!(credential::fingerprint(TOKEN));
            assert_eq!(
                run("login-copy", &args).unwrap_err().detail.as_str(),
                REFUSED_ROTATING
            );
            args.as_object_mut().unwrap().remove("fingerprint");
            args["expect"] = json!(credential::fingerprint(TOKEN));
            assert_eq!(
                run("login-remove", &args).unwrap_err().detail.as_str(),
                REFUSED_ROTATING
            );
        }
        // A refresh token pasted as a Claude token, or a whole sign-in file, is refused too.
        for secret in [
            "sk-ant-ort01-TESTONLY-refresh-0000",
            r#"{"refreshToken":"x"}"#,
        ] {
            let error = run("login-put", &put_args(&home, secret, "absent")).unwrap_err();
            assert_eq!(error.detail.as_str(), REFUSED_ROTATING);
        }
        // A Claude token is Claude's; an API key needs a provider whose keys AAC stores.
        let mut args = put_args(&home, "zai.TESTONLY.key0000", "absent");
        args["kind"] = json!("api-key");
        args["provider"] = json!("codex");
        assert_eq!(
            run("login-put", &args).unwrap_err().detail.as_str(),
            REFUSED_KIND
        );
        assert!(!home.path().join(".ccs/logins").exists());
    }

    /// Red/green: no answer except the dashboard-only `login-copy` ever carries the secret.
    #[test]
    fn no_answer_but_copy_carries_the_secret() {
        let home = home();
        let put = run("login-put", &put_args(&home, TOKEN, "absent")).unwrap();
        assert_eq!(put["written"], "created");
        let inventory = run(
            "login-inventory",
            &json!({"home": home.path().to_str().unwrap()}),
        )
        .unwrap();
        assert_eq!(inventory["logins"].as_array().unwrap().len(), 1);
        assert_eq!(
            inventory["logins"][0]["fingerprint"],
            credential::fingerprint(TOKEN)
        );
        for answer in [&put, &inventory] {
            let text = answer.to_string();
            assert!(!text.contains("TESTONLY"), "{text}");
            assert!(!text.contains("sk-ant"), "{text}");
        }
        let mut args = ident(&home);
        args["fingerprint"] = json!(credential::fingerprint(TOKEN));
        let copy = run("login-copy", &args).unwrap();
        assert_eq!(copy["secret"], TOKEN);
        let mut args = ident(&home);
        args["expect"] = json!(credential::fingerprint(TOKEN));
        let removed = run("login-remove", &args).unwrap();
        assert_eq!(removed["removed"], true);
        assert!(!removed.to_string().contains("TESTONLY"));
    }

    #[test]
    fn compare_before_replace_and_identity_guard() {
        let home = home();
        run("login-put", &put_args(&home, TOKEN, "absent")).unwrap();
        // Same delivery again (lost answer): fine. Another secret with `absent`: changed.
        run("login-put", &put_args(&home, TOKEN, "absent")).unwrap();
        let error = run("login-put", &put_args(&home, OTHER, "absent")).unwrap_err();
        assert_eq!(error.cause, Cause::ConfirmationInvalid);
        // Replace with the confirmed fingerprint.
        let fp = credential::fingerprint(TOKEN);
        let answer = run("login-put", &put_args(&home, OTHER, &fp)).unwrap();
        assert_eq!(answer["written"], "replaced");
        // The old fingerprint no longer matches.
        let error = run("login-put", &put_args(&home, TOKEN, &fp)).unwrap_err();
        assert_eq!(error.cause, Cause::ConfirmationInvalid);
        // Another account under the same id is an identity mismatch, for every verb.
        let mut args = put_args(&home, TOKEN, &credential::fingerprint(OTHER));
        args["account"] = json!("claude:someone-else");
        assert_eq!(
            run("login-put", &args).unwrap_err().cause,
            Cause::IdentityMismatch
        );
        let mut args = ident(&home);
        args["account"] = json!("claude:someone-else");
        args["fingerprint"] = json!(credential::fingerprint(OTHER));
        assert_eq!(
            run("login-copy", &args).unwrap_err().cause,
            Cause::IdentityMismatch
        );
        let mut args = ident(&home);
        args["expect"] = json!(fp);
        assert_eq!(
            run("login-remove", &args).unwrap_err().cause,
            Cause::ConfirmationInvalid
        );
        // Unknown keys are refused.
        let mut args = ident(&home);
        args["extra"] = json!("x");
        assert!(run("login-inventory", &args).is_err());
    }

    #[test]
    fn a_failure_never_carries_the_secret() {
        let home = home();
        run("login-put", &put_args(&home, TOKEN, "absent")).unwrap();
        let error = run("login-put", &put_args(&home, OTHER, "absent")).unwrap_err();
        let text = serde_json::to_string(&error).unwrap();
        assert!(!text.contains("TESTONLY"), "{text}");
    }
}
