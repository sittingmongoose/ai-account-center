//! Public failover boundary. Adapters retain credentials; only reconstructed fields leave it.
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn matches(pattern: &str, s: &str) -> bool {
    regex::Regex::new(pattern).is_ok_and(|r| r.is_match(s))
}
pub(super) fn text(v: &Value, cap: usize) -> Option<String> {
    let s = v.as_str()?;
    if s.chars().count() > cap
        || s.trim().is_empty()
        || matches(
            r"(?i)[\x00-\x1f\x7f-\x9f\x{061c}\x{200e}\x{200f}\x{2028}-\x{202e}\x{2066}-\x{2069}\x{feff}]|bearer\s|sk[-_]|dca:|eyJ[A-Za-z0-9_-]{8}|access_token|refresh_token|https?://",
            s,
        )
    {
        return None;
    }
    Some(s.trim().into())
}
fn email(v: &Value) -> Option<String> {
    text(v, 254).filter(|s| matches(r"\A[^\s@]+@[^\s@]+\.[^\s@]+\z", s))
}
fn number(v: &Value, nonnegative: bool) -> Option<f64> {
    v.as_f64()
        .filter(|n| n.is_finite() && (!nonnegative || *n >= 0.0))
}
pub(super) fn timestamp(v: &Value, observation: bool, now: i64) -> Option<String> {
    let s = v.as_str()?;
    if s.len() > 64 || !matches(r"\A\d{4}-\d{2}-\d{2}T", s) {
        return None;
    }
    let t = DateTime::parse_from_rfc3339(s).ok()?;
    use chrono::Datelike;
    if !(2000..=2200).contains(&t.year()) || (observation && t.timestamp_millis() > now + 300_000) {
        return None;
    }
    Some(
        t.with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Millis, true),
    )
}
pub(super) fn kinds(provider: &str) -> &'static [&'static str] {
    match provider {
        "codex" => &["codex-principal-v1"],
        "claude" => &["claude-account-org-v1"],
        "antigravity" => &["antigravity-subject"],
        "opencode-wallet" => &["opencode-workspace"],
        "muse" => &["muse-team", "muse-native-credential"],
        "cursor" => &["cursor-subject", "cursor-credential"],
        "kimi-code" => &["kimi-code-credential"],
        "qwen" => &["qwen-session"],
        "zai" => &["zai-credential"],
        "opencode-go" => &["opencode-go-credential"],
        _ => &[],
    }
}
fn digest(v: &Value) -> bool {
    v.as_str().is_some_and(|s| matches(r"\A[a-f0-9]{64}\z", s))
}
pub(super) fn candidate(v: &Value, provider: &str, request: bool) -> Option<Value> {
    let o = v.as_object()?;
    let keys = [
        "locator",
        "selectors",
        "identity",
        "revision",
        "email",
        "plan",
    ];
    if request && (o.len() != 6 || keys.iter().any(|k| !o.contains_key(*k))) {
        return None;
    }
    if !digest(&v["locator"]) || !digest(&v["revision"]) {
        return None;
    }
    let identity = v["identity"].as_object()?;
    if identity.len() != 2
        || !digest(&v["identity"]["key"])
        || !kinds(provider).contains(&v["identity"]["kind"].as_str()?)
    {
        return None;
    }
    let selectors = v["selectors"].as_array()?;
    if selectors.is_empty() || selectors.len() > 32 {
        return None;
    }
    let mut unique = Vec::new();
    for s in selectors {
        let s = s.as_str()?;
        if !matches(r"\A[A-Za-z0-9][A-Za-z0-9:_.@-]{0,159}\z", s) {
            return None;
        }
        if !unique.contains(&s) {
            unique.push(s);
        }
    }
    if !v["email"].is_null() && email(&v["email"]).is_none() {
        return None;
    }
    let plan = text(&v["plan"], 160);
    if request && !v["plan"].is_null() && plan.is_none() {
        return None;
    }
    Some(
        json!({"locator":v["locator"],"selectors":unique,"identity":v["identity"],"revision":v["revision"],"email":email(&v["email"]),"plan":plan}),
    )
}
fn window(v: &Value, now: i64) -> Option<Value> {
    v.as_object()?;
    let mut out = json!({"key":text(&v["key"],64)?,"label":text(&v["label"],80)?});
    for key in [
        "usedPercent",
        "remainingPercent",
        "windowMinutes",
        "used",
        "limit",
        "remaining",
    ] {
        let n = number(&v[key], key != "remaining");
        if !v[key].is_null() && n.is_none()
            || key == "remainingPercent" && n.is_some_and(|n| n > 100.0)
        {
            return None;
        }
        out[key] = if n.is_some() {
            v[key].clone()
        } else {
            Value::Null
        };
    }
    for key in ["resetAt", "expiresAt"] {
        let parsed = timestamp(&v[key], false, now);
        if !v[key].is_null() && parsed.is_none() {
            return None;
        }
        out[key] = json!(parsed);
    }
    let unit = text(&v["unit"], 40);
    if !v["unit"].is_null() && unit.is_none() {
        return None;
    }
    out["unit"] = json!(unit);
    if let Some(kind) = v.get("kind") {
        if !["rate_limit", "balance", "spend", "extra_usage"].contains(&kind.as_str()?) {
            return None;
        }
        out["kind"] = kind.clone();
    }
    for key in ["unlimited", "enabled"] {
        if let Some(b) = v.get(key) {
            b.as_bool()?;
            out[key] = b.clone();
        }
    }
    if let Some(raw) = v.get("clears") {
        let raw = raw.as_array()?;
        let mut seen = BTreeSet::new();
        if raw.len() > 8 {
            return None;
        }
        for key in raw {
            let key = key.as_str()?;
            if !matches(r"\A[a-z0-9_]{1,40}\z", key) || !seen.insert(key) {
                return None;
            }
        }
        if !raw.is_empty() {
            out["clears"] = json!(raw);
        }
    }
    if v["status"] == "cached" {
        out["status"] = json!("cached");
        out["sampledAt"] = json!(timestamp(&v["sampledAt"], true, now)?);
    } else if v["status"] == "unavailable" {
        if [
            "usedPercent",
            "remainingPercent",
            "used",
            "limit",
            "remaining",
            "resetAt",
            "expiresAt",
        ]
        .iter()
        .any(|k| !out[k].is_null())
        {
            return None;
        }
        out["status"] = json!("unavailable");
    }
    for (key, cap) in [("poolId", 160), ("poolLabel", 80)] {
        if let Some(s) = text(&v[key], cap) {
            out[key] = json!(s);
        }
    }
    if ["provider-id", "provider-bucket-membership"]
        .contains(&v["poolIdSource"].as_str().unwrap_or(""))
    {
        out["poolIdSource"] = v["poolIdSource"].clone();
    }
    if let Some(models) = v["modelIds"].as_array().filter(|a| a.len() <= 64) {
        out["modelIds"] = json!(
            models
                .iter()
                .filter(|m| text(m, 160).is_some()
                    && m.as_str()
                        .is_some_and(|s| matches(r"\A[A-Za-z0-9][A-Za-z0-9_.:/@-]{0,159}\z", s)))
                .collect::<Vec<_>>()
        );
    }
    if out["status"] != "unavailable"
        && [
            "usedPercent",
            "remainingPercent",
            "used",
            "limit",
            "remaining",
            "resetAt",
            "expiresAt",
            "unlimited",
            "enabled",
        ]
        .iter()
        .all(|k| out[k].is_null())
    {
        return None;
    }
    Some(out)
}
pub(super) fn sample(v: &Value, provider: &str, platform: &str, now: i64) -> Option<Value> {
    let actual = if provider == "opencode-wallet" {
        "opencode-go"
    } else {
        provider
    };
    if v["provider"] != actual
        || v["platform"] != platform
        || !["ok", "cached"].contains(&v["status"].as_str()?)
    {
        return None;
    }
    let sampled = timestamp(&v["sampledAt"], true, now)?;
    let fetched = timestamp(&v["fetchedAt"], true, now)?;
    if sampled > fetched || !v["email"].is_null() && email(&v["email"]).is_none() {
        return None;
    }
    let raw = v["windows"].as_array()?;
    if raw.is_empty() || raw.len() > 256 {
        return None;
    }
    let mut seen = BTreeSet::new();
    let mut windows = Vec::new();
    for w in raw {
        let w = window(w, now)?;
        if !seen.insert(w["key"].as_str()?.to_owned())
            || w["status"] == "cached" && w["sampledAt"].as_str()? > fetched.as_str()
        {
            return None;
        }
        windows.push(w);
    }
    let label = match provider {
        "codex" => "Codex",
        "claude" => "Claude",
        "antigravity" => "Antigravity",
        "muse" => "Muse Code",
        "cursor" => "Cursor",
        "kimi-code" => "Kimi Code",
        "qwen" => "Qwen",
        "zai" => "Z.ai",
        "opencode-go" => "OpenCode Go",
        "opencode-wallet" => "OpenCode",
        _ => return None,
    };
    let host = match platform {
        "ubuntu" => "Ubuntu",
        "mac" => "Mac",
        "windows" => "Windows",
        _ => return None,
    };
    let mut out = json!({"id":format!("native:{actual}:{platform}"),"provider":actual,"providerLabel":label,"label":label,"email":email(&v["email"]),"plan":text(&v["plan"],160),"platform":platform,"source":format!("{label} usage on {host}"),"status":v["status"],"message":null,"sampledAt":sampled,"fetchedAt":fetched,"isActive":false,"windows":windows,"capabilities":{"codexProfile":null,"claudeProfileId":null,"claudePlatforms":[]}});
    if provider == "claude" {
        if !v["planClass"].is_null() {
            let class = text(&v["planClass"], 160)?;
            if !["max", "pro"].contains(&class.as_str()) {
                return None;
            }
            out["planClass"] = json!(class);
        }
        let extras = &v["optionalExtras"];
        if extras.is_object()
            && ["resetCredits", "prepaidBalance"]
                .iter()
                .all(|k| ["ok", "unavailable"].contains(&extras[k].as_str().unwrap_or("")))
        {
            out["optionalExtras"] = json!({"resetCredits":extras["resetCredits"],"prepaidBalance":extras["prepaidBalance"]});
            if digest(&v["optionalExtrasContext"]) {
                out["optionalExtrasContext"] = v["optionalExtrasContext"].clone();
            }
        }
    }
    if provider == "antigravity" && v["reportedPlan"].is_object() {
        let p = &v["reportedPlan"];
        let mut report = json!({});
        if let Some(t) = text(&p["planType"], 80) {
            report["planType"] = json!(t);
        }
        for tier in ["paidTier", "currentTier"] {
            let mut obj = json!({});
            for key in ["id", "name"] {
                if let Some(t) = text(&p[tier][key], 120) {
                    obj[key] = json!(t);
                }
            }
            if !obj.as_object()?.is_empty() {
                report[tier] = obj;
            }
        }
        if !report.as_object()?.is_empty() {
            out["reportedPlan"] = report;
        }
    }
    Some(out)
}
const FAILURES: &[&str] = &[
    "unavailable",
    "not_installed",
    "needs_sign_in",
    "identity_mismatch",
    "account_mismatch",
    "source_changed",
    "rate_limited",
    "invalid_response",
    "network_error",
    "provider_error",
    "error",
    "busy",
    "no_browser_cookie",
    "local_storage_error",
    "identity_unavailable",
    "plan_mismatch",
    "team_mismatch",
    "choose_team",
    "inactive_subscription",
];
pub(super) fn failure(code: &str, candidate: &Value, retry: &Value) -> Value {
    let mut out = json!({"version":1,"action":"collect","candidate":candidate,"sample":null,"failureCode":if FAILURES.contains(&code){code}else{"unavailable"}});
    if number(retry, true).is_some_and(|n| n <= 86400.0) {
        out["retryAfterSeconds"] = retry.clone();
    }
    out
}
pub(super) fn same(a: &Value, b: &Value) -> bool {
    ["locator", "identity", "revision"]
        .iter()
        .all(|k| a[k] == b[k])
}
pub(super) fn changed(result: &Value) -> Value {
    let mut out = failure("source_changed", &Value::Null, &Value::Null);
    if result["failureCode"] == "rate_limited"
        || result["failureCode"] == "source_changed" && result["rateLimited"] == true
    {
        out = failure("source_changed", &Value::Null, &result["retryAfterSeconds"]);
        out["rateLimited"] = json!(true);
    }
    out
}
pub(super) fn unverified(result: &Value) -> Value {
    if result["failureCode"] == "source_changed" {
        changed(result)
    } else {
        if let Some(code) = result["failureCode"]
            .as_str()
            .filter(|c| FAILURES.contains(c))
        {
            failure(code, &Value::Null, &result["retryAfterSeconds"])
        } else {
            failure("unavailable", &Value::Null, &Value::Null)
        }
    }
}
pub(super) fn finish(
    result: &Value,
    public: &Value,
    provider: &str,
    platform: &str,
    now: i64,
) -> Value {
    if !result.is_object() {
        return failure("invalid_response", public, &Value::Null);
    }
    let quota = sample(&result["sample"], provider, platform, now);
    let mut out = if !result["failureCode"].is_null() {
        let code = result["failureCode"].as_str().unwrap_or("unavailable");
        let mut out = failure(code, public, &result["retryAfterSeconds"]);
        if code == "source_changed" && result["rateLimited"] == true {
            out["rateLimited"] = json!(true);
        }
        if [
            "rate_limited",
            "network_error",
            "provider_error",
            "busy",
            "unavailable",
        ]
        .contains(&code)
            && quota.is_some()
        {
            out["sample"] = json!(quota);
        }
        out
    } else if let Some(quota) = quota {
        let mut out = json!({"version":1,"action":"collect","candidate":public,"sample":quota,"failureCode":null});
        if number(&result["refreshAfterSeconds"], true).is_some_and(|n| n > 0.0 && n <= 86400.0) {
            out["refreshAfterSeconds"] = result["refreshAfterSeconds"].clone();
        }
        out
    } else {
        failure("invalid_response", public, &Value::Null)
    };
    if ["muse", "cursor"].contains(&provider)
        && let (Some(plan), Some(at)) = (
            text(&result["observedPlan"], 160),
            timestamp(&result["observedPlanAt"], true, now),
        )
    {
        out["observedPlan"] = json!(plan);
        out["observedPlanAt"] = json!(at);
    }
    out
}

pub(crate) struct Context {
    pub provider: String,
    pub platform: &'static str,
    pub home: std::path::PathBuf,
    pub ccs: std::path::PathBuf,
    pub claude_profile: Option<Value>,
}
pub(crate) trait Adapter {
    /// Private rows may carry underscore fields. Errors mean unverified discovery.
    fn inventory(&mut self, context: &Context) -> Result<Value, ()>;
    async fn collect(&mut self, candidate: &Value, context: &Context) -> Value;
}
fn scope(v: Option<&Value>, default: std::path::PathBuf) -> Option<std::path::PathBuf> {
    match v {
        None | Some(Value::Null) => Some(default),
        Some(Value::String(s))
            if !s.is_empty()
                && s.chars().count() <= 4096
                && !s.chars().any(|c| c < ' ' || c == '\x7f')
                && std::path::Path::new(s).is_absolute() =>
        {
            Some(crate::files::lexical(std::path::Path::new(s)))
        }
        _ => None,
    }
}
fn profile(v: &Value) -> Option<Value> {
    let obj = v.as_object()?;
    if !obj.contains_key("id")
        || !obj.contains_key("email")
        || obj
            .keys()
            .any(|k| !["id", "email", "profilePath", "isDefault"].contains(&k.as_str()))
        || !matches(r"\A[A-Za-z0-9][A-Za-z0-9_-]{0,63}\z", v["id"].as_str()?)
    {
        return None;
    }
    let mut result = json!({"id":v["id"],"email":email(&v["email"])?});
    if let Some(path) = obj.get("profilePath") {
        let s = path.as_str()?;
        if s.is_empty()
            || s.chars().count() > 4096
            || s.chars().any(|c| c < ' ' || c == '\x7f')
            || !std::path::Path::new(s).is_absolute()
            || s.starts_with("//")
            || s.starts_with("\\\\")
        {
            return None;
        }
        result["profilePath"] = path.clone();
    }
    if let Some(default) = obj.get("isDefault") {
        default.as_bool()?;
        result["isDefault"] = default.clone();
    }
    Some(result)
}
fn rows(raw: &Value, provider: &str) -> Vec<(Value, Value)> {
    let Some(values) = raw["candidates"].as_array().filter(|a| a.len() <= 512) else {
        return vec![];
    };
    let mut found: Vec<(Value, Value)> = vec![];
    let mut ambiguous = BTreeSet::new();
    for private in values.iter().take(128) {
        let Some(public) = candidate(private, provider, false) else {
            continue;
        };
        if let Some((previous, _)) = found
            .iter()
            .find(|(p, _)| p["locator"] == public["locator"])
        {
            if previous != &public {
                ambiguous.insert(public["locator"].clone().as_str().unwrap_or("").to_owned());
            }
        } else {
            found.push((public, private.clone()));
        }
    }
    found.retain(|(p, _)| !ambiguous.contains(p["locator"].as_str().unwrap_or("")));
    found
}
fn unread(raw: &Value, selectors: &Value) -> bool {
    raw["failed"] == true
        || raw["incompleteSelectors"]
            .as_array()
            .is_some_and(|incomplete| {
                incomplete.iter().any(|s| {
                    s == "*"
                        || selectors.as_array().is_some_and(|ss| {
                            ss.iter().any(|selector| {
                                s == selector
                                    || selector
                                        .as_str()
                                        .and_then(|s| s.split_once(':'))
                                        .is_some_and(|(family, _)| {
                                            s == &json!(format!("{family}:*"))
                                        })
                            })
                        })
                })
            })
}
pub(crate) async fn execute(request: &Value, adapter: &mut impl Adapter, now: i64) -> Value {
    let inventory = request["action"] == "inventory";
    let failed = if inventory {
        json!({"version":1,"action":"inventory","candidates":[]})
    } else {
        failure("invalid_response", &Value::Null, &Value::Null)
    };
    let Some(obj) = request.as_object() else {
        return failed;
    };
    let provider = request["provider"].as_str().unwrap_or("");
    if request["version"].as_u64() != Some(1)
        || !(inventory || request["action"] == "collect")
        || kinds(provider).is_empty()
        || obj.keys().any(|k| {
            ![
                "version",
                "action",
                "provider",
                "ccsDir",
                "homeDir",
                "claudeProfile",
            ]
            .contains(&k.as_str())
                && !(k == "candidate" && !inventory)
        })
        || obj.contains_key("claudeProfile") && provider != "claude"
    {
        return failed;
    }
    let error = || {
        if inventory {
            json!({"version":1,"action":"inventory","candidates":[],"failed":true})
        } else {
            failure("unavailable", &Value::Null, &Value::Null)
        }
    };
    let claude_profile = if let Some(raw) = obj.get("claudeProfile") {
        let Some(p) = profile(raw) else {
            return error();
        };
        Some(p)
    } else {
        None
    };
    let wanted = if inventory {
        Value::Null
    } else {
        let Some(c) = candidate(&request["candidate"], provider, true) else {
            return failed;
        };
        c
    };
    let default = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let Some(home) = scope(obj.get("homeDir"), default) else {
        return error();
    };
    let Some(ccs) = scope(obj.get("ccsDir"), home.join(".ccs")) else {
        return error();
    };
    let context = Context {
        provider: provider.into(),
        platform: if cfg!(target_os = "windows") {
            "windows"
        } else if cfg!(target_os = "macos") {
            "mac"
        } else {
            "ubuntu"
        },
        home,
        ccs,
        claude_profile,
    };
    let Ok(before) = adapter.inventory(&context) else {
        return error();
    };
    if before["failed"] == true {
        return error();
    }
    let before_rows = rows(&before, provider);
    if inventory {
        let mut out = json!({"version":1,"action":"inventory","candidates":before_rows.iter().map(|(p,_)|p).collect::<Vec<_>>()});
        if provider == "claude"
            && let Some(statuses) = before["signInNeededSelectors"]
                .as_array()
                .filter(|a| a.len() <= 256)
        {
            let mut valid = vec![];
            for s in statuses {
                if s.as_str().is_some_and(|s| {
                    matches(
                        r"\Aclaude-(?:desktop-)?profile:[A-Za-z0-9][A-Za-z0-9_-]{0,63}\z",
                        s,
                    )
                }) && !valid.contains(s)
                {
                    valid.push(s.clone());
                }
            }
            if !valid.is_empty() {
                out["signInNeededSelectors"] = json!(valid);
            }
        }
        if let Some(incomplete) = before["incompleteSelectors"].as_array() {
            let valid: BTreeSet<_> = incomplete
                .iter()
                .filter_map(Value::as_str)
                .filter(|s| {
                    *s == "*"
                        || matches(r"\A[A-Za-z0-9][A-Za-z0-9_.@-]{0,62}:\*\z", s)
                        || matches(r"\A[A-Za-z0-9][A-Za-z0-9:_.@-]{0,159}\z", s)
                })
                .collect();
            if !valid.is_empty() {
                out["incompleteSelectors"] = if valid.len() > 128 {
                    json!(["*"])
                } else {
                    json!(valid)
                };
            }
        }
        if before["failed"] == true {
            out["failed"] = json!(true);
        }
        return if serde_json::to_vec(&out).is_ok_and(|b| b.len() <= 512 * 1024) {
            out
        } else {
            error()
        };
    }
    let Some((public, private)) = before_rows.iter().find(|(p, _)| same(p, &wanted)) else {
        return failure(
            if unread(&before, &wanted["selectors"]) {
                "unavailable"
            } else {
                "source_changed"
            },
            &Value::Null,
            &Value::Null,
        );
    };
    let result = adapter.collect(private, &context).await;
    let mut out = match adapter.inventory(&context) {
        Err(()) => unverified(&result),
        Ok(after) => {
            if after["failed"] == true {
                unverified(&result)
            } else if rows(&after, provider).iter().any(|(p, _)| same(p, public)) {
                finish(&result, public, provider, context.platform, now)
            } else if unread(&after, &public["selectors"]) {
                unverified(&result)
            } else {
                changed(&result)
            }
        }
    };
    if provider == "claude"
        && let Some(observations) = result["claudeHttp"].as_array()
    {
        let safe:Vec<_>=observations.iter().filter(|o|["oauth_profile","oauth_usage","optional_web"].contains(&o["stage"].as_str().unwrap_or(""))&&(o["status"].is_null()||o["status"].as_u64().is_some_and(|s|(100..=599).contains(&s)))).take(5).map(|o|json!({"stage":o["stage"],"status":o["status"],"retryAfterSeconds":number(&o["retryAfterSeconds"],true)})).collect();
        if !safe.is_empty() {
            out["claudeHttp"] = json!(safe);
        }
    }
    if serde_json::to_vec(&out).is_ok_and(|b| b.len() <= 512 * 1024) {
        out
    } else {
        error()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn python_public_boundary_oracle() {
        let cases: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/failover-public.json"))
                .expect("oracle");
        for (i, c) in cases.as_array().expect("cases").iter().enumerate() {
            let args = &c["args"];
            let got = match c["kind"].as_str().expect("kind") {
                "candidate" => json!(candidate(
                    &c["value"],
                    args["provider"].as_str().expect("provider"),
                    args["request"] == true
                )),
                "sample" => json!(sample(
                    &c["value"],
                    args["provider"].as_str().expect("provider"),
                    args["platform"].as_str().expect("platform"),
                    1_795_716_000_000
                )),
                "changed" => changed(&c["value"]),
                "unverified" => unverified(&c["value"]),
                _ => panic!("case"),
            };
            assert_eq!(got, c["expected"], "oracle case {i}: {c}");
        }
    }
}

#[cfg(test)]
mod race_tests {
    use super::*;
    struct Fake {
        inventories: std::collections::VecDeque<Result<Value, ()>>,
        result: Value,
        calls: usize,
        inventories_read: usize,
    }
    impl Adapter for Fake {
        fn inventory(&mut self, _: &Context) -> Result<Value, ()> {
            self.inventories_read += 1;
            self.inventories
                .pop_front()
                .expect("bounded inventory calls")
        }
        async fn collect(&mut self, c: &Value, _: &Context) -> Value {
            self.calls += 1;
            assert_eq!(c["_token"], "private-native-fixture");
            assert_eq!(c["email"], "native@example.test");
            self.result.clone()
        }
    }
    fn private() -> Value {
        json!({"locator":"a".repeat(64),"identity":{"kind":"codex-principal-v1","key":"b".repeat(64)},"revision":"c".repeat(64),"selectors":["codex-profile:default"],"email":"native@example.test","plan":"Plus","_token":"private-native-fixture"})
    }
    fn request() -> Value {
        let mut c = candidate(&private(), "codex", false).expect("candidate");
        c["email"] = json!("caller@example.test");
        json!({"version":1,"action":"collect","provider":"codex","homeDir":"/synthetic-home","ccsDir":"/synthetic-ccs","candidate":c})
    }
    fn fake(after: Result<Value, ()>) -> Fake {
        Fake {
            inventories: [Ok(json!({"candidates":[private()]})), after].into(),
            result: json!({"failureCode":"rate_limited","retryAfterSeconds":123}),
            calls: 0,
            inventories_read: 0,
        }
    }
    #[tokio::test]
    async fn rotation_discards_quota_but_keeps_original_provider_hold() {
        let mut f = fake(Ok(json!({"candidates":[]})));
        let out = execute(&request(), &mut f, 0).await;
        assert_eq!(
            out,
            json!({"version":1,"action":"collect","candidate":null,"sample":null,"failureCode":"source_changed","retryAfterSeconds":123,"rateLimited":true})
        );
        assert_eq!(f.calls, 1);
    }
    #[tokio::test]
    async fn unread_recheck_preserves_identity_rejection() {
        let mut f = fake(Err(()));
        f.result = json!({"failureCode":"team_mismatch","sample":{"private":"discard"}});
        assert_eq!(
            execute(&request(), &mut f, 0).await["failureCode"],
            "team_mismatch"
        );
    }
    #[tokio::test]
    async fn unread_store_is_not_removed() {
        let mut f = fake(Ok(
            json!({"candidates":[],"incompleteSelectors":["codex-profile:*"]}),
        ));
        let out = execute(&request(), &mut f, 0).await;
        assert_eq!(out["failureCode"], "rate_limited");
        assert_eq!(out["retryAfterSeconds"], 123);
        assert!(out["candidate"].is_null());
    }
    #[tokio::test]
    async fn unrelated_unread_store_does_not_hide_removal() {
        let mut f = fake(Ok(json!({"candidates":[],"incompleteSelectors":["t3"]})));
        assert_eq!(
            execute(&request(), &mut f, 0).await["failureCode"],
            "source_changed"
        );
    }
    #[tokio::test]
    async fn caller_metadata_is_never_used_for_collection() {
        let mut f = fake(Ok(json!({"candidates":[private()]})));
        let out = execute(&request(), &mut f, 0).await;
        assert_eq!(out["candidate"]["email"], "native@example.test");
        assert!(!out.to_string().contains("private-native-fixture"));
    }
    #[tokio::test]
    async fn private_request_fields_rejected_before_inventory() {
        let mut f = fake(Err(()));
        let mut r = request();
        r["candidate"]["_token"] = json!("private-caller");
        assert_eq!(
            execute(&r, &mut f, 0).await["failureCode"],
            "invalid_response"
        );
        assert_eq!(f.inventories_read, 0);
    }
    #[tokio::test]
    async fn stale_revision_never_calls_provider() {
        let mut f = fake(Err(()));
        let mut r = request();
        r["candidate"]["revision"] = json!("d".repeat(64));
        assert_eq!(
            execute(&r, &mut f, 0).await["failureCode"],
            "source_changed"
        );
        assert_eq!(f.calls, 0);
    }
    #[tokio::test]
    async fn conflicting_locator_never_calls_provider() {
        let mut f = fake(Err(()));
        let mut conflicting = private();
        conflicting["identity"]["key"] = json!("e".repeat(64));
        f.inventories[0] = Ok(json!({"candidates":[private(),conflicting]}));
        assert_eq!(
            execute(&request(), &mut f, 0).await["failureCode"],
            "source_changed"
        );
        assert_eq!(f.calls, 0);
    }
}

#[cfg(test)]
mod diagnostics_tests {
    use super::*;
    struct Fake {
        calls: usize,
    }
    impl Adapter for Fake {
        fn inventory(&mut self, _: &Context) -> Result<Value, ()> {
            self.calls += 1;
            if self.calls > 1 {
                return Err(());
            }
            Ok(
                json!({"candidates":[{"locator":"a".repeat(64),"identity":{"kind":"claude-account-org-v1","key":"b".repeat(64)},"revision":"c".repeat(64),"selectors":["discover"],"email":null,"plan":null}]}),
            )
        }
        async fn collect(&mut self, _: &Value, _: &Context) -> Value {
            json!({"failureCode":"rate_limited","retryAfterSeconds":123,"claudeHttp":[{"stage":"oauth_profile","status":200,"retryAfterSeconds":null,"_token":"private-fixture"},{"stage":"oauth_usage","status":429,"retryAfterSeconds":123},{"stage":"private-fixture","status":200}]})
        }
    }
    #[tokio::test]
    async fn unverified_recheck_keeps_only_safe_http_observations() {
        let request = json!({"version":1,"action":"collect","provider":"claude","homeDir":"/synthetic-home","candidate":{"locator":"a".repeat(64),"identity":{"kind":"claude-account-org-v1","key":"b".repeat(64)},"revision":"c".repeat(64),"selectors":["discover"],"email":null,"plan":null}});
        let out = execute(&request, &mut Fake { calls: 0 }, 0).await;
        assert_eq!(out["failureCode"], "rate_limited");
        assert_eq!(out["claudeHttp"].as_array().expect("safe HTTP").len(), 2);
        assert!(!out.to_string().contains("private-fixture"));
    }
}
