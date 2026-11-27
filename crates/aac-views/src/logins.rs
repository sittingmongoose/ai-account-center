//! The Logins view (Central Logins step 1, `GET /api/logins`): every account x computer x tool with its kind, end,
//! renewer and health, built only from facts AAC already holds: the dashboard's account rows and the Claude renewal
//! runner's published report. Nothing is collected, refreshed or written here.
//!
//! Every value passes through an allowlist into [`aac_api::logins::LoginRow`], whose type has no field for a secret;
//! text that looks like a credential, a path or a link is dropped (`display_text`), so a token pasted into a label
//! cannot reach the page either.
use crate::antigravity::{display_text_value, email_value};
use aac_api::logins::{LOGINS_VIEW_VERSION, LoginCounts, LoginRow, LoginsView};
use serde_json::Value;

const DAY_MS: i64 = 86_400_000;
const HOUR_MS: i64 = 3_600_000;
/// A sign-in nobody renews by itself counts as ending soon this many days ahead.
const EXPIRING_DAYS: i64 = 7;
/// The Accounts page's order of providers.
const ORDER: [&str; 9] = [
    "claude",
    "codex",
    "antigravity",
    "muse",
    "cursor",
    "kimi-code",
    "qwen",
    "zai",
    "opencode-go",
];

fn text(value: Option<&Value>, max: usize) -> Option<String> {
    display_text_value(value, max)
}

/// A name word: letters, digits and `._ -`, up to 64 (computer and profile names).
fn name(value: Option<&Value>) -> Option<String> {
    let s = value?.as_str()?.trim();
    ((1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._- ".contains(&b)))
    .then(|| s.to_owned())
}

fn instant(value: Option<&Value>) -> Option<(String, i64)> {
    let s = value?.as_str()?;
    let ms = aac_num::ecma::parse_instant(s)?;
    Some((aac_num::ecma::to_iso_string(ms)?, ms))
}

/// The computer's label from the packaged registry (by key, machine id or known name), else its own name.
fn computer_label(host: &str) -> String {
    let registry = aac_status::computers::registry();
    let by_key = registry.get(host);
    let row = by_key
        .or_else(|| aac_status::computers::computer(host))
        .or_else(|| {
            aac_status::computers::named_computer(host).and_then(aac_status::computers::computer)
        });
    row.and_then(|r| r["label"].as_str())
        .map_or_else(|| host.to_owned(), str::to_owned)
}

fn days_word(days: i64) -> String {
    if days == 1 {
        "1 day".into()
    } else {
        format!("{days} days")
    }
}

/// "in 22 days", "in 5 hours", "ended 3 days ago".
fn relative(at: i64, now: i64) -> String {
    let delta = at - now;
    if delta >= DAY_MS {
        format!("in {}", days_word(delta / DAY_MS))
    } else if delta >= HOUR_MS {
        let h = delta / HOUR_MS;
        format!("in {h} hour{}", if h == 1 { "" } else { "s" })
    } else if delta > 0 {
        "within the hour".into()
    } else if -delta >= DAY_MS {
        format!("ended {} ago", days_word(-delta / DAY_MS))
    } else {
        "ended today".into()
    }
}

fn ago(at: i64, now: i64) -> String {
    let delta = (now - at).max(0);
    if delta >= DAY_MS {
        format!("{} ago", days_word(delta / DAY_MS))
    } else if delta >= HOUR_MS {
        let h = delta / HOUR_MS;
        format!("{h} hour{} ago", if h == 1 { "" } else { "s" })
    } else {
        "within the hour".into()
    }
}

/// Kind, kind label, tool and renewer for a provider read from the dashboard's rows.
fn provider_facts(provider: &str) -> (&'static str, &'static str, &'static str, &'static str) {
    match provider {
        "claude" => ("renewed", "Auto-renewed", "Claude Code", "AAC runner"),
        "codex" => ("tool", "Tool sign-in", "Codex", "Codex itself"),
        "antigravity" => ("tool", "Tool sign-in", "Antigravity", "Antigravity itself"),
        "muse" => ("tool", "Tool sign-in", "Muse", "Muse itself"),
        "cursor" => ("tool", "Tool sign-in", "Cursor app", "Cursor itself"),
        "qwen" => (
            "tool",
            "Browser session",
            "Qwen console",
            "You, in the browser",
        ),
        _ => ("static", "API key", "API key", "You, by hand"),
    }
}

/// A dashboard status word as health.
fn status_health(status: &str) -> (&'static str, &'static str) {
    match status {
        "ok" => ("ok", "Working"),
        "cached" => ("ok", "Working, cached"),
        "needs_sign_in" => ("needs_you", "Sign in again"),
        _ => ("unknown", "Not read"),
    }
}

fn severity(health: &str) -> u8 {
    match health {
        "needs_you" => 0,
        "ended" => 1,
        "expiring" => 2,
        "unknown" => 3,
        _ => 4,
    }
}

fn order(provider: &str) -> usize {
    ORDER
        .iter()
        .position(|p| *p == provider)
        .unwrap_or(ORDER.len())
}

/// One Claude sign-in from the renewal report.
fn renewal_row(
    row: &Value,
    needs: &[(String, String)],
    runner: &str,
    now: i64,
) -> Option<LoginRow> {
    let host = name(row.get("host"))?;
    let profile = name(row.get("profile"))?;
    let email = email_value(row.get("email"));
    let state = row.get("state")?.as_str()?;
    let auto = row.get("autoRenew") != Some(&Value::Bool(false));
    let ends = instant(row.get("signInEndsAt"));
    let last = instant(row.get("lastRenewAt"));
    let need = email
        .as_ref()
        .and_then(|e| needs.iter().find(|(who, _)| who == e).map(|(_, r)| r));
    let (health, health_text): (&str, String) = match state {
        "logged_out" => ("needs_you", "Signed out".into()),
        "no_refresh_token" => ("needs_you", "Needs sign-in".into()),
        "not_signed_in" => ("needs_you", "Not signed in".into()),
        "unreadable" => ("unknown", "Unreadable".into()),
        "no_end_date" => ("unknown", "No end date".into()),
        _ => match &ends {
            Some((_, at)) if *at <= now => ("ended", "Ended".into()),
            Some((_, at)) if !auto && *at - now <= EXPIRING_DAYS * DAY_MS => {
                ("expiring", "Ends soon".into())
            }
            _ if auto => ("ok", "Renews itself".into()),
            _ => ("ok", "Working".into()),
        },
    };
    let (health, health_text) = match need {
        Some(reason) if health != "needs_you" => (
            "needs_you",
            if reason == "captcha" {
                format!("CAPTCHA on {}", computer_label(runner))
            } else {
                "Needs you".into()
            },
        ),
        _ => (health, health_text),
    };
    let account = email.unwrap_or_else(|| profile.clone());
    Some(LoginRow {
        id: format!("claude:{host}:{profile}"),
        provider: "claude".into(),
        provider_label: "Claude".into(),
        account,
        computer: computer_label(&host),
        tool: format!("Claude Code · {profile}"),
        kind: if auto { "renewed" } else { "tool" }.into(),
        kind_label: if auto { "Auto-renewed" } else { "Tool sign-in" }.into(),
        ends_text: ends
            .as_ref()
            .map_or_else(|| "Not known".into(), |(_, at)| relative(*at, now)),
        ends_at: ends.map(|(iso, _)| iso),
        renewer: if auto {
            format!("AAC runner on {}", computer_label(runner))
        } else {
            "You".into()
        },
        health: health.into(),
        health_text,
        last_renewal: last.map(|(_, at)| ago(at, now)).unwrap_or_default(),
    })
}

/// One account from the dashboard's rows.
fn account_row(account: &Value) -> Option<LoginRow> {
    let provider = name(account.get("provider"))?.to_ascii_lowercase();
    let (kind, kind_label, tool, renewer) = provider_facts(&provider);
    let host = name(account.get("host")).or_else(|| name(account.get("platform")));
    let computer = host
        .as_deref()
        .map_or_else(|| "Not known".into(), computer_label);
    let label = text(account.get("label"), 80);
    let account_text = email_value(account.get("email"))
        .or_else(|| label.clone())
        .unwrap_or_else(|| "Unnamed account".into());
    let profile = name(account.get("profile"));
    let provider_label = text(account.get("providerLabel"), 40).unwrap_or_else(|| provider.clone());
    let tool = match (&provider[..], &profile) {
        ("claude", Some(p)) => format!("{tool} · {p}"),
        _ if kind == "static" => format!("{provider_label} {tool}"),
        _ => tool.to_owned(),
    };
    let status = account.get("status").and_then(Value::as_str).unwrap_or("");
    let (health, health_text) = status_health(status);
    let id_part = text(account.get("id"), 128).unwrap_or_else(|| account_text.clone());
    Some(LoginRow {
        id: format!("{provider}:{}:{id_part}", host.as_deref().unwrap_or("-")),
        provider_label,
        provider,
        account: account_text,
        computer,
        tool,
        kind: kind.into(),
        kind_label: kind_label.into(),
        ends_at: None,
        ends_text: if kind == "static" {
            "No set end".into()
        } else {
            "Not published".into()
        },
        renewer: renewer.into(),
        health: health.into(),
        health_text: health_text.into(),
        last_renewal: String::new(),
    })
}

/// Build the view. `accounts` are the dashboard's merged account rows; `renewal` is the Claude renewal runner's
/// report as `ClaudeSignInRenewal::to_json` writes it, when one is published.
pub fn compose(accounts: &[Value], renewal: Option<&Value>, now: i64) -> LoginsView {
    let mut rows = Vec::new();
    let mut notes = Vec::new();
    let mut claude_emails: Vec<String> = Vec::new();
    match renewal {
        Some(report) => {
            let runner = name(report.get("runner")).unwrap_or_else(|| "unknown".into());
            let needs: Vec<(String, String)> = report["needsYou"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|n| {
                    Some((
                        email_value(n.get("email"))?,
                        name(n.get("reason")).unwrap_or_default(),
                    ))
                })
                .collect();
            for row in report["signIns"].as_array().into_iter().flatten() {
                if let Some(login) = renewal_row(row, &needs, &runner, now) {
                    claude_emails.push(login.account.clone());
                    rows.push(login);
                }
            }
            if report["stale"] == Value::Bool(true) {
                let when = instant(report.get("generatedAt"))
                    .map(|(_, at)| format!(" (written {})", ago(at, now)))
                    .unwrap_or_default();
                notes.push(format!(
                    "The Claude renewal report is old{when}: Claude rows may be out of date."
                ));
            }
        }
        None => notes.push(
            "No Claude renewal report is published: Claude rows come from usage readings, without end dates.".into(),
        ),
    }
    for account in accounts {
        if account.get("hidden") == Some(&Value::Bool(true)) {
            continue;
        }
        let Some(login) = account_row(account) else {
            continue;
        };
        // Claude accounts the renewal report covers are listed per computer from it already.
        if login.provider == "claude" && claude_emails.contains(&login.account) {
            continue;
        }
        if !rows.iter().any(|r: &LoginRow| r.id == login.id) {
            rows.push(login);
        }
    }
    rows.sort_by(|a, b| {
        (
            severity(&a.health),
            order(&a.provider),
            &a.account,
            &a.computer,
            &a.tool,
        )
            .cmp(&(
                severity(&b.health),
                order(&b.provider),
                &b.account,
                &b.computer,
                &b.tool,
            ))
    });
    let counts = LoginCounts {
        total: rows.len() as u32,
        needs_you: rows
            .iter()
            .filter(|r| matches!(&r.health[..], "needs_you" | "ended"))
            .count() as u32,
        expiring: rows.iter().filter(|r| r.health == "expiring").count() as u32,
    };
    LoginsView {
        version: LOGINS_VIEW_VERSION,
        generated_at: aac_num::ecma::to_iso_string(now).unwrap_or_default(),
        rows,
        counts,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_795_000_000_000; // 2026-11-18

    fn iso(ms: i64) -> String {
        aac_num::ecma::to_iso_string(ms).expect("test value")
    }

    #[test]
    fn a_renewed_claude_sign_in_reads_its_end_and_renewer_per_computer() {
        let report = json!({
            "runner": "mac", "generatedAt": iso(NOW), "stale": false, "needsYou": [],
            "signIns": [{"host": "1070ti", "profile": "gmail", "email": "a@example.com", "state": "ok",
                         "signInEndsAt": iso(NOW + 22 * DAY_MS + 5), "autoRenew": true,
                         "lastRenewAt": iso(NOW - 3 * DAY_MS)}]
        });
        let view = compose(&[], Some(&report), NOW);
        assert_eq!(view.rows.len(), 1);
        let row = &view.rows[0];
        assert_eq!(row.computer, "1070ti");
        assert_eq!(row.tool, "Claude Code · gmail");
        assert_eq!(row.kind, "renewed");
        assert_eq!(row.ends_text, "in 22 days");
        assert_eq!(row.renewer, "AAC runner on Mac");
        assert_eq!(row.health, "ok");
        assert_eq!(row.last_renewal, "3 days ago");
        assert!(view.notes.is_empty());
    }

    #[test]
    fn a_captcha_wait_and_an_ended_sign_in_sort_first_and_count() {
        let report = json!({
            "runner": "mac", "generatedAt": iso(NOW - 5 * HOUR_MS), "stale": true,
            "needsYou": [{"email": "b@example.com", "reason": "captcha", "how": "x", "since": iso(NOW)}],
            "signIns": [
                {"host": "mac", "profile": "default", "email": "a@example.com", "state": "ok",
                 "signInEndsAt": iso(NOW + 30 * DAY_MS)},
                {"host": "mac", "profile": "me", "email": "b@example.com", "state": "ok",
                 "signInEndsAt": iso(NOW + 30 * DAY_MS)},
                {"host": "alexis", "profile": "party", "email": "c@example.com", "state": "ok",
                 "signInEndsAt": iso(NOW - DAY_MS - 1), "autoRenew": false}
            ]
        });
        let view = compose(&[], Some(&report), NOW);
        let health: Vec<&str> = view.rows.iter().map(|r| r.health.as_str()).collect();
        assert_eq!(health, ["needs_you", "ended", "ok"]);
        assert_eq!(view.rows[0].health_text, "CAPTCHA on Mac");
        assert_eq!(view.rows[1].computer, "Alexis PC");
        assert_eq!(view.rows[1].ends_text, "ended 1 day ago");
        assert_eq!(view.counts.needs_you, 2);
        assert_eq!(view.notes.len(), 1);
        assert!(view.notes[0].contains("written 5 hours ago"));
    }

    #[test]
    fn dashboard_accounts_read_their_kind_and_health_and_hidden_ones_stay_out() {
        let accounts = [
            json!({"id": "codex:1", "provider": "codex", "providerLabel": "Codex", "email": "a@example.com",
                   "host": "ubuntu", "status": "needs_sign_in"}),
            json!({"id": "zai:1", "provider": "zai", "providerLabel": "Z.ai", "label": "Main key",
                   "platform": "ubuntu", "status": "ok"}),
            json!({"id": "kimi:1", "provider": "kimi-code", "email": "h@example.com", "hidden": true, "status": "ok"}),
        ];
        let view = compose(&accounts, None, NOW);
        assert_eq!(view.rows.len(), 2);
        assert_eq!(view.rows[0].provider, "codex");
        assert_eq!(view.rows[0].health, "needs_you");
        assert_eq!(view.rows[0].computer, "Ubuntu");
        assert_eq!(view.rows[0].ends_text, "Not published");
        assert_eq!(view.rows[1].kind, "static");
        assert_eq!(view.rows[1].account, "Main key");
        assert_eq!(view.rows[1].tool, "Z.ai API key");
        assert_eq!(view.rows[1].ends_text, "No set end");
        assert_eq!(view.notes.len(), 1, "a missing renewal report is said");
    }

    #[test]
    fn a_claude_account_the_report_covers_is_not_listed_twice() {
        let report = json!({"runner": "mac", "generatedAt": iso(NOW), "stale": false, "needsYou": [],
            "signIns": [{"host": "mac", "profile": "me", "email": "a@example.com", "state": "ok"}]});
        let accounts = [
            json!({"id": "claude:me", "provider": "claude", "email": "a@example.com", "host": "mac", "status": "ok"}),
            json!({"id": "claude:x", "provider": "claude", "email": "z@example.com", "host": "mac", "status": "ok"}),
        ];
        let view = compose(&accounts, Some(&report), NOW);
        assert_eq!(view.rows.len(), 2);
        assert_eq!(
            view.rows
                .iter()
                .filter(|r| r.account == "a@example.com")
                .count(),
            1
        );
    }

    #[test]
    fn no_secret_in_any_input_reaches_the_view() {
        let token = "sk-ant-oat01-SECRETSECRETSECRET";
        let jwt = "eyJhbGciOiJIUzI1NiJ9.SECRETPAYLOAD";
        let report = json!({"runner": "mac", "generatedAt": iso(NOW), "stale": false,
            "needsYou": [{"email": "a@example.com", "reason": "captcha", "how": format!("paste {token}")}],
            "signIns": [{"host": "mac", "profile": "me", "email": "a@example.com", "state": "ok",
                         "accessToken": token, "refreshToken": jwt, "configDir": "/home/u/.claude-me",
                         "fixCommand": "claude login --token SECRETCMD"}]});
        let accounts = [
            json!({"id": "codex:1", "provider": "codex", "email": "b@example.com", "host": "ubuntu", "status": "ok",
                   "access_token": token, "tokens": {"id_token": jwt}, "authFile": "/home/u/.codex/auth.json",
                   "cookie": "llama_dev_sess=SECRETCOOKIE"}),
            json!({"id": "zai:1", "provider": "zai", "label": token, "host": "ubuntu", "status": "ok",
                   "apiKey": "SECRETKEY"}),
            json!({"id": "qwen:1", "provider": "qwen", "label": jwt, "email": format!("Bearer {token}"),
                   "host": "windows", "status": "ok"}),
        ];
        let view = compose(&accounts, Some(&report), NOW);
        assert_eq!(view.rows.len(), 4);
        let out = serde_json::to_string(&view).expect("test value");
        for leak in [
            "SECRET",
            "sk-ant",
            "eyJ",
            "/home/",
            ".claude",
            "auth.json",
            "Bearer",
            "cookie",
            "llama_dev_sess",
        ] {
            assert!(!out.contains(leak), "{leak} leaked: {out}");
        }
        let keys: std::collections::BTreeSet<String> = view
            .rows
            .iter()
            .flat_map(|r| {
                serde_json::to_value(r)
                    .expect("test value")
                    .as_object()
                    .expect("test value")
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .collect();
        let allowed = [
            "id",
            "provider",
            "providerLabel",
            "account",
            "computer",
            "tool",
            "kind",
            "kindLabel",
            "endsAt",
            "endsText",
            "renewer",
            "health",
            "healthText",
            "lastRenewal",
        ];
        assert!(
            keys.iter().all(|k| allowed.contains(&k.as_str())),
            "{keys:?}"
        );
    }
}
