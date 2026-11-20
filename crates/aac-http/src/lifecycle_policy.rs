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
    Recheck {
        now: i64,
        last: Option<i64>,
        interval: i64,
    },
    Guide {
        kind: String,
        provider: String,
        platform: Option<String>,
        key_provider: bool,
    },
    DueTrash {
        now: i64,
        entries: Vec<Deadline>,
    },
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(crate) enum Edit {
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

/// The private compatibility command used by the existing lifecycle adapters.
#[doc(hidden)]
pub fn run() -> i32 {
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
        Request::Recheck {
            now,
            last,
            interval,
        } => {
            let retry = last.and_then(|last| {
                let remaining = interval.saturating_sub(now.saturating_sub(last));
                (remaining > 0).then(|| 1 + (remaining - 1) / 1000)
            });
            json!(retry)
        }
        Request::Guide {
            kind,
            provider,
            platform,
            key_provider,
        } => match (kind.as_str(), provider.as_str()) {
            ("claude", _) => json!({"guide":{"kind":"open-app","platforms":["mac","windows"]}}),
            ("wallet", _) => json!({"guide":{"kind":"browser-extension","platform":"mac"}}),
            ("additional", "cursor") => {
                let platforms: Vec<_> = platform
                    .filter(|platform| platform != "ubuntu")
                    .into_iter()
                    .collect();
                json!({"guide":{"kind":"open-app","platforms":platforms}})
            }
            ("additional", "qwen") => {
                let platform = if platform.as_deref() == Some("mac") {
                    "mac"
                } else {
                    "windows"
                };
                json!({"guide":{"kind":"browser-extension","platform":platform}})
            }
            ("additional", _) if key_provider => json!({"refusal":"use_replace_key"}),
            _ => json!({"refusal":"not_implemented"}),
        },
        Request::DueTrash { now, entries } => {
            json!(
                entries
                    .into_iter()
                    .filter(|entry| entry.at.is_some_and(|at| at <= now))
                    .map(|entry| entry.id)
                    .collect::<Vec<_>>()
            )
        }
    })
}

pub(crate) fn edit_registry(mut registry: Value, edit: Edit) -> Option<Value> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_and_rollback_keep_registry_order() {
        let row = json!({"id":"zai:acct:00000002","provider":"zai"});
        let original = json!({"version":2,"accounts":[
            {"id":"zai:acct:00000001","provider":"zai"},
            row,
            {"id":"cursor:usage","provider":"cursor"}
        ]});
        let removed = edit_registry(
            original.clone(),
            Edit::Remove {
                id: "zai:acct:00000002".into(),
            },
        );
        let Some(removed) = removed else {
            panic!("missing registry decision")
        };
        let index = removed["position"].as_u64();
        assert_eq!(index, Some(1));
        let restored = edit_registry(
            removed["registry"].clone(),
            Edit::Restore {
                entry: row,
                index: 1,
            },
        );
        assert_eq!(
            restored.map(|value| value["registry"].clone()),
            Some(original)
        );
    }

    #[test]
    fn recheck_allows_exactly_at_the_boundary() {
        for (now, expected) in [(9_999, json!(1)), (10_000, Value::Null)] {
            assert_eq!(
                evaluate(Request::Recheck {
                    now,
                    last: Some(0),
                    interval: 10_000
                }),
                Some(expected)
            );
        }
    }
}
