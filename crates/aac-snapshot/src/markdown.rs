//! The rendering of `usage.md` (`agent-usage-markdown.ts`): pure text production from the
//! snapshot value, with the publish-time `signInNow` as its only clock input. The rendered bytes
//! are a live external contract other agents read every minute, pinned by the golden test.

use crate::js::{self, amount, member, span, utc};
use crate::projection::{ProjectedAccount, host_name};
use serde_json::{Value, json};

fn status_words(status: &str) -> &'static str {
    match status {
        "ok" => "current",
        "cached" => "cached earlier reading",
        "unavailable" => "no current reading",
        "error" => "reading failed",
        "needs_sign_in" => "needs sign-in",
        _ => "no current reading",
    }
}

fn cache_reason_words(reason: &str) -> &'static str {
    match reason {
        "read_failed" => "last provider read failed or was skipped",
        "rate_limited" => "provider refresh is rate limited",
        "refresh_pending" => "next provider read is pending",
        "source_changed" => "saved sign-in changed; next provider read is pending",
        _ => "last provider read outcome was not recorded",
    }
}

/// `burnText`: the pace in words, or null when there is no pace.
fn burn_text(burn: &Value, now: i64, used_percent: Option<f64>) -> Option<String> {
    if burn.is_null() {
        return None;
    }
    let pace = member(burn, "pace").as_f64()?;
    let pace_minutes = member(burn, "paceMinutes").as_f64()? as i64;
    let over = if pace_minutes < 60 {
        format!("{pace_minutes} min")
    } else {
        format!(
            "{} h",
            js::amount(aac_num::ecma::math_round(pace_minutes as f64 / 60.0))
        )
    };
    let last_hour = member(burn, "lastHour").as_f64();
    let hour = if pace_minutes < 60 && last_hour.is_some() {
        format!("; {}%/h over 1 h", amount(last_hour.unwrap_or(0.0)))
    } else {
        String::new()
    };
    let pace_text = format!("{}%/h over the last {over}{hour}", amount(pace));
    if pace < 0.0 {
        return Some(format!("usage falling {pace_text}"));
    }
    let projected = member(burn, "projectedFullAt");
    if projected.is_null() {
        return Some(format!("burning {pace_text}"));
    }
    let projected_at = js::date_parse(projected)?;
    let runs_out = member(burn, "runsOutBeforeReset");
    if projected_at <= now {
        if used_percent.is_some_and(|used| used >= 100.0) {
            let until = if runs_out == &json!(true) {
                " until the reset"
            } else {
                ""
            };
            return Some(format!("at its limit{until} ({pace_text})"));
        }
        return Some(format!(
            "reaching its limit about now at this pace ({pace_text})"
        ));
    }
    let full = format!("full in {}", span((projected_at - now) as f64 / 1000.0));
    let reset = if runs_out == &json!(true) {
        " (before the reset)"
    } else if runs_out == &json!(false) {
        " (after the reset)"
    } else {
        ""
    };
    Some(format!("burning {pace_text}, {full}{reset}"))
}

/// `windowText`: one window's line.
fn window_text(window: &Value, now: i64) -> String {
    let mut parts: Vec<String> = Vec::new();
    let unlimited = member(window, "unlimited");
    let used_percent = member(window, "usedPercent").as_f64();
    if unlimited == &json!(true) {
        parts.push("unlimited".into());
    } else if let Some(used) = used_percent {
        let remaining_percent = member(window, "remainingPercent").as_f64();
        let left = remaining_percent
            .map(|left| format!(", {}% left", amount(left)))
            .unwrap_or_default();
        parts.push(format!("{}% used{left}", amount(used)));
    }
    let unit = member(window, "unit")
        .as_str()
        .filter(|u| !u.is_empty())
        .map(|u| format!(" {u}"))
        .unwrap_or_default();
    let remaining = member(window, "remaining").as_f64();
    let limit = member(window, "limit").as_f64();
    let used = member(window, "used").as_f64();
    if let (Some(remaining), Some(limit)) = (remaining, limit) {
        parts.push(format!(
            "{} of {}{unit} left",
            amount(remaining),
            amount(limit)
        ));
    } else if let Some(remaining) = remaining {
        parts.push(format!("{}{unit} left", amount(remaining)));
    } else if let (Some(used), Some(limit)) = (used, limit) {
        parts.push(format!("{} of {}{unit} used", amount(used), amount(limit)));
    } else if let Some(used) = used {
        parts.push(format!("{}{unit} used", amount(used)));
    }
    let reset_at = member(window, "resetAt");
    if let Some(reset) = reset_at.as_str().filter(|s| !s.is_empty()) {
        let reset_passed = member(window, "resetPassed") == &json!(true);
        if reset_passed {
            parts.push(format!(
                "reset at {} already passed, new reading pending",
                utc(reset)
            ));
        } else {
            let at = js::date_parse_str(reset)
                .map(|v| v as f64)
                .unwrap_or(f64::NAN);
            parts.push(format!(
                "resets {} (in {})",
                utc(reset),
                span((at - now as f64) / 1000.0)
            ));
        }
    }
    if let Some(expires) = member(window, "expiresAt")
        .as_str()
        .filter(|s| !s.is_empty())
    {
        parts.push(format!("expires {}", utc(expires)));
    }
    if member(window, "resetPassed") != &json!(true) {
        if let Some(burn) = burn_text(member(window, "burn"), now, used_percent) {
            parts.push(burn);
        }
    }
    if parts.is_empty() {
        parts.push("no number reported".into());
    }
    if member(window, "cached") == &json!(true) {
        let sampled = member(window, "sampledAt")
            .as_str()
            .filter(|s| !s.is_empty());
        if let Some(sampled) = sampled {
            let at = js::date_parse_str(sampled)
                .map(|v| v as f64)
                .unwrap_or(f64::NAN);
            parts.push(format!(
                "earlier reading from {} ago",
                span((now as f64 - at) / 1000.0)
            ));
        } else {
            parts.push("earlier reading, time unknown".into());
        }
    }
    let pool = member(window, "pool")
        .as_str()
        .filter(|p| !p.is_empty())
        .map(|p| format!(" [{p}]"))
        .unwrap_or_default();
    format!(
        "{}{pool}: {}",
        member(window, "label").as_str().unwrap_or(""),
        parts.join(", ")
    )
}

/// The sign-in state words.
fn signin_state_words(state: &str) -> Option<&'static str> {
    match state {
        "ok" => Some("healthy"),
        "logged_out" => Some("already logged out"),
        "unavailable" => Some("not readable on this machine"),
        "unsupported" => Some("no expiry published"),
        _ => None,
    }
}

fn signin_rank(state: &str) -> i32 {
    match state {
        "logged_out" => 0,
        "critical" => 1,
        "warn" => 2,
        "unavailable" => 3,
        "unsupported" => 4,
        "ok" => 5,
        _ => 9,
    }
}

/// Whether a repair command carries an unpaired surrogate. A Rust string holds valid UTF-8, so
/// this is always false here; the TypeScript renderer's last-line screen is kept as the named
/// rule, and the JSON boundary (serde_json) refuses lone surrogates before they reach this text.
fn unpaired_surrogate(_text: &str) -> bool {
    false
}

/// `renderSignInMarkdown`: the sign-in section, worst state first.
fn render_sign_in_markdown(profiles: &[Value], now: i64) -> Vec<String> {
    let mut rows: Vec<Value> = profiles
        .iter()
        .map(|row| {
            let mut copy = row.clone();
            let machine = member(row, "machine")
                .as_str()
                .or_else(|| member(row, "platform").as_str())
                .unwrap_or("unknown");
            copy["machine"] = json!(machine);
            let fix = member(row, "fixCommand").as_str().unwrap_or("");
            if unpaired_surrogate(fix) {
                copy["fixCommand"] = json!("");
            }
            copy
        })
        .collect();
    rows.sort_by_key(|row| signin_rank(member(row, "state").as_str().unwrap_or("")));
    let mut lines = Vec::new();
    for row in &rows {
        let state = member(row, "state").as_str().unwrap_or("");
        let logged_out_at = member(row, "loggedOutAt");
        let ended_at = js::date_parse(logged_out_at);
        let names_end = ended_at.is_some() && state != "logged_out";
        let live = state == "ok" || state == "warn" || state == "critical";
        let refresh_at = if logged_out_at.is_null() || logged_out_at == &json!("") {
            js::date_parse(member(row, "refreshExpiresAt"))
        } else {
            None
        };
        let crossed_end = live && ended_at.is_some_and(|at| at <= now);
        let crossed_refresh = live && refresh_at.is_some_and(|at| at <= now);
        let mut word = signin_state_words(state)
            .map(str::to_owned)
            .unwrap_or_else(|| state.to_owned());
        if crossed_end {
            word = format!(
                "ended {} (past its deadline; awaiting recheck)",
                utc(logged_out_at.as_str().unwrap_or(""))
            );
        } else if crossed_refresh {
            word = format!(
                "refresh expired {} (awaiting recheck)",
                utc(member(row, "refreshExpiresAt").as_str().unwrap_or(""))
            );
        } else if state == "warn" || state == "critical" {
            if names_end {
                word = format!("ends {}", utc(logged_out_at.as_str().unwrap_or("")));
            } else if let Some(refresh) = member(row, "refreshExpiresAt")
                .as_str()
                .filter(|s| !s.is_empty())
            {
                word = format!("refresh expires {}", utc(refresh));
            } else if member(row, "accessExpiresAt").is_null()
                || member(row, "accessExpiresAt") == &json!("")
            {
                word = "no access token in the store; the next run renews".into();
            } else {
                word = "exact end unknown".into();
            }
        }
        let fix_command = member(row, "fixCommand").as_str().unwrap_or("");
        let auto_renew = member(row, "autoRenew") == &json!(true);
        let access = member(row, "accessExpiresAt");
        let access_empty = access.is_null() || access == &json!("");
        let detail = if state == "logged_out" && auto_renew {
            "logged out; the renewal runner signs it in again on its next run".to_owned()
        } else if state == "logged_out" {
            let ended = if !logged_out_at.is_null() && ended_at.is_some_and(|at| at <= now) {
                format!(
                    "sign-in ended {}",
                    utc(logged_out_at.as_str().unwrap_or(""))
                )
            } else {
                "tokens are gone or expired".to_owned()
            };
            format!(
                "{ended}{}",
                if fix_command.is_empty() {
                    String::new()
                } else {
                    format!("; re-login: `{fix_command}`")
                }
            )
        } else if state == "unavailable" {
            match member(row, "error").as_str() {
                Some(error) if !error.is_empty() => format!("({error})"),
                _ => "reason unknown".to_owned(),
            }
        } else if state == "unsupported" {
            format!(
                "no login end time is published by {}",
                member(row, "provider").as_str().unwrap_or("")
            )
        } else if crossed_end || crossed_refresh {
            if fix_command.is_empty() {
                String::new()
            } else {
                format!("re-login: `{fix_command}`")
            }
        } else if auto_renew {
            let mut parts: Vec<String> = Vec::new();
            parts.push(
                member(row, "refreshExpiresAt")
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(|at| format!("sign-in ends {}", utc(at)))
                    .unwrap_or_else(|| "sign-in end not published".into()),
            );
            parts.push(
                member(row, "renewFrom")
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(|at| format!("renews by itself from {}", utc(at)))
                    .unwrap_or_else(|| "renews by itself".into()),
            );
            if member(row, "lastResult").as_str() == Some("renewed") {
                if let Some(at) = member(row, "lastRenewAt")
                    .as_str()
                    .filter(|s| !s.is_empty())
                {
                    parts.push(format!("last renewed {}", utc(at)));
                }
            }
            if let Some(result) = member(row, "lastResult").as_str() {
                if result != "renewed" && !result.is_empty() {
                    parts.push(format!("last attempt: {}", result.replace('_', " ")));
                }
            }
            parts.join("; ")
        } else if state == "ok" {
            if names_end {
                format!("sign-in ends {}", utc(logged_out_at.as_str().unwrap_or("")))
            } else if let Some(refresh) = member(row, "refreshExpiresAt")
                .as_str()
                .filter(|s| !s.is_empty())
            {
                format!("refresh expires {}", utc(refresh))
            } else {
                "no exact end published yet".to_owned()
            }
        } else if state == "warn" && access_empty {
            "the next run renews this login".to_owned()
        } else if !fix_command.is_empty() {
            if names_end {
                format!("re-login before then: `{fix_command}`")
            } else {
                format!("re-login when the window ends: `{fix_command}`")
            }
        } else if names_end {
            "re-login before then".to_owned()
        } else {
            "re-login when the window ends".to_owned()
        };
        let who = member(row, "email")
            .as_str()
            .filter(|e| !e.is_empty())
            .map(|e| format!(" ({e})"))
            .unwrap_or_default();
        let provider = member(row, "provider").as_str().unwrap_or("");
        let machine = member(row, "machine").as_str().unwrap_or("");
        let name = member(row, "name").as_str().unwrap_or("");
        let tail = if detail.is_empty() {
            String::new()
        } else {
            format!(" {detail}")
        };
        lines.push(format!(
            "- **{provider} · {machine}/{name}**{who}: {word}.{tail}"
        ));
    }
    lines
}

/// `renderRenewalMarkdown`: who keeps the sign-ins alive and what needs a person.
fn render_renewal_markdown(renewal: &Value, now: i64) -> Vec<String> {
    if renewal.is_null() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let runner = member(renewal, "runner").as_str().unwrap_or("");
    let generated_at = member(renewal, "generatedAt").as_str().unwrap_or("");
    if member(renewal, "stale") == &json!(true) {
        lines.push(format!(
            "**The sign-in renewal runner on {runner} last reported {}; renewals are not happening until it runs again.**",
            utc(generated_at)
        ));
        lines.push(String::new());
    } else {
        let ago = js::date_parse_str(generated_at)
            .map(|at| ((now - at) as f64 / 1000.0).round().max(0.0))
            .unwrap_or(0.0);
        lines.push(format!(
            "Claude Code sign-ins renew by themselves (runner on {runner}, last run {} ago).",
            span(ago)
        ));
        lines.push(String::new());
    }
    if let Some(needs) = member(renewal, "needsYou").as_array() {
        for need in needs {
            let email = member(need, "email").as_str().unwrap_or("");
            let how = member(need, "how").as_str().unwrap_or("");
            let reason = member(need, "reason")
                .as_str()
                .unwrap_or("")
                .replace('_', " ");
            let body = if how.is_empty() {
                reason
            } else {
                how.to_owned()
            };
            let since = member(need, "since")
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| format!(" (since {})", utc(s)))
                .unwrap_or_default();
            lines.push(format!("- **NEEDS YOU · {email}**: {body}{since}"));
        }
        if !needs.is_empty() {
            lines.push(String::new());
        }
    }
    lines
}

/// `renderSnapshotMarkdown`: the same snapshot in plain words, one provider section and one
/// line per window. `signInNow` is the publish-time clock the sign-in section reads against.
pub fn render_snapshot_markdown(snapshot: &Value, sign_in_now: i64) -> String {
    let generated_at = member(snapshot, "generatedAt").as_str().unwrap_or("");
    let now = js::date_parse_str(generated_at).unwrap_or(sign_in_now);
    let refresh_seconds = member(snapshot, "refreshSeconds").as_f64().unwrap_or(60.0);
    let stale_after = member(snapshot, "staleAfterSeconds")
        .as_f64()
        .unwrap_or(300.0);
    let mut lines: Vec<String> = vec![
        "# AI Account Center usage".into(),
        String::new(),
        format!(
            "Snapshot {}. The AI Account Center dashboard rewrites it about every {}; if it is more than {} old, the dashboard has stopped updating it. Percentages and amounts are the providers' own; a missing number was not reported, not zero. Burn rates come from measured readings since the window last reset. Machine-readable: usage.json, 7-day readings: history.json, or run aac-usage in this folder.",
            utc(generated_at),
            span(refresh_seconds),
            span(stale_after)
        ),
        String::new(),
    ];
    let accounts = member(snapshot, "accounts")
        .as_array()
        .cloned()
        .unwrap_or_default();
    let visible: Vec<&Value> = accounts
        .iter()
        .filter(|account| member(account, "hidden") != &json!(true))
        .collect();
    if visible.is_empty() {
        lines.push("No account readings yet.".into());
        lines.push(String::new());
    }
    let mut provider: Option<String> = None;
    for account in visible {
        let account_provider = member(account, "provider")
            .as_str()
            .unwrap_or("")
            .to_owned();
        if provider.as_deref() != Some(account_provider.as_str()) {
            provider = Some(account_provider);
            lines.push(format!(
                "## {}",
                member(account, "providerLabel").as_str().unwrap_or("")
            ));
            lines.push(String::new());
        }
        let label = member(account, "label").as_str().unwrap_or("");
        let email = member(account, "email")
            .as_str()
            .filter(|e| !e.is_empty() && *e != label);
        let profile = member(account, "profile")
            .as_str()
            .filter(|p| !p.is_empty() && *p != label);
        let mut who: Vec<String> = Vec::new();
        if let Some(email) = email {
            who.push(email.to_owned());
        }
        if let Some(profile) = profile {
            who.push(format!("profile {profile}"));
        }
        let mut facts: Vec<String> = Vec::new();
        if let Some(plan) = member(account, "plan").as_str().filter(|p| !p.is_empty()) {
            facts.push(plan.to_owned());
        }
        let projected = projected_account(account);
        facts.push(host_name(&projected));
        if member(account, "active") == &json!(true) {
            facts.push("active".into());
        }
        let age = match member(account, "ageSeconds").as_f64() {
            None => "sample time unknown".to_owned(),
            Some(age) if age < 1.0 => "sampled just now".to_owned(),
            Some(age) => format!("sampled {} ago", span(age)),
        };
        let who_text = if who.is_empty() {
            String::new()
        } else {
            format!(" ({})", who.join(", "))
        };
        let status = member(account, "status").as_str().unwrap_or("");
        lines.push(format!(
            "- **{label}**{who_text}, {}: {}, {age}",
            facts.join(", "),
            status_words(status)
        ));
        if status == "cached" {
            let reason = member(account, "cacheReason")
                .as_str()
                .unwrap_or("outcome_unknown");
            lines.push(format!("  - {}", cache_reason_words(reason)));
        }
        if status == "ok" || status == "cached" {
            if let Some(windows) = member(account, "windows").as_array() {
                for window in windows {
                    lines.push(format!("  - {}", window_text(window, now)));
                }
            }
        }
        lines.push(String::new());
    }
    let sign_in = member(snapshot, "signIn")
        .as_array()
        .cloned()
        .unwrap_or_default();
    if !sign_in.is_empty() {
        lines.push(String::new());
        lines.push("## Sign-in health".into());
        lines.push(String::new());
        lines.extend(render_renewal_markdown(
            member(snapshot, "signInRenewal"),
            now,
        ));
        lines.extend(render_sign_in_markdown(&sign_in, sign_in_now));
        lines.push(String::new());
    }
    format!("{}\n", js::js_trim_end(&lines.join("\n")))
}

/// A minimal projected account, only to name the host in the markdown.
fn projected_account(account: &Value) -> ProjectedAccount {
    ProjectedAccount {
        id: String::new(),
        provider: member(account, "provider")
            .as_str()
            .unwrap_or("")
            .to_owned(),
        provider_label: String::new(),
        label: String::new(),
        email: None,
        profile: None,
        source: None,
        plan: None,
        plan_class: None,
        platform: member(account, "platform")
            .as_str()
            .unwrap_or("")
            .to_owned(),
        host: member(account, "host").as_str().unwrap_or("").to_owned(),
        active: false,
        status: String::new(),
        cache_reason: None,
        sampled_at: None,
        age_seconds: None,
        hidden: false,
        windows: Vec::new(),
    }
}
