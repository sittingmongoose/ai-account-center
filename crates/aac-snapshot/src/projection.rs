//! Dashboard rows to the agent usage answer (`agent-usage-projection.ts`): every output field
//! is built from an allowlist, never spread from a row, so nothing a collector attached (paths,
//! messages, sources, profile names, credentials) reaches an agent. Readings are the dashboard's
//! own cached ones; nothing is invented when a provider gave no number.

use crate::js::{self, control_char, date_parse, finite, iso, member, text};
use serde_json::{Value, json};
use std::sync::LazyLock;

/// The snapshot format version.
pub const AGENT_USAGE_VERSION: u32 = 1;

/// The provider order accounts keep (the TypeScript `PROVIDERS` list): the provider table's
/// `snapshotOrder`, so the answer and the table cannot drift.
static PROVIDERS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    aac_providers::snapshot_providers()
        .map(|row| row.id.as_str())
        .collect()
});
const STATUSES: [&str; 5] = ["ok", "cached", "unavailable", "error", "needs_sign_in"];
const PLATFORMS: [&str; 3] = ["ubuntu", "mac", "windows"];
const KINDS: [&str; 4] = ["rate_limit", "balance", "spend", "extra_usage"];
const CACHE_REASONS: [&str; 4] = [
    "read_failed",
    "rate_limited",
    "refresh_pending",
    "source_changed",
];

/// The package-owned computer registry, in its file's key order (`dashboard-hosts.ts`).
static COMPUTERS: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../web-dashboard/public/computers.json"))
        .expect("packaged computer registry")
});

/// The host ids in registry order (`DASHBOARD_HOSTS`).
pub fn dashboard_hosts() -> Vec<&'static str> {
    COMPUTERS
        .as_object()
        .map(|map| map.keys().map(String::as_str).collect())
        .unwrap_or_default()
}

fn host_label(host: &str) -> &'static str {
    COMPUTERS
        .get(host)
        .and_then(|row| row["label"].as_str())
        .unwrap_or("")
}

fn host_machine_label(host: &str) -> &'static str {
    COMPUTERS
        .get(host)
        .and_then(|row| row["machineLabel"].as_str())
        .unwrap_or("")
}

/// One projected window: the allowlisted reading fields.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectedWindow {
    /// The provider's window key.
    pub key: String,
    /// The window's display label.
    pub label: String,
    /// rate_limit, balance, spend or extra_usage.
    pub kind: String,
    /// The used percentage, when reported.
    pub used_percent: Option<f64>,
    /// The remaining percentage, when reported.
    pub remaining_percent: Option<f64>,
    /// The reset time, normalized ISO.
    pub reset_at: Option<String>,
    /// The reset already passed after this reading.
    pub reset_passed: bool,
    /// The window's span in minutes.
    pub window_minutes: Option<f64>,
    /// The used amount.
    pub used: Option<f64>,
    /// The amount limit.
    pub limit: Option<f64>,
    /// The remaining amount.
    pub remaining: Option<f64>,
    /// The amount unit.
    pub unit: Option<String>,
    /// The entitlement expiry, normalized ISO.
    pub expires_at: Option<String>,
    /// Reset grants only: the limit types this grant refills.
    pub clears: Option<Vec<String>>,
    /// Whether the window is unlimited.
    pub unlimited: Option<bool>,
    /// The shared model pool's label.
    pub pool: Option<String>,
    /// The provider's pool id.
    pub pool_id: Option<String>,
    /// The screened model ids.
    pub model_ids: Vec<String>,
    /// When this window was read (a retained window keeps its own older time).
    pub sampled_at: Option<String>,
    /// Whether the reading is retained from an earlier sample.
    pub cached: bool,
}

/// One projected account: identity and readings, nothing private.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectedAccount {
    /// The dashboard's account id.
    pub id: String,
    /// The provider id.
    pub provider: String,
    /// The provider's display name.
    pub provider_label: String,
    /// The account's dashboard name.
    pub label: String,
    /// The signed-in provider account email.
    pub email: Option<String>,
    /// The email the owner typed for an account whose provider reports none (display only).
    pub owner_email: Option<String>,
    /// AAC's saved profile name, when it is a short identifier.
    pub profile: Option<String>,
    /// Where AAC reads it, without path separators.
    pub source: Option<String>,
    /// The screened plan name.
    pub plan: Option<String>,
    /// The entitlement class beside the plan name, when known.
    pub plan_class: Option<String>,
    /// AAC's platform for the account.
    pub platform: String,
    /// The computer the quota reading came from.
    pub host: String,
    /// The account the provider currently uses on its computer.
    pub active: bool,
    /// ok, cached, unavailable, error or needs_sign_in.
    pub status: String,
    /// The safe retained-reading cause, only for cached rows.
    pub cache_reason: Option<String>,
    /// The account's sample time, normalized ISO.
    pub sampled_at: Option<String>,
    /// Whole seconds since the sample, never negative.
    pub age_seconds: Option<i64>,
    /// Hidden on the dashboard by its owner.
    pub hidden: bool,
    /// The projected windows.
    pub windows: Vec<ProjectedWindow>,
}

impl ProjectedWindow {
    /// The window as `usage.json` writes it, field order as the TypeScript literal.
    pub fn value(&self) -> Value {
        js::object(vec![
            ("key", json!(self.key)),
            ("label", json!(self.label)),
            ("kind", json!(self.kind)),
            ("usedPercent", js::opt_number(self.used_percent)),
            ("remainingPercent", js::opt_number(self.remaining_percent)),
            ("resetAt", js::opt_str(self.reset_at.as_deref())),
            ("resetPassed", json!(self.reset_passed)),
            ("windowMinutes", js::opt_number(self.window_minutes)),
            ("used", js::opt_number(self.used)),
            ("limit", js::opt_number(self.limit)),
            ("remaining", js::opt_number(self.remaining)),
            ("unit", js::opt_str(self.unit.as_deref())),
            ("expiresAt", js::opt_str(self.expires_at.as_deref())),
            (
                "clears",
                self.clears
                    .as_ref()
                    .map(|list| json!(list))
                    .unwrap_or(Value::Null),
            ),
            (
                "unlimited",
                self.unlimited.map(Value::Bool).unwrap_or(Value::Null),
            ),
            ("pool", js::opt_str(self.pool.as_deref())),
            ("poolId", js::opt_str(self.pool_id.as_deref())),
            ("modelIds", json!(self.model_ids)),
            ("sampledAt", js::opt_str(self.sampled_at.as_deref())),
            ("cached", json!(self.cached)),
        ])
    }
}

impl ProjectedAccount {
    /// The account as `usage.json` writes it; `planClass` and `cacheReason` appear only when known.
    pub fn value(&self) -> Value {
        let mut members = vec![
            ("id", json!(self.id)),
            ("provider", json!(self.provider)),
            ("providerLabel", json!(self.provider_label)),
            ("label", json!(self.label)),
            ("email", js::opt_str(self.email.as_deref())),
        ];
        // Only when set, so an export without one keeps its exact shape.
        if let Some(email) = &self.owner_email {
            members.push(("ownerEmail", json!(email)));
        }
        members.extend([
            ("profile", js::opt_str(self.profile.as_deref())),
            ("source", js::opt_str(self.source.as_deref())),
            ("plan", js::opt_str(self.plan.as_deref())),
        ]);
        if let Some(class) = &self.plan_class {
            members.push(("planClass", json!(class)));
        }
        members.extend([
            ("platform", json!(self.platform)),
            ("host", json!(self.host)),
            ("active", json!(self.active)),
            ("status", json!(self.status)),
        ]);
        if let Some(reason) = &self.cache_reason {
            members.push(("cacheReason", json!(reason)));
        }
        members.extend([
            ("sampledAt", js::opt_str(self.sampled_at.as_deref())),
            (
                "ageSeconds",
                self.age_seconds
                    .map(|age| json!(age))
                    .unwrap_or(Value::Null),
            ),
            ("hidden", json!(self.hidden)),
            (
                "windows",
                Value::Array(self.windows.iter().map(ProjectedWindow::value).collect()),
            ),
        ]);
        js::object(members)
    }
}

/// `MODEL_ID`: an identifier start, then up to 159 identifier characters.
fn model_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=160).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:/@-".contains(b))
}

/// `PROFILE`: profile names are short identifiers; anything else is left out.
fn profile_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"_.@-".contains(b))
}

/// `CLEAR_SLUG`: lowercase slugs only.
fn clear_slug(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=40).contains(&bytes.len())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
}

/// `profileOf`: the first saved profile name that is a short identifier.
fn profile_of(account: &Value) -> Option<String> {
    let capabilities = member(account, "capabilities");
    for key in ["codexProfile", "claudeProfileId", "antigravityProfileId"] {
        if let Some(value) = member(capabilities, key).as_str()
            && profile_name(value)
        {
            return Some(value.to_owned());
        }
    }
    None
}

/// The words after the first case-insensitive `\bon ` in the source, lowercased; `.` stops at
/// a line terminator exactly as the JavaScript regex does.
fn on_words(source: &str) -> String {
    let bytes = source.as_bytes();
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    for i in 0..bytes.len().saturating_sub(2) {
        if bytes[i..].len() >= 3
            && bytes[i..i + 3].eq_ignore_ascii_case(b"on ")
            && (i == 0 || !word(bytes[i - 1]))
        {
            let rest = &source[i + 3..];
            let end = rest
                .char_indices()
                .find(|(_, c)| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
                .map(|(at, _)| at)
                .unwrap_or(rest.len());
            return rest[..end].to_lowercase();
        }
    }
    String::new()
}

/// `quotaHost`: the named host, else the computer the source's "on ..." words name (longest
/// label first), else the OS platform.
fn quota_host(account: &Value) -> String {
    let host = member(account, "host");
    if let Some(id) = host.as_str()
        && dashboard_hosts().contains(&id)
    {
        return id.to_owned();
    }
    let words = on_words(member(account, "source").as_str().unwrap_or(""));
    let mut hosts = dashboard_hosts();
    // Longest label first; ties keep registry order (a stable sort).
    hosts.sort_by_key(|host| std::cmp::Reverse(host_label(host).len()));
    for id in hosts {
        let label = host_label(id).to_lowercase();
        if words == label
            || words.starts_with(&format!("{label} "))
            || words.starts_with(&format!("{label})"))
        {
            return id.to_owned();
        }
    }
    member(account, "platform")
        .as_str()
        .unwrap_or("")
        .to_owned()
}

/// `sourceOf`: the source text without path separators.
fn source_of(account: &Value) -> Option<String> {
    let value = text(member(account, "source"), 120)?;
    if value.contains('/') || value.contains('\\') {
        return None;
    }
    Some(value)
}

/// `planText`: bounded at 80 code points, without control characters or credential shapes.
fn plan_text(value: &Value) -> Option<String> {
    let s = value.as_str()?;
    if s.is_empty() || s.chars().count() > 80 || s.chars().any(control_char) {
        return None;
    }
    if credential_shaped(s) {
        return None;
    }
    Some(s.to_owned())
}

/// The case-insensitive `/(bearer\s|dca:|sk-[A-Za-z0-9]|eyJ[A-Za-z0-9_-]{8})/i` screen.
fn credential_shaped(s: &str) -> bool {
    let bytes = s.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric();
    let token = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
    for i in 0..bytes.len() {
        let rest = &bytes[i..];
        if rest.len() >= 7
            && rest[..6].eq_ignore_ascii_case(b"bearer")
            && s[i + 6..].chars().next().is_some_and(js::js_whitespace)
        {
            return true;
        }
        if rest.len() >= 4 && rest[..4].eq_ignore_ascii_case(b"dca:") {
            return true;
        }
        if rest.len() >= 4 && rest[..3].eq_ignore_ascii_case(b"sk-") && ident(rest[3]) {
            return true;
        }
        if rest.len() >= 11
            && rest[..3].eq_ignore_ascii_case(b"eyJ")
            && rest[3..11].iter().all(|b| token(*b))
        {
            return true;
        }
    }
    false
}

/// `clearsOf`: at most eight unique slugs, else null.
fn clears_of(value: &Value) -> Option<Vec<String>> {
    if value.is_null() {
        return None;
    }
    let items = value.as_array()?;
    if items.len() > 8 {
        return None;
    }
    let mut slugs = Vec::with_capacity(items.len());
    for item in items {
        let slug = item.as_str().filter(|s| clear_slug(s))?;
        if slugs.contains(&slug.to_owned()) {
            return None;
        }
        slugs.push(slug.to_owned());
    }
    Some(slugs)
}

/// `readingPredatesReset`: the reset is due and the reading predates it (or its time is unknown).
pub fn reading_predates_reset(reset_at: &Value, sampled_at: &Value, now: i64) -> bool {
    let Some(reset) = iso(reset_at).and_then(|text| date_parse(&json!(text))) else {
        return false;
    };
    if reset > now {
        return false;
    }
    match iso(sampled_at).and_then(|text| date_parse(&json!(text))) {
        Some(sampled) => sampled < reset,
        None => true,
    }
}

fn reset_passed_at(window: &Value, now: i64, account_sampled_at: Option<&str>) -> bool {
    let sampled =
        iso(member(window, "sampledAt")).or_else(|| account_sampled_at.map(str::to_owned));
    reading_predates_reset(
        member(window, "resetAt"),
        &sampled.map(Value::String).unwrap_or(Value::Null),
        now,
    )
}

/// `windowOf`: one allowlisted window, or null without a key and label.
fn window_of(
    window: &Value,
    now: i64,
    account_sampled_at: Option<&str>,
) -> Option<ProjectedWindow> {
    let key = text(member(window, "key"), 64)?;
    let label = text(member(window, "label"), 80)?;
    let kind = member(window, "kind")
        .as_str()
        .filter(|k| KINDS.contains(k))
        .unwrap_or("rate_limit");
    let model_ids = member(window, "modelIds")
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|id| id.as_str().filter(|s| model_id(s)).map(str::to_owned))
                .take(64)
                .collect()
        })
        .unwrap_or_default();
    let cached = member(window, "status").as_str() == Some("cached");
    let reset_passed = member(window, "resetPassed") == &json!(true)
        || reset_passed_at(window, now, account_sampled_at);
    Some(ProjectedWindow {
        key,
        label,
        kind: kind.to_owned(),
        used_percent: finite(member(window, "usedPercent")),
        remaining_percent: finite(member(window, "remainingPercent")),
        reset_at: iso(member(window, "resetAt")),
        reset_passed,
        window_minutes: finite(member(window, "windowMinutes")),
        used: finite(member(window, "used")),
        limit: finite(member(window, "limit")),
        remaining: finite(member(window, "remaining")),
        unit: text(member(window, "unit"), 40),
        expires_at: iso(member(window, "expiresAt")),
        clears: clears_of(member(window, "clears")),
        unlimited: member(window, "unlimited").as_bool(),
        pool: text(member(window, "poolLabel"), 80),
        pool_id: text(member(window, "poolId"), 160),
        model_ids,
        sampled_at: if cached {
            iso(member(window, "sampledAt"))
        } else {
            account_sampled_at.map(str::to_owned)
        },
        cached,
    })
}

/// `accountOf`: one allowlisted account, or null without the identity fields.
fn account_of(account: &Value, now: i64) -> Option<ProjectedAccount> {
    let id = text(member(account, "id"), 200)?;
    let label = text(member(account, "label"), 120)?;
    let provider_label = text(member(account, "providerLabel"), 60)?;
    let provider = member(account, "provider")
        .as_str()
        .filter(|p| PROVIDERS.contains(p))?;
    let platform = member(account, "platform")
        .as_str()
        .filter(|p| PLATFORMS.contains(p))?;
    let sampled_at = iso(member(account, "sampledAt"));
    let age_seconds = sampled_at
        .as_deref()
        .and_then(|s| date_parse(&json!(s)))
        .map(|sampled| ((now - sampled) as f64 / 1000.0).floor().max(0.0) as i64);
    let status = member(account, "status")
        .as_str()
        .filter(|s| STATUSES.contains(s))
        .unwrap_or("unavailable");
    let cache_reason = if status == "cached" {
        Some(
            member(account, "cacheReason")
                .as_str()
                .filter(|reason| CACHE_REASONS.contains(reason))
                .unwrap_or("outcome_unknown")
                .to_owned(),
        )
    } else {
        None
    };
    let plan_class = member(account, "planClass")
        .as_str()
        .filter(|class| *class == "max" || *class == "pro")
        .map(str::to_owned);
    let windows = member(account, "windows")
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|window| window_of(window, now, sampled_at.as_deref()))
                .collect()
        })
        .unwrap_or_default();
    Some(ProjectedAccount {
        id,
        provider: provider.to_owned(),
        provider_label,
        label,
        email: text(member(account, "email"), 254),
        owner_email: text(member(account, "email"), 254)
            .is_none()
            .then(|| text(member(account, "ownerEmail"), 254))
            .flatten(),
        profile: profile_of(account),
        source: source_of(account),
        plan: plan_text(member(account, "plan")),
        plan_class,
        platform: platform.to_owned(),
        host: quota_host(account),
        active: member(account, "isActive") == &json!(true),
        status: status.to_owned(),
        cache_reason,
        sampled_at,
        age_seconds,
        hidden: member(account, "hidden") == &json!(true),
        windows,
    })
}

/// `projectAgentUsage`: one answer from dashboard rows. A row listed twice appears once with
/// its newest reading; accounts keep provider order, then row order.
pub fn project_agent_usage(
    rows: &[Value],
    now: i64,
    provider: Option<&str>,
    include_hidden: bool,
) -> Vec<ProjectedAccount> {
    let mut by_id: indexmap::IndexMap<String, ProjectedAccount> = indexmap::IndexMap::new();
    for row in rows {
        let Some(account) = account_of(row, now) else {
            continue;
        };
        if let Some(wanted) = provider
            && account.provider != wanted
        {
            continue;
        }
        if account.hidden && !include_hidden {
            continue;
        }
        let newer = match by_id.get(&account.id) {
            Some(existing) => {
                let left = account
                    .sampled_at
                    .as_deref()
                    .and_then(|s| date_parse(&json!(s)));
                let right = existing
                    .sampled_at
                    .as_deref()
                    .and_then(|s| date_parse(&json!(s)));
                match (left, right) {
                    (Some(l), Some(r)) => l > r,
                    _ => false,
                }
            }
            None => true,
        };
        if newer {
            by_id.insert(account.id.clone(), account);
        }
    }
    let mut accounts: Vec<ProjectedAccount> = by_id.into_values().collect();
    accounts.sort_by_key(|account| {
        PROVIDERS
            .iter()
            .position(|p| *p == account.provider)
            .unwrap_or(PROVIDERS.len())
    });
    accounts
}

/// `hostName` in the markdown: the Ubuntu VM's machine label, else the computer's label.
pub fn host_name(account: &ProjectedAccount) -> String {
    if account.host == "ubuntu" {
        host_machine_label("ubuntu").to_owned()
    } else {
        host_label(&account.host).to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The account order the answer publishes is the hand list it replaced, value for value.
    #[test]
    fn account_order_is_the_tables_snapshot_order() {
        assert_eq!(
            *PROVIDERS,
            [
                "codex",
                "claude",
                "antigravity",
                "muse",
                "cursor",
                "kimi-code",
                "qwen",
                "zai",
                "opencode-go"
            ]
        );
    }

    fn row() -> Value {
        json!({
            "id": "codex:gmail", "provider": "codex", "providerLabel": "Codex",
            "label": "Gmail", "email": "person@example.com", "plan": "Pro",
            "platform": "ubuntu", "source": "Codex usage on Ubuntu", "status": "ok",
            "sampledAt": "2026-10-15T12:00:00.000Z", "isActive": true,
            "windows": [{"key": "five_hour", "label": "5-hour", "usedPercent": 40}],
            "capabilities": {"codexProfile": "gmail"}
        })
    }

    #[test]
    fn screens_private_fields() {
        let mut secret = row();
        secret["message"] = json!("sentinel-secret-value-7d1e");
        secret["capabilities"]["antigravityProfileId"] = json!("/home/sentinel-secret-value-7d1e");
        let answer = project_agent_usage(&[secret], 1_792_065_600_000, None, false);
        let text = js::stringify(&json!(
            answer
                .iter()
                .map(ProjectedAccount::value)
                .collect::<Vec<_>>()
        ));
        assert!(!text.contains("sentinel-secret-value-7d1e"));
        assert_eq!(answer[0].profile.as_deref(), Some("gmail"));
        assert_eq!(answer[0].host, "ubuntu");
    }

    /// The owner's display email reaches the export under its own key, only when the
    /// provider reports none; an account without one keeps the exact old shape.
    #[test]
    fn the_owners_display_email_is_exported_beside_a_missing_provider_email() {
        let mut key = row();
        key["provider"] = json!("opencode-go");
        key["providerLabel"] = json!("OpenCode Go");
        key["id"] = json!("opencode-go:acct:0123abcd");
        key["label"] = json!("Account 2");
        key["email"] = Value::Null;
        key["ownerEmail"] = json!("go@example.com");
        let mut both = row();
        both["ownerEmail"] = json!("typed@example.com");
        let answer = project_agent_usage(&[key, both, row()], 1_792_065_600_000, None, false);
        let values: Vec<Value> = answer.iter().map(ProjectedAccount::value).collect();
        let go = values
            .iter()
            .find(|v| v["provider"] == "opencode-go")
            .expect("the OpenCode Go row");
        assert_eq!(
            (&go["email"], &go["ownerEmail"]),
            (&Value::Null, &json!("go@example.com"))
        );
        let codex = values
            .iter()
            .find(|v| v["provider"] == "codex")
            .expect("the Codex row");
        assert_eq!(codex["email"], "person@example.com");
        assert!(codex.get("ownerEmail").is_none());
        let keys: Vec<&str> = codex
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            &keys[..5],
            ["id", "provider", "providerLabel", "label", "email"]
        );
        assert_eq!(keys[5], "profile");
    }

    #[test]
    fn plan_screen_counts_code_points() {
        let plan_of = |plan: Value| {
            let mut r = row();
            r["plan"] = plan;
            project_agent_usage(&[r], 1_792_065_600_000, None, false)[0]
                .plan
                .clone()
        };
        assert_eq!(plan_of(json!("Ultra")).as_deref(), Some("Ultra"));
        assert_eq!(
            plan_of(json!("🚀".repeat(80))).as_deref(),
            Some("🚀".repeat(80)).as_deref()
        );
        assert_eq!(plan_of(json!("🚀".repeat(81))), None);
        assert_eq!(plan_of(json!("Pro\nMax")), None);
        assert_eq!(plan_of(json!("Pro\u{0085}Max")), None);
        assert_eq!(plan_of(json!("Pro\u{200e}Max")), None);
        assert_eq!(plan_of(json!("sk-abcdefgh")), None);
        assert_eq!(plan_of(json!(42)), None);
        assert_eq!(plan_of(json!("")), None);
        assert_eq!(plan_of(json!("Bearer x")).as_deref(), None);
        assert_eq!(plan_of(json!("dca:token")).as_deref(), None);
        assert_eq!(plan_of(json!("eyJabcdefgh")).as_deref(), None);
    }

    #[test]
    fn quota_host_names_the_reading_computer() {
        let mut claude = row();
        claude["id"] = json!("claude:party");
        claude["provider"] = json!("claude");
        claude["platform"] = json!("windows");
        claude["source"] = json!("Verified quota on Ubuntu (fallback)");
        let answer = project_agent_usage(&[claude], 1_792_065_600_000, None, false);
        assert_eq!(answer[0].platform, "windows");
        assert_eq!(answer[0].host, "ubuntu");
        let mut nas1 = row();
        nas1["host"] = json!("nas1");
        nas1["source"] = json!("Kimi Code usage");
        assert_eq!(
            project_agent_usage(&[nas1], 1_792_065_600_000, None, false)[0].host,
            "nas1"
        );
    }

    #[test]
    fn dedup_keeps_newest_reading_and_provider_order() {
        let now = 1_792_065_600_000;
        let mut older = row();
        older["sampledAt"] = json!("2026-10-15T11:00:00.000Z");
        older["windows"][0]["usedPercent"] = json!(10);
        let mut newer = row();
        newer["windows"][0]["usedPercent"] = json!(50);
        let answer = project_agent_usage(&[older.clone(), newer, older], now, None, false);
        assert_eq!(answer.len(), 1);
        assert_eq!(answer[0].windows[0].used_percent, Some(50.0));
    }

    #[test]
    fn reset_passed_and_hidden() {
        let now = 1_792_065_600_000;
        let mut passed = row();
        passed["sampledAt"] = json!("2026-10-15T09:00:00.000Z");
        passed["windows"][0]["resetAt"] = json!("2026-10-15T10:00:00.000Z");
        assert!(project_agent_usage(&[passed], now, None, false)[0].windows[0].reset_passed);
        let mut hidden = row();
        hidden["hidden"] = json!(true);
        assert!(project_agent_usage(&[hidden.clone()], now, None, false).is_empty());
        assert_eq!(project_agent_usage(&[hidden], now, None, true).len(), 1);
    }

    #[test]
    fn clears_screen() {
        assert_eq!(
            clears_of(&json!(["five_hour", "seven_day"])),
            Some(vec!["five_hour".into(), "seven_day".into()])
        );
        assert_eq!(clears_of(&json!(["five_hour", "five_hour"])), None);
        assert_eq!(clears_of(&json!(["Bad"])), None);
        assert_eq!(clears_of(&Value::Null), None);
        assert_eq!(clears_of(&json!([])), Some(vec![]));
    }
}
