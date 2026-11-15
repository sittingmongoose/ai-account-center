//! GET /api/accounts/registry: the port of `composeRegistryListing`
//! (`src/web-server/services/account-lifecycle-listing.ts`) with `accountView`, `entryView`,
//! `credentialView` and `keyActions`. Every owner fact (dashboard rows, removal refusals,
//! sign-in availability, key metadata, jobs, trash) was read by TS; nothing is read here.
use crate::spread::{Admit, Refuse, Spread, req, truthy};
use serde_json::{Map, Value, json};

type Object = Map<String, Value>;

fn boolean(object: &Object, key: &str) -> Admit<bool> {
    req(object.get(key).and_then(Value::as_bool))
}

/// `value === null`; an absent member is undefined, not null.
fn is_null(object: &Object, key: &str) -> bool {
    matches!(object.get(key), Some(Value::Null))
}

/// A defined, non-null member: the left side of `??` that is kept.
fn present<'a>(object: Option<&'a Object>, key: &str) -> Option<&'a Value> {
    object
        .and_then(|o| o.get(key))
        .filter(|value| **value != Value::Null)
}

fn equals(object: &Object, key: &str, word: &str) -> bool {
    object.get(key).and_then(Value::as_str) == Some(word)
}

fn actions(
    sign_in_again: bool,
    replace_key: bool,
    remove: bool,
    open: Value,
    recheck: bool,
) -> Value {
    json!({
        "signInAgain": sign_in_again,
        "replaceKey": replace_key,
        "remove": remove,
        "open": open,
        "recheck": recheck,
    })
}

/// `accountView(row, credential, actions, removeRefusal)`; an absent row member stays absent.
fn account_view(
    row: &Spread,
    credential: Value,
    actions: Value,
    remove_refusal: Option<Value>,
) -> Spread {
    let mut view = Spread::default();
    for key in ["id", "provider", "label", "email", "platform"] {
        view.set(key, row.get(key).cloned());
    }
    view.set("credential", Some(credential));
    view.set(
        "hidden",
        Some(json!(matches!(row.get("hidden"), Some(Value::Bool(true))))),
    );
    view.set(
        "trayHidden",
        Some(json!(matches!(
            row.get("trayHidden"),
            Some(Value::Bool(true))
        ))),
    );
    view.set(
        "lifecycle",
        Some(match row.get("lifecycle") {
            None | Some(Value::Null) => json!({"state": "ready", "jobId": null}),
            Some(value) => value.clone(),
        }),
    );
    view.set("actions", Some(actions));
    view.set("removeRefusal", remove_refusal);
    view
}

/// `providers.find((entry) => entry.id === provider)?.capabilities.replaceKey === true`.
fn can_replace(providers: &[Value], provider: Option<&Value>) -> Admit<bool> {
    for entry in providers {
        let id = match entry {
            Value::Null => return Err(Refuse),
            Value::Object(entry) => entry.get("id"),
            _ => None,
        };
        let found = match (id, provider) {
            (Some(Value::String(id)), Some(Value::String(provider))) => id == provider,
            _ => false,
        };
        if found {
            return match entry.as_object().and_then(|e| e.get("capabilities")) {
                None | Some(Value::Null) => Err(Refuse),
                Some(Value::Object(capabilities)) => Ok(matches!(
                    capabilities.get("replaceKey"),
                    Some(Value::Bool(true))
                )),
                Some(_) => Ok(false),
            };
        }
    }
    Ok(false)
}

fn entry_view(fact: &Object, row: &Object, providers: &[Value]) -> Admit<Spread> {
    let entry = req(fact.get("entry").and_then(Value::as_object))?;
    let kind = req(fact.get("credentialKind").and_then(Value::as_str))?;
    let provider = entry.get("provider");
    let mut source = Spread::default();
    source.set("id", entry.get("id").cloned());
    source.set("provider", provider.cloned());
    // `entry.label ?? row.label ?? entry.provider`
    source.set(
        "label",
        present(Some(entry), "label")
            .or(present(Some(row), "label"))
            .or(provider)
            .cloned(),
    );
    source.set(
        "email",
        Some(match row.get("email") {
            None | Some(Value::Null) => Value::Null,
            Some(email) => email.clone(),
        }),
    );
    source.set("platform", entry.get("platform").cloned());
    source.set("hidden", row.get("hidden").cloned());
    source.set("trayHidden", row.get("trayHidden").cloned());
    source.set("lifecycle", row.get("lifecycle").cloned());
    let key = match fact.get("key") {
        None | Some(Value::Null) => None,
        Some(Value::Object(key)) => Some(key),
        Some(_) => return Err(Refuse),
    };
    let or_null = |name: &str| present(key, name).cloned().unwrap_or(Value::Null);
    let credential = if kind == "aac-key" {
        let mut credential = Spread::default();
        credential.set("kind", Some(json!("aac-key")));
        credential.set("last4", Some(or_null("last4")));
        credential.set("fingerprint", Some(or_null("fingerprint")));
        // `key?.storedOn ?? entry.platform`; an absent platform stays absent.
        credential.set(
            "storedOn",
            present(key, "storedOn").or(entry.get("platform")).cloned(),
        );
        credential.into_value()
    } else {
        json!({ "kind": kind })
    };
    let is_cursor = equals(entry, "provider", "cursor");
    let ssh_not_null = !matches!(entry.get("sshHost"), Some(Value::Null));
    let replace = can_replace(providers, provider)?;
    let actions = actions(
        is_cursor || equals(entry, "provider", "qwen"),
        replace && kind == "aac-key",
        true,
        if is_cursor && equals(entry, "platform", "mac") && ssh_not_null {
            json!(["mac"])
        } else {
            json!([])
        },
        true,
    );
    let mut view = account_view(&source, credential, actions, Some(Value::Null));
    if equals(entry, "provider", "muse") {
        let sign_in = matches!(fact.get("museSignIn"), Some(Value::Bool(true)));
        if let Some(Value::Object(mut actions)) = view.get("actions").cloned() {
            actions.insert("signInAgain".into(), json!(sign_in));
            view.set("actions", Some(Value::Object(actions)));
        }
    }
    let running = boolean(fact, "running")?;
    view.set(
        "removeRefusal",
        Some(if running {
            json!("signin_running")
        } else {
            Value::Null
        }),
    );
    Ok(view)
}

fn row_view(fact: &Value, providers: &[Value]) -> Admit<Value> {
    let fact = req(fact.as_object())?;
    let row_object = req(fact.get("row").and_then(Value::as_object))?;
    let mut row = Spread::default();
    for (key, value) in row_object {
        row.set(key, Some(value.clone()));
    }
    let row = &row;
    let none = || json!([]);
    let view = match req(fact.get("kind").and_then(Value::as_str))? {
        "codex" => {
            // The live login signs in again in place; account_active is Remove's refusal only.
            let available = boolean(fact, "available")?;
            let unavailable = equals(fact, "refusal", "unavailable");
            account_view(
                row,
                Value::Null,
                actions(
                    available && !unavailable,
                    false,
                    !unavailable,
                    none(),
                    false,
                ),
                fact.get("refusal").cloned(),
            )
        }
        "claude" => {
            let platforms = req(fact.get("claudePlatforms").and_then(Value::as_array))?.clone();
            let profile = match fact.get("profile") {
                None | Some(Value::Null) => None,
                Some(Value::Object(profile)) => Some(profile),
                Some(_) => return Err(Refuse),
            };
            let protected =
                profile.is_some_and(|p| truthy(p.get("isDefault")) && !truthy(p.get("email")));
            account_view(
                row,
                Value::Null,
                actions(
                    true,
                    false,
                    boolean(fact, "removeAllowed")? && profile.is_some(),
                    Value::Array(platforms),
                    false,
                ),
                Some(if protected {
                    json!("account_protected")
                } else {
                    Value::Null
                }),
            )
        }
        "antigravity" => {
            let reason_blocks =
                equals(fact, "reason", "tool_missing") || equals(fact, "reason", "not_implemented");
            let refusal_null = is_null(fact, "refusal");
            account_view(
                row,
                Value::Null,
                actions(
                    !reason_blocks && refusal_null,
                    false,
                    boolean(fact, "removeAllowed")? && !equals(fact, "refusal", "unavailable"),
                    none(),
                    false,
                ),
                fact.get("refusal").cloned(),
            )
        }
        "entry" => entry_view(fact, row_object, providers)?,
        "other" => {
            let id = req(row.get("id").and_then(Value::as_str))?;
            let wallet = id.starts_with("plan-opencode-go-console-");
            let running = boolean(fact, "running")?;
            account_view(
                row,
                Value::Null,
                actions(wallet, false, boolean(fact, "walletRemove")?, none(), false),
                Some(if wallet && running {
                    json!("signin_running")
                } else {
                    Value::Null
                }),
            )
        }
        _ => return Err(Refuse),
    };
    Ok(view.into_value())
}

/// The listing body, or a refusal (TS then composes it).
pub fn compose(input: &Value) -> Admit<Value> {
    let input = req(input.as_object())?;
    let providers = req(input.get("providers").and_then(Value::as_array))?;
    let rows = req(input.get("rows").and_then(Value::as_array))?;
    let accounts = rows
        .iter()
        .map(|fact| row_view(fact, providers))
        .collect::<Admit<Vec<_>>>()?;
    let list = |key: &str| req(input.get(key).filter(|v| v.is_array()).cloned());
    Ok(json!({
        "providers": providers,
        "accounts": accounts,
        "jobs": list("jobs")?,
        "trash": list("trash")?,
    }))
}
