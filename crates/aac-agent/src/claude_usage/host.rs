//! The private reset host (`reset_host`): capture, read and one confirmed claim, run on the
//! Claude credential host. Credentials stay on this host. Before the claim POST is reached,
//! every refusal proves nothing was sent; after it, any failure is ambiguous, never success.
use super::{
    Unavailable,
    desktop::{self, CodeLogin},
    http::{self, Client, HttpError},
    normalize,
};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::{path::PathBuf, sync::LazyLock, time::Duration};

const CLI_FLOOR: (u32, u32, u32) = (2, 1, 325);
static CLI_VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A(\d{1,4})\.(\d{1,4})\.(\d{1,6})\z").expect("version"));
static GRANT_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[a-z0-9_-]{1,40}\z").expect("grant"));
static REQUEST_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[A-Za-z0-9_-]{1,64}\z").expect("request"));
const LOGIN_REASONS: [(&str, &str); 4] = [
    ("identity_unconfirmed", "identity-unconfirmed"),
    ("no_matching_code_login", "no-matching-code-login"),
    ("ambiguous_code_login", "ambiguous-code-login"),
    ("signed_out", "signed-out"),
];
fn login_reason(status: &str) -> Option<&'static str> {
    LOGIN_REASONS
        .iter()
        .find(|(s, _)| *s == status)
        .map(|(_, r)| *r)
}
fn unavailable() -> Unavailable {
    Unavailable::new("unavailable")
}

/// `claude_usage.Client.get` statuses.
fn usage_error(e: HttpError) -> Unavailable {
    match e.status {
        Some(429) if !e.invalid => Unavailable {
            status: "rate_limited",
            retry_after: e.retry_after,
        },
        Some(403) if !e.invalid => Unavailable::new("forbidden"),
        Some(401) if !e.invalid => Unavailable::new("needs_sign_in"),
        _ => Unavailable::new("error"),
    }
}
fn client() -> Result<Client, Unavailable> {
    Client::new(Duration::from_secs(10), Duration::from_secs(4)).ok_or(Unavailable::new("error"))
}

/// What a reset read proves before it trusts usage: the Desktop profile is the same folder and
/// login, and (with Code logins) the saved login choice is unchanged.
#[derive(Clone)]
struct DesktopCheck {
    id: Value,
    dir: Value,
    is_default: bool,
    root: PathBuf,
    binding: String,
}
#[derive(Clone, Default)]
struct Checks {
    desktop: Option<DesktopCheck>,
    saved: Option<(Value, String)>,
}
impl Checks {
    fn desktop_root(d: &DesktopCheck) -> Result<(), Unavailable> {
        (desktop::resolve_profile_path(&d.id, &d.dir, &super::home(), d.is_default)? == d.root)
            .then_some(())
            .ok_or(unavailable())
    }
    fn saved(request: &Value, binding: &str) -> Result<(), Unavailable> {
        let codes = desktop::code_reset_logins(
            &request["expectedEmail"],
            &super::home(),
            super::now_ms() as f64,
        );
        (reset_logins(request, codes)?.binding == binding)
            .then_some(())
            .ok_or(unavailable())
    }
    fn root(&self) -> Result<(), Unavailable> {
        match (&self.desktop, &self.saved) {
            (Some(d), _) => Self::desktop_root(d),
            (None, Some((request, binding))) => Self::saved(request, binding),
            (None, None) => Ok(()),
        }
    }
    fn login(&self) -> Result<(), Unavailable> {
        if let Some(d) = &self.desktop {
            Self::desktop_root(d)?;
            let (account, cache) = desktop::decrypt_cache(&d.root)?;
            if desktop::login_binding(&json!(account), &Value::Object(cache)) != d.binding {
                return Err(unavailable());
            }
        }
        if let Some((request, binding)) = &self.saved {
            Self::saved(request, binding)?;
        }
        Ok(())
    }
}

/// The private reading one login produced (`private_reset_sink`).
struct Private {
    account: String,
    organization: String,
    usage: Value,
    token: String,
    observed_ms: i64,
    saved_binding: Option<String>,
    version_dirs: Option<Vec<PathBuf>>,
    checks: Checks,
}
/// What a collect told the host, without any provider text.
#[derive(Default)]
struct Outcome {
    status: &'static str,
    reset_reason: Option<&'static str>,
    rate_limited: bool,
}

/// `_reset_target_organization`.
fn target_organization(request: &Value) -> Result<Option<String>, Unavailable> {
    if request["action"] == "claim" {
        return request["write"]["identity"]["organization"]
            .as_str()
            .map(|o| Some(o.to_owned()))
            .ok_or(unavailable());
    }
    Ok(organization_value(&request["expectedSavedOrganization"]))
}
/// A non-null, non-string organization pin matches no saved login.
fn organization_value(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        _ => Some("\0".into()),
    }
}

struct DesktopLogin {
    account: String,
    organization: String,
    binding: String,
}
/// `_desktop_reset_login`: the Desktop profile's saved login, with no provider call.
fn desktop_login(request: &Value) -> Result<DesktopLogin, Unavailable> {
    if !cfg!(any(windows, target_os = "macos"))
        || !desktop::valid_profile_id(&request["profile"])
        || desktop::expected_email(&request["expectedEmail"]).is_none()
    {
        return Err(unavailable());
    }
    let organization = target_organization(request)?;
    let is_default = request["isDefault"] == true;
    let home = super::home();
    let root = desktop::resolve_profile_path(
        &request["profile"],
        &request["profileDir"],
        &home,
        is_default,
    )?;
    let (account, cache) = desktop::decrypt_cache(&root)?;
    let now = super::now_ms() as f64;
    let found = desktop::eligible_entries(&account, &cache, false, organization.as_deref(), now);
    let Some(first) = found.first() else {
        if organization.is_some()
            && !desktop::eligible_entries(&account, &cache, false, None, now).is_empty()
        {
            return Err(unavailable());
        }
        return Err(Unavailable::new("needs_sign_in"));
    };
    if desktop::resolve_profile_path(
        &request["profile"],
        &request["profileDir"],
        &home,
        is_default,
    )? != root
    {
        return Err(unavailable());
    }
    Ok(DesktopLogin {
        organization: first.identities[2].clone(),
        binding: desktop::login_binding(&json!(account), &Value::Object(cache)),
        account,
    })
}

struct Login {
    account: String,
    organization: String,
    binding: String,
    desktop: Option<DesktopLogin>,
    code: Option<CodeLogin>,
    code_ambiguous: bool,
}
/// `reset_logins`: which saved login a reset goes through, without any provider call.
fn reset_logins(request: &Value, codes: Vec<CodeLogin>) -> Result<Login, Unavailable> {
    let mut organization = organization_value(&request["expectedSavedOrganization"]);
    let mut account: Option<String> = None;
    if request["action"] == "claim" {
        let target = &request["write"]["identity"];
        organization = organization_value(&target["organization"]);
        account = organization_value(&target["account"]);
        if target.get("organization").is_none() || target.get("account").is_none() {
            return Err(unavailable());
        }
    }
    let matches = |a: &str, o: &str| {
        organization.as_deref().is_none_or(|x| x == o) && account.as_deref().is_none_or(|x| x == a)
    };
    let mut seen: Vec<(String, String)> = vec![];
    let codes: Vec<CodeLogin> = codes
        .into_iter()
        .filter(|c| matches(&c.account, &c.organization))
        .filter(|c| {
            let pair = (c.account.clone(), c.organization.clone());
            if seen.contains(&pair) {
                false
            } else {
                seen.push(pair);
                true
            }
        })
        .collect();
    let mut found_desktop = desktop_login(request).ok();
    if let Some(d) = &found_desktop
        && (!matches(&d.account, &d.organization)
            || (!codes.is_empty() && !codes.iter().any(|c| c.account == d.account)))
    {
        found_desktop = None;
    }
    let (account, organization, codes) = if let Some(d) = &found_desktop {
        let same: Vec<CodeLogin> = codes
            .into_iter()
            .filter(|c| c.account == d.account && c.organization == d.organization)
            .collect();
        (d.account.clone(), d.organization.clone(), same)
    } else if codes.len() == 1 {
        (
            codes[0].account.clone(),
            codes[0].organization.clone(),
            codes,
        )
    } else {
        return Err(Unavailable::new(if codes.is_empty() {
            "no_matching_code_login"
        } else {
            "ambiguous_code_login"
        }));
    };
    let code = (codes.len() == 1).then(|| codes[0].clone());
    let binding = match (&found_desktop, &code) {
        (Some(d), Some(c)) => desktop::sha256(&desktop::py_dumps(
            &json!(["claude-reset-login-v2", d.binding, c.binding]),
            false,
        )),
        (Some(d), None) => d.binding.clone(),
        (None, Some(c)) => c.binding.clone(),
        (None, None) => return Err(unavailable()),
    };
    Ok(Login {
        account,
        organization,
        binding,
        desktop: found_desktop,
        code,
        code_ambiguous: codes.len() > 1,
    })
}

/// `capture_reset_login`: saved credentials only, no provider call.
fn capture(request: &Value) -> Result<Value, Unavailable> {
    let codes = desktop::code_reset_logins(
        &request["expectedEmail"],
        &super::home(),
        super::now_ms() as f64,
    );
    let pinned = match &request["expectedSavedOrganization"] {
        Value::Null | Value::Bool(false) => false,
        Value::String(s) => !s.is_empty(),
        _ => true,
    };
    let (account, organization, binding) = if !codes.is_empty() || pinned {
        let login = reset_logins(request, codes)?;
        (login.account, login.organization, login.binding)
    } else {
        let login = desktop_login(request).map_err(|e| {
            if e.status == "needs_sign_in" {
                Unavailable::new("signed_out")
            } else {
                e
            }
        })?;
        (login.account, login.organization, login.binding)
    };
    Ok(
        json!({"account":account,"organization":organization,"savedLoginBinding":binding,"observedAtMs":super::now_ms()}),
    )
}

/// `collect(..., private_reset_sink=...)`: the Desktop profile's verified usage, privately.
async fn collect_desktop(
    request: &Value,
    expected_binding: Option<&str>,
    expected_organization: Option<&str>,
    sink: &mut Vec<Private>,
) -> Outcome {
    let expected = desktop::expected_email(&request["expectedEmail"]);
    let platform_ok = cfg!(any(windows, target_os = "macos"));
    if !desktop::valid_profile_id(&request["profile"]) || expected.is_none() || !platform_ok {
        return Outcome {
            status: "unavailable",
            ..Default::default()
        };
    }
    let expected = expected.unwrap_or_default();
    match collect_desktop_inner(
        request,
        &expected,
        expected_binding,
        expected_organization,
        sink,
    )
    .await
    {
        Ok(()) => Outcome {
            status: "ok",
            ..Default::default()
        },
        Err(e) => Outcome {
            status: match e.status {
                "rate_limited" => "error",
                "identity_unconfirmed" => "unavailable",
                s => s,
            },
            reset_reason: (e.status == "identity_unconfirmed").then_some("identity-unconfirmed"),
            rate_limited: e.status == "rate_limited",
        },
    }
}
async fn collect_desktop_inner(
    request: &Value,
    expected: &str,
    expected_binding: Option<&str>,
    expected_organization: Option<&str>,
    sink: &mut Vec<Private>,
) -> Result<(), Unavailable> {
    let home = super::home();
    let is_default = request["isDefault"] == true;
    let root = desktop::resolve_profile_path(
        &request["profile"],
        &request["profileDir"],
        &home,
        is_default,
    )?;
    let (account, cache) = desktop::decrypt_cache(&root)?;
    let now = super::now_ms() as f64;
    let candidates = desktop::eligible_entries(&account, &cache, false, expected_organization, now);
    let saved_binding = desktop::login_binding(&json!(account), &Value::Object(cache.clone()));
    if expected_binding.is_some_and(|b| b != saved_binding) {
        return Err(unavailable());
    }
    let checks = Checks {
        desktop: Some(DesktopCheck {
            id: request["profile"].clone(),
            dir: request["profileDir"].clone(),
            is_default,
            root: root.clone(),
            binding: saved_binding.clone(),
        }),
        saved: None,
    };
    if candidates.is_empty() {
        if expected_organization.is_some()
            && !desktop::eligible_entries(&account, &cache, false, None, now).is_empty()
        {
            return Err(unavailable());
        }
        return Err(Unavailable::new("needs_sign_in"));
    }
    let client = client()?;
    let mut forbidden = false;
    for entry in candidates {
        checks.root()?;
        let profile = match client
            .get(http::PROFILE, &entry.token)
            .await
            .map_err(usage_error)
        {
            Ok(p) => p,
            Err(e) if e.status == "forbidden" => {
                forbidden = true;
                continue;
            }
            Err(e) if e.status == "needs_sign_in" => continue,
            Err(e) => return Err(e),
        };
        let (Some(live), Some(org)) = (
            profile["account"].as_object(),
            profile["organization"].as_object(),
        ) else {
            continue;
        };
        let Some(org_id) = org.get("uuid").and_then(Value::as_str) else {
            continue;
        };
        if live.get("email") != Some(&json!(expected))
            || live.get("uuid") != Some(&json!(account))
            || org_id != entry.identities[2]
        {
            continue;
        }
        checks.root()?;
        let observed = super::now_ms();
        checks.login()?;
        let usage = client
            .get(http::USAGE, &entry.token)
            .await
            .map_err(usage_error)?;
        checks.root()?;
        checks.login()?;
        let mut windows = normalize::normalize_windows(&usage);
        windows.extend(normalize::normalize_reset_credits(
            &usage["cedar_ember"],
            super::now_ms() as f64 / 1000.0,
        ));
        windows.extend(normalize::normalize_session_reset_allowance(
            &usage["juniper_tide"],
        ));
        if windows.is_empty() {
            return Err(unavailable());
        }
        checks.root()?;
        sink.push(Private {
            account: account.clone(),
            organization: org_id.to_owned(),
            usage,
            token: entry.token,
            observed_ms: observed,
            saved_binding: Some(saved_binding),
            version_dirs: None,
            checks,
        });
        return Ok(());
    }
    Err(Unavailable::new(if forbidden {
        "forbidden"
    } else {
        "needs_sign_in"
    }))
}

/// `collect_code_reset`: the live profile must name the same account, organization and email.
async fn collect_code(
    login: &CodeLogin,
    request: &Value,
    saved: &str,
    sink: &mut Vec<Private>,
) -> Outcome {
    let checks = Checks {
        desktop: None,
        saved: Some((request.clone(), saved.to_owned())),
    };
    let run = async {
        let expected = desktop::expected_email(&request["expectedEmail"]).ok_or(unavailable())?;
        let client = client()?;
        checks.root()?;
        let profile = client
            .get(http::PROFILE, &login.token)
            .await
            .map_err(usage_error)?;
        let email_ok = profile["account"]["email"].as_str().is_some_and(|e| {
            normalize::py_strip(e).to_lowercase() == normalize::py_strip(&expected).to_lowercase()
        });
        if !profile["account"].is_object()
            || !profile["organization"].is_object()
            || !email_ok
            || profile["account"]["uuid"] != json!(login.account)
            || profile["organization"]["uuid"] != json!(login.organization)
        {
            return Ok::<bool, Unavailable>(false);
        }
        checks.root()?;
        let observed = super::now_ms();
        let usage = client
            .get(http::USAGE, &login.token)
            .await
            .map_err(usage_error)?;
        checks.root()?;
        sink.push(Private {
            account: login.account.clone(),
            organization: login.organization.clone(),
            usage,
            token: login.token.clone(),
            observed_ms: observed,
            saved_binding: None,
            version_dirs: Some(vec![super::home().join(".local/share/claude/versions")]),
            checks: checks.clone(),
        });
        Ok::<bool, Unavailable>(true)
    };
    match run.await {
        Ok(true) => Outcome {
            status: "ok",
            ..Default::default()
        },
        Ok(false) => Outcome {
            status: "unavailable",
            reset_reason: Some("identity-unconfirmed"),
            ..Default::default()
        },
        Err(e) if e.status == "rate_limited" => Outcome {
            status: "error",
            rate_limited: true,
            ..Default::default()
        },
        Err(e) => match login_reason(e.status) {
            Some(r) => Outcome {
                status: "unavailable",
                reset_reason: Some(r),
                ..Default::default()
            },
            None => Outcome {
                status: if e.status == "needs_sign_in" {
                    "needs_sign_in"
                } else {
                    "unavailable"
                },
                ..Default::default()
            },
        },
    }
}

/// `claude_version_dirs` + `claude_claim_user_agent`: entry names only, never a binary run.
fn claim_user_agent(dirs: &[PathBuf]) -> String {
    let mut best = CLI_FLOOR;
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.take(256).flatten() {
            let name = entry.file_name();
            let Some(c) = name.to_str().and_then(|n| CLI_VERSION.captures(n)) else {
                continue;
            };
            let v = (
                c[1].parse().unwrap_or(0),
                c[2].parse().unwrap_or(0),
                c[3].parse().unwrap_or(0),
            );
            best = best.max(v);
        }
    }
    format!(
        "claude-cli/{}.{}.{} (external, cli)",
        best.0, best.1, best.2
    )
}

/// `reset_host(request)`: claims that never reached the POST answer `not-sent`.
pub(crate) async fn reset_host(request: &Value) -> Value {
    let mut started = false;
    let result = run(request, &mut started).await;
    if request["action"] == "claim" && !started {
        return match result["reason"].as_str() {
            Some(r) if LOGIN_REASONS.iter().any(|(_, w)| *w == r) || r == "rate-limited" => {
                json!({"outcome":"not-sent","reason":r})
            }
            _ => json!({"outcome":"not-sent"}),
        };
    }
    result
}
fn ambiguous() -> Value {
    json!({"unavailable":true,"outcome":"ambiguous"})
}
async fn run(request: &Value, started: &mut bool) -> Value {
    if super::keychain_bridge::needed() {
        if let Some(bridged) = super::keychain_bridge::run(request, started) {
            return bridged;
        }
        if *started {
            return ambiguous();
        }
    }
    match host(request, started).await {
        Ok(v) => v,
        Err(e) => match login_reason(e.status) {
            Some(r) => json!({"unavailable":true,"reason":r}),
            None => ambiguous(),
        },
    }
}
async fn host(request: &Value, started: &mut bool) -> Result<Value, Unavailable> {
    let Some(profile) = request.get("profile").filter(|_| request.is_object()) else {
        return Ok(ambiguous());
    };
    if request["action"] == "capture" {
        return capture(request);
    }
    if request.get("expectedEmail").is_none() {
        return Ok(ambiguous());
    }
    let mut captured: Vec<Private> = vec![];
    let codes = desktop::code_reset_logins(
        &request["expectedEmail"],
        &super::home(),
        super::now_ms() as f64,
    );
    let result = if codes.is_empty() {
        let organization = target_organization(request).map_err(|_| unavailable());
        let organization = match organization {
            Ok(o) => o,
            Err(_) => return Ok(ambiguous()),
        };
        let binding = request["expectedSavedBinding"].as_str().map(str::to_owned);
        if !request["expectedSavedBinding"].is_null() && binding.is_none() {
            Outcome {
                status: "unavailable",
                ..Default::default()
            }
        } else {
            collect_desktop(
                request,
                binding.as_deref(),
                organization.as_deref(),
                &mut captured,
            )
            .await
        }
    } else {
        let login = reset_logins(request, codes)?;
        if (!request["expectedSavedBinding"].is_null()
            && request["expectedSavedBinding"] != json!(login.binding))
            || (!request["expectedSavedOrganization"].is_null()
                && request["expectedSavedOrganization"] != json!(login.organization))
        {
            return Ok(json!({"unavailable":true}));
        }
        let mut result = Outcome {
            status: "needs_sign_in",
            ..Default::default()
        };
        if let Some(d) = &login.desktop {
            result = collect_desktop(
                request,
                Some(&d.binding),
                Some(&login.organization),
                &mut captured,
            )
            .await;
        }
        if ["needs_sign_in", "forbidden"].contains(&result.status) && captured.is_empty() {
            if login.code_ambiguous {
                return Ok(json!({"unavailable":true,"reason":"ambiguous-code-login"}));
            }
            if let Some(code) = &login.code {
                result = collect_code(code, request, &login.binding, &mut captured).await;
            }
        }
        if captured.len() == 1 {
            captured[0].checks.saved = Some((request.clone(), login.binding.clone()));
            captured[0].saved_binding = Some(login.binding.clone());
        }
        result
    };
    if result.status != "ok" || captured.len() != 1 {
        if let Some(r) = result.reset_reason {
            return Ok(json!({"unavailable":true,"reason":r}));
        }
        if result.rate_limited {
            return Ok(json!({"unavailable":true,"reason":"rate-limited"}));
        }
        if result.status == "needs_sign_in" {
            return Ok(json!({"unavailable":true,"reason":"signed-out"}));
        }
        return Ok(json!({"unavailable":true}));
    }
    let private = captured.remove(0);
    let cedar = &private.usage["cedar_ember"];
    if !cedar.is_object() || cedar["eligible"] != true || !cedar["grants"].is_array() {
        return Ok(json!({"unavailable":true}));
    }
    let identity = json!({"provider":"claude","account":private.account,"organization":private.organization,
        "profile":profile,"host":request.get("host").cloned().unwrap_or(Value::Null)});
    let binding = desktop::sha256(&desktop::py_dumps(&json!([identity, cedar]), true));
    let windows = normalize::reset_windows(&private.usage);
    let mut expiries = vec![];
    for grant in cedar["grants"].as_array().into_iter().flatten() {
        let raw = if grant.is_object() {
            &grant["ends_at"]
        } else {
            &Value::Null
        };
        let end = normalize::reset_at(raw);
        if !raw.is_null() && end.is_null() {
            return Ok(json!({"unavailable":true}));
        }
        expiries.push(
            normalize::reset_ms(&end)
                .map(Value::from)
                .unwrap_or(Value::Null),
        );
    }
    if request["action"] == "read" {
        private.checks.login()?;
        return Ok(
            json!({"identity":identity,"observedAtMs":private.observed_ms,"savedLoginBinding":private.saved_binding,
            "raw":cedar,"expiries":expiries,"windows":Value::Object(windows),"sourceBinding":binding,
            "accountLabel":request["expectedEmail"]}),
        );
    }
    let write = request.get("write").cloned().unwrap_or_else(|| json!({}));
    let Some(write) = write.as_object() else {
        return Ok(ambiguous());
    };
    let empty = Map::new();
    let body = write
        .get("body")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let body_value = write.get("body").cloned().unwrap_or_else(|| json!({}));
    if !body_value.is_object() && write.contains_key("body") {
        return Ok(ambiguous());
    }
    let keys_ok = body.len() == 3
        && ["program", "grant_id", "request_id"]
            .iter()
            .all(|k| body.contains_key(*k));
    if request["action"] != "claim"
        || !write
            .get("identity")
            .is_some_and(|i| normalize::py_eq(i, &identity))
        || write.get("sourceBinding") != Some(&json!(binding))
        || write.get("method") != Some(&json!("claude.reset_rate_limits"))
        || !keys_ok
        || body.get("program") != Some(&json!("cedar_ember"))
        || !body
            .get("grant_id")
            .and_then(Value::as_str)
            .is_some_and(|g| GRANT_ID.is_match(g))
        || !body
            .get("request_id")
            .and_then(Value::as_str)
            .is_some_and(|r| REQUEST_ID.is_match(r))
    {
        return Ok(json!({"outcome":"identity-changed"}));
    }
    let matches: Vec<&Value> = cedar["grants"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|g| g.is_object() && g.get("id") == body.get("grant_id"))
        .collect();
    if matches.len() != 1 || matches[0]["usable_now"] != true || matches[0]["paused"] != false {
        return Ok(json!({"outcome":"unavailable"}));
    }
    if !desktop::is_uuid(&private.organization) {
        return Ok(json!({"outcome":"identity-changed"}));
    }
    if !desktop::token_ok(&private.token) {
        return Ok(json!({"outcome":"unavailable"}));
    }
    let dirs = match &private.version_dirs {
        Some(d) => d.clone(),
        None => {
            let root = desktop::resolve_profile_path(
                profile,
                &request["profileDir"],
                &super::home(),
                request["isDefault"] == true,
            )
            .map_err(|_| unavailable());
            let Ok(root) = root else {
                return Ok(ambiguous());
            };
            vec![
                super::home().join(".local/share/claude/versions"),
                root.join("claude-code"),
            ]
        }
    };
    let user_agent = claim_user_agent(&dirs);
    private.checks.root()?;
    private.checks.login()?;
    // Python's json.dumps(body, separators=(",", ":")) keeps the request's own key order.
    let payload = desktop::py_dumps(&body_value, false).into_bytes();
    let Some(client) = Client::new(Duration::from_secs(10), Duration::from_secs(8)) else {
        return Ok(ambiguous());
    };
    *started = true;
    match client
        .claim(&private.organization, &private.token, payload, &user_agent)
        .await
    {
        Err(()) => Ok(ambiguous()),
        Ok((status, answer, _)) if (200..300).contains(&status) => Ok(match answer {
            Some(a) => json!({"outcome":normalize::claim_outcome(&a)}),
            None => ambiguous(),
        }),
        Ok((429, _, retry_after)) => Ok(
            json!({"outcome":"rate-limited","retryAfterMs":retry_after.filter(|r| *r != 0).unwrap_or(60) * 1000}),
        ),
        Ok((401 | 403, _, _)) => Ok(json!({"outcome":"signed-out"})),
        Ok(_) => Ok(json!({"outcome":"ambiguous"})),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn claim_user_agent_names_the_newest_installed_cli_above_the_floor() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            claim_user_agent(&[dir.path().join("missing")]),
            "claude-cli/2.1.325 (external, cli)"
        );
        for name in ["2.1.300", "2.2.1", "10.0.0.1", "notes", "2.10.3"] {
            std::fs::create_dir_all(dir.path().join(name)).unwrap();
        }
        assert_eq!(
            claim_user_agent(&[dir.path().to_path_buf()]),
            "claude-cli/2.10.3 (external, cli)"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_claim_that_never_reaches_the_provider_is_not_sent() {
        let request = json!({"action":"claim","profile":"work","expectedEmail":"user@example.test","host":"windows:x",
            "write":{"identity":{"provider":"claude","account":"a","organization":"o","profile":"work","host":"windows:x"}}});
        assert_eq!(reset_host(&request).await, json!({"outcome":"not-sent"}));
        let read = json!({"action":"read","profile":"work","expectedEmail":"user@example.test","host":"windows:x"});
        assert_eq!(reset_host(&read).await["unavailable"], true);
        assert_eq!(
            reset_host(&json!({"action":"read"})).await,
            json!({"unavailable":true,"outcome":"ambiguous"})
        );
    }
}
