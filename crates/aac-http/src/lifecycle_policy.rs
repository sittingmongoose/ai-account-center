//! Deterministic lifecycle decisions from guarded, non-secret facts.
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{self, Read};

const INPUT_MAX: u64 = 256 * 1024;

#[derive(Deserialize)]
#[serde(
    tag = "operation",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum Request {
    RegistryEdit {
        registry: Value,
        edit: Edit,
    },
    KeyProbe {
        status: String,
    },
    AddAdmission {
        multi_account: bool,
        count: usize,
        limit: usize,
    },
    OpenCursor {
        additional: bool,
        provider: String,
        request_platform: String,
        account_platform: Option<String>,
        host_configured: bool,
    },
    DueTrash {
        now: i64,
        entries: Vec<Deadline>,
    },
    OrphanKeys {
        now: i64,
        min_age: i64,
        named: Vec<String>,
        files: Vec<Deadline>,
    },
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum Edit {
    Label {
        id: String,
        label: Option<String>,
    },
    Insert {
        entry: Value,
        unique_provider: bool,
        limit: usize,
    },
    Remove {
        id: String,
    },
    Restore {
        entry: Value,
        index: usize,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Deadline {
    id: String,
    at: Option<i64>,
}

pub(crate) fn run() -> i32 {
    let mut bytes = Vec::new();
    if io::stdin()
        .take(INPUT_MAX + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > INPUT_MAX
    {
        return 74;
    }
    let Ok(request) = serde_json::from_slice::<Request>(&bytes) else {
        return 74;
    };
    let Some(result) = evaluate(request) else {
        return 74;
    };
    println!("{result}");
    0
}

fn evaluate(request: Request) -> Option<Value> {
    Some(match request {
        Request::RegistryEdit { registry, edit } => return edit_registry(registry, edit),
        Request::KeyProbe { status } => json!(match status.as_str() {
            "needs_sign_in" => "rejected",
            "ok" | "cached" => "ok",
            _ => "unverified",
        }),
        Request::AddAdmission {
            multi_account,
            count,
            limit,
        } => {
            if !multi_account && count != 0 {
                json!("single_account_provider")
            } else if count >= limit {
                json!("too_many_accounts")
            } else {
                Value::Null
            }
        }
        Request::OpenCursor {
            additional,
            provider,
            request_platform,
            account_platform,
            host_configured,
        } => {
            json!(
                additional
                    && provider == "cursor"
                    && request_platform == "mac"
                    && account_platform.as_deref() == Some("mac")
                    && host_configured
            )
        }
        Request::DueTrash { now, entries } => {
            json!(
                entries
                    .into_iter()
                    .filter(|entry| entry.at.is_some_and(|at| at <= now))
                    .map(|entry| entry.id)
                    .collect::<Vec<_>>()
            )
        }
        Request::OrphanKeys {
            now,
            min_age,
            named,
            files,
        } => {
            let named: std::collections::HashSet<_> = named.into_iter().collect();
            json!(
                files
                    .into_iter()
                    .filter(|file| !named.contains(&file.id)
                        && file.at.is_some_and(|at| now.saturating_sub(at) >= min_age))
                    .map(|file| file.id)
                    .collect::<Vec<_>>()
            )
        }
    })
}

fn edit_registry(mut registry: Value, edit: Edit) -> Option<Value> {
    if registry.get("version").and_then(Value::as_u64) != Some(2) {
        return None;
    }
    let accounts = registry.get_mut("accounts")?.as_array_mut()?;
    let position = match edit {
        Edit::Label { id, label } => {
            let Some(entry) = accounts
                .iter_mut()
                .find(|entry| entry["id"].as_str() == Some(id.as_str()))
            else {
                return Some(json!({"refusal":"missing"}));
            };
            entry.as_object_mut()?.insert("label".into(), json!(label));
            None
        }
        Edit::Insert {
            entry,
            unique_provider,
            limit,
        } => {
            if unique_provider
                && accounts
                    .iter()
                    .any(|row| row["provider"] == entry["provider"])
            {
                return Some(json!({"refusal":"single_account_provider"}));
            }
            if accounts.len() >= limit {
                return Some(json!({"refusal":"too_many_accounts"}));
            }
            accounts.push(entry);
            None
        }
        Edit::Remove { id } => {
            let Some(index) = accounts
                .iter()
                .position(|entry| entry["id"].as_str() == Some(id.as_str()))
            else {
                return Some(json!({"refusal":"missing"}));
            };
            accounts.remove(index);
            Some(index)
        }
        Edit::Restore { entry, index } => {
            if !accounts.iter().any(|row| row["id"] == entry["id"]) {
                accounts.insert(index.min(accounts.len()), entry);
            }
            None
        }
    };
    Some(json!({"registry":registry, "position":position}))
}
