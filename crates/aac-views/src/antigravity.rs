//! GET /api/antigravity/profiles and /api/antigravity/profiles/quotas.
//!
//! [`inventory`] ports `publicInventory` (`src/web-server/routes/antigravity-routes.ts`) with
//! `publicRuntimeServiceProblem` and `isNativeVersion`; [`quotas`] ports `publicQuotas`:
//! `publicDashboardAccount`, `poolWindow`, `displayText`, `email`, `timestamp` and
//! `antigravityPlanDisplay` (`src/antigravity/usage-normalization.ts`, `plan.ts`) with
//! `markPassedResets` (`account-window-reset.ts`). Inputs are the runtime's inventory and
//! accounts as TS read them, in the ISO-only presentation envelope: `Date.parse` epochs and
//! `toISOString` words come from its facts. Every input TS throws on (a 500) is a [`Refuse`].
use crate::js::{ascii_word, email_shape, has_control, is_true, trim};
use crate::spread::{Admit, Refuse, req, truthy, utf16_len};
use aac_status::read_context::{Context, Envelope, js_whitespace};
use serde_json::{Map, Value, json};

type Object = Map<String, Value>;

/// 2000-01-01T00:00:00Z and 2201-01-01T00:00:00Z: `timestamp` keeps UTC years 2000..=2200.
const YEAR_2000: i64 = 946_684_800_000;
const YEAR_2201: i64 = 7_289_654_400_000;
/// `Date.parse('2026-12-01T00:00:00Z')`, plan.ts `MODEL_REMOVAL_AT`.
const MODEL_REMOVAL_AT: f64 = 1_796_083_200_000.0;
const MAX_WINDOWS: usize = 256;
const MAX_MODEL_IDS: usize = 64;

/// `displayText`'s secret pattern, `/(?:bearer\s|dca:|sk[-_]|eyJ[A-Za-z0-9_-]{8}|https?:\/\/|
/// [{}]|access_token|refresh_token|client_secret)/i`. Without the `u` flag, case folding maps
/// no non-ASCII character onto an ASCII letter, so ASCII lowercasing decides it exactly.
fn secret(text: &str) -> bool {
    let lower: Vec<char> = text.chars().map(|c| c.to_ascii_lowercase()).collect();
    let at = |index: usize, word: &str| {
        word.chars()
            .enumerate()
            .all(|(offset, c)| lower.get(index + offset) == Some(&c))
    };
    (0..lower.len()).any(|i| {
        matches!(lower[i], '{' | '}')
            || [
                "dca:",
                "sk-",
                "sk_",
                "http://",
                "https://",
                "access_token",
                "refresh_token",
                "client_secret",
            ]
            .iter()
            .any(|word| at(i, word))
            || (at(i, "bearer") && lower.get(i + 6).is_some_and(|c| js_whitespace(*c)))
            || (at(i, "eyj")
                && lower.len() >= i + 11
                && lower[i + 3..i + 11].iter().all(|c| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-'
                }))
    })
}

/// Match the shared dashboard usage normalization rule `displayText(value, maximum)`.
pub fn display_text(value: Option<&Value>, maximum: usize) -> Option<String> {
    let text = value?.as_str()?;
    if utf16_len(text) > maximum || has_control(text) || secret(text) {
        return None;
    }
    let trimmed = trim(text);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn text_or_null(text: Option<String>) -> Value {
    text.map_or(Value::Null, Value::String)
}

/// `email(value)`.
fn email(value: Option<&Value>) -> Option<String> {
    display_text(value, 254).filter(|candidate| email_shape(candidate))
}

/// `safeId`.
fn safe_id(value: Option<&Value>) -> Option<&str> {
    value?.as_str().filter(|id| {
        ascii_word(
            id,
            |b| b.is_ascii_alphanumeric(),
            |b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-',
            63,
        )
    })
}

/// `safeProviderId` (`max` 160, extra `_.:/@-`) and `safePoolId` (`max` 128, extra `._:@/-`).
fn safe_key(value: Option<&Value>, max: usize) -> Option<&str> {
    let text = value?.as_str()?;
    let shaped = ascii_word(
        text,
        |b| b.is_ascii_alphanumeric(),
        |b| b.is_ascii_alphanumeric() || b"_.:/@-".contains(&b),
        max - 1,
    );
    (shaped && display_text(value, max).is_some()).then_some(text)
}

/// `/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/`.
fn timestamp_shape(text: &str) -> bool {
    let b = text.as_bytes();
    let digits = |range: std::ops::Range<usize>| {
        b.get(range)
            .is_some_and(|s| s.iter().all(u8::is_ascii_digit))
    };
    if b.len() < 20
        || !digits(0..4)
        || b[4] != b'-'
        || !digits(5..7)
        || b[7] != b'-'
        || !digits(8..10)
        || b[10] != b'T'
        || !digits(11..13)
        || b[13] != b':'
        || !digits(14..16)
        || b[16] != b':'
        || !digits(17..19)
    {
        return false;
    }
    let mut at = 19;
    if b[at] == b'.' {
        let start = at + 1;
        at = start;
        while at < b.len() && b[at].is_ascii_digit() {
            at += 1;
        }
        if at == start {
            return false;
        }
    }
    match b.get(at) {
        Some(b'Z') => at + 1 == b.len(),
        Some(b'+' | b'-') => {
            at + 6 == b.len()
                && digits(at + 1..at + 3)
                && b[at + 3] == b':'
                && digits(at + 4..at + 6)
        }
        _ => false,
    }
}

/// `timestamp(value)`: the `toISOString` word and its epoch, `None` for its null.
/// One instant reader: the envelope's facts, or the pure ISO parse for runtime words.
type TimeSource<'a> = &'a dyn Fn(Option<&Value>) -> Admit<Option<(String, i64)>>;

/// `timestamp(value)` from the envelope: an ISO word and the epoch fact its presentation carries.
fn timestamp_fact(ctx: &Context, value: Option<&Value>) -> Admit<Option<(String, i64)>> {
    let Some(text) = value.and_then(Value::as_str) else {
        return Ok(None);
    };
    if utf16_len(text) > 40 || !timestamp_shape(text) {
        return Ok(None);
    }
    let Some(ms) = crate::js::date_parse(ctx, text) else {
        return Ok(None);
    };
    let Some(ms) = ms else {
        return Ok(None);
    };
    if !(YEAR_2000..YEAR_2201).contains(&ms) {
        return Ok(None);
    }
    let word = ctx.iso(Some(ms));
    Ok(Some((req(word.as_str())?.to_owned(), ms)))
}

/// `timestamp(value)` for a caller whose words are already canonical ISO: a pure `Date` parse.
fn timestamp_pure(value: Option<&Value>) -> Admit<Option<(String, i64)>> {
    Ok(canonical_timestamp(value).and_then(|word| {
        crate::js::parse_date(&word)
            .ok()
            .flatten()
            .map(|ms| (word, ms))
    }))
}

fn time_value(time: Option<&(String, i64)>) -> Value {
    time.map_or(Value::Null, |(word, _)| json!(word))
}

/// `nonnegative(value)`: the number itself (its JSON spelling is kept).
fn nonnegative(value: Option<&Value>) -> Option<&Value> {
    value.filter(|v| v.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0))
}

fn percent(value: Option<&Value>) -> Option<&Value> {
    nonnegative(value).filter(|v| v.as_f64().is_some_and(|n| n <= 100.0))
}

fn or_null(value: Option<&Value>) -> Value {
    value.cloned().unwrap_or(Value::Null)
}

/// `100 - percent`, or null.
fn complement(value: Option<&Value>) -> Value {
    // `100 - n` is a JavaScript number: an integral result stringifies without a fraction.
    value.and_then(Value::as_f64).map_or(Value::Null, |n| {
        let rest = 100.0 - n;
        if rest.fract() == 0.0 {
            json!(rest as i64)
        } else {
            json!(rest)
        }
    })
}

/// `poolWindow(value)` with its reset epoch; `None` is a window it drops.
fn pool_window_inner(value: &Value, time: TimeSource<'_>) -> Admit<Option<(Object, Option<i64>)>> {
    let Some(window) = value.as_object() else {
        return Ok(None);
    };
    let Some(key) = safe_key(window.get("key"), 160) else {
        return Ok(None);
    };
    let used_percent = percent(window.get("usedPercent"));
    let remaining_percent = percent(window.get("remainingPercent"));
    let used = nonnegative(window.get("used"));
    let limit = nonnegative(window.get("limit"));
    let remaining = nonnegative(window.get("remaining"));
    let reset_at = time(window.get("resetAt"))?;
    let expires_at = time(window.get("expiresAt"))?;
    let enabled = window.get("enabled").filter(|v| v.is_boolean());
    let unlimited = window.get("unlimited").filter(|v| v.is_boolean());
    if used_percent.is_none()
        && remaining_percent.is_none()
        && used.is_none()
        && limit.is_none()
        && remaining.is_none()
        && reset_at.is_none()
        && expires_at.is_none()
        && unlimited.is_none()
        && enabled.is_none()
    {
        return Ok(None);
    }
    // `String(value.kind)`: an array could join to one of the words; that coercion is TS's.
    let kind = match window.get("kind") {
        Some(Value::Array(_)) => return Err(Refuse),
        Some(Value::String(kind))
            if ["rate_limit", "balance", "spend", "extra_usage"].contains(&kind.as_str()) =>
        {
            json!(kind)
        }
        _ => json!("rate_limit"),
    };
    let minutes = nonnegative(window.get("windowMinutes"))
        .filter(|v| v.as_f64().is_some_and(|n| n > 0.0 && n <= 525_600.0));
    let mut model_ids: Vec<&str> = Vec::new();
    if let Some(Value::Array(ids)) = window.get("modelIds") {
        for id in ids {
            if let Some(id) = safe_key(Some(id), 160)
                && !model_ids.contains(&id)
            {
                model_ids.push(id);
            }
        }
        model_ids.truncate(MAX_MODEL_IDS);
    }
    let mut out = Object::new();
    out.insert("key".into(), json!(key));
    out.insert(
        "label".into(),
        if key == "google-ai-credits" {
            json!("AI credits (overage)")
        } else {
            json!(display_text(window.get("label"), 80).unwrap_or_else(|| "Model quota".into()))
        },
    );
    out.insert(
        "usedPercent".into(),
        used_percent
            .cloned()
            .unwrap_or_else(|| complement(remaining_percent)),
    );
    out.insert(
        "remainingPercent".into(),
        remaining_percent
            .cloned()
            .unwrap_or_else(|| complement(used_percent)),
    );
    out.insert("resetAt".into(), time_value(reset_at.as_ref()));
    out.insert("windowMinutes".into(), or_null(minutes));
    out.insert("used".into(), or_null(used));
    out.insert("limit".into(), or_null(limit));
    out.insert(
        "unit".into(),
        text_or_null(display_text(window.get("unit"), 24)),
    );
    out.insert("kind".into(), kind);
    // Own members: TS refuses to send an own member whose value is undefined.
    if window.contains_key("remaining") {
        out.insert("remaining".into(), or_null(remaining));
    }
    if window.contains_key("expiresAt") {
        out.insert("expiresAt".into(), time_value(expires_at.as_ref()));
    }
    if let Some(enabled) = enabled {
        out.insert("enabled".into(), enabled.clone());
    }
    if let Some(unlimited) = unlimited {
        out.insert("unlimited".into(), unlimited.clone());
    }
    if let Some(pool) = safe_key(window.get("poolId"), 128) {
        out.insert("poolId".into(), json!(pool));
        if let Some(Value::String(source)) = window.get("poolIdSource")
            && (source == "provider-id" || source == "provider-bucket-membership")
        {
            out.insert("poolIdSource".into(), json!(source));
        }
        if let Some(label) = display_text(window.get("poolLabel"), 80) {
            out.insert("poolLabel".into(), json!(label));
        }
    }
    if !model_ids.is_empty() {
        out.insert("modelIds".into(), json!(model_ids));
    }
    Ok(Some((out, reset_at.map(|(_, ms)| ms))))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Models {
    Consumer,
    Trial,
    PaidConsumer,
    UltraConsumer,
    Gemini,
    Flash,
}

struct Plan {
    class: &'static str,
    label: &'static str,
    aliases: &'static [&'static str],
    policy: &'static str,
    summary: &'static str,
    models: Models,
    credits: bool,
}

const STARTER: &str = "Weekly quota only; no 5-hour refresh.";
const NO_QUOTA: &str = "This plan does not include Antigravity coding quota.";
const CLI_QUOTA: &str = "This plan includes separate Gemini CLI quota that AAC does not read; no Antigravity coding quota.";
const POOLED: &str =
    "Gemini models use pooled edition seat credits; Claude models need administrator opt-in.";
const FLASH_MODELS: [&str; 1] = ["Gemini 3.8 Flash"];
/// Paid Google AI Pro (not a trial): docs/models, 2026-11-19.
const PRO_GEMINI_MODELS: [&str; 6] = [
    "Gemini 4 Neon",
    "Gemini 4 Argon",
    "Gemini 4 Hydrogen",
    "Orchestrator (Neon)",
    "Orchestrator (Argon)",
    "Gemini 3.8 Flash",
];
const ULTRA_GEMINI_MODELS: [&str; 8] = [
    "Gemini 4 Neon",
    "Gemini 4 Carbon",
    "Gemini 4 Argon",
    "Gemini 4 Hydrogen",
    "Orchestrator (Neon)",
    "Orchestrator (Carbon)",
    "Orchestrator (Argon)",
    "Gemini 3.8 Flash",
];
const ENTERPRISE_GEMINI_MODELS: [&str; 8] = [
    "Gemini 4 Neon",
    "Gemini 4 Carbon",
    "Gemini 4 Argon",
    "Gemini 4 Hydrogen",
    "Gemini 3.8 Flash",
    "Gemini 3.7 Flash",
    "Gemini 3.6 Flash",
    "Gemini 3.1 Pro",
];
const RETIRING_MODELS: [&str; 3] = ["Claude Sonnet 4.6", "Claude Opus 4.6", "GPT-OSS-120B"];
const PAID_CLAUDE_MODELS: [&str; 2] = ["Claude Sonnet 5.5", "Claude Opus 5.5"];

/// plan.ts `PLANS`, in its key order. `tests/spec/profile-read-parity.ts` compares every label
/// and alias (and respellings) with the running TS `antigravityPlanDisplay`.
const PLANS: &[Plan] = &[
    Plan {
        class: "free",
        label: "Free",
        aliases: &["free-tier", "Antigravity Starter Quota"],
        policy: "weekly",
        summary: STARTER,
        models: Models::Consumer,
        credits: false,
    },
    Plan {
        class: "plus",
        label: "Google AI Plus",
        aliases: &["plus"],
        policy: "weekly",
        summary: STARTER,
        models: Models::Consumer,
        credits: false,
    },
    Plan {
        class: "pro-trial",
        label: "Google AI Pro (trial)",
        aliases: &["pro-trial", "Google AI Pro trial"],
        policy: "five-hour-weekly",
        summary: "Refreshes every 5 hours up to a weekly limit. Gemini 4 Neon, Gemini 4 Argon and their Orchestrators only on trials with eligible premium devices; no Gemini 4 Hydrogen or Claude 5.5 during a trial.",
        models: Models::Trial,
        credits: false,
    },
    Plan {
        class: "pro",
        label: "Google AI Pro",
        aliases: &["pro", "Gemini Code Assist in Google One AI Pro"],
        policy: "five-hour-weekly",
        summary: "Refreshes every 5 hours up to a weekly limit. Gemini models share one limit that costlier models use up faster (Gemini 4 Neon and Hydrogen count against it); Gemini 4 Argon has its own weekly limit; Claude and GPT models have their own.",
        models: Models::PaidConsumer,
        credits: true,
    },
    Plan {
        class: "ultra",
        label: "Google AI Ultra",
        aliases: &["ultra", "Gemini Code Assist in Google One AI Ultra"],
        policy: "five-hour-weekly",
        summary: "Refreshes every 5 hours up to a weekly limit. Gemini models share one limit that costlier models use up faster; Claude and GPT models have their own.",
        models: Models::UltraConsumer,
        credits: true,
    },
    Plan {
        class: "ultra-5x",
        label: "Google AI Ultra 5x",
        aliases: &["ultra-5x"],
        policy: "five-hour-weekly",
        summary: "5x Google AI Pro capacity; refreshes every 5 hours up to a weekly limit. Gemini models share one limit that costlier models use up faster; Claude and GPT models have their own.",
        models: Models::UltraConsumer,
        credits: true,
    },
    Plan {
        class: "ultra-20x",
        label: "Google AI Ultra 20x",
        aliases: &["ultra-20x"],
        policy: "five-hour-weekly",
        summary: "20x Google AI Pro capacity; refreshes every 5 hours up to a weekly limit. Gemini models share one limit that costlier models use up faster; Claude and GPT models have their own.",
        models: Models::UltraConsumer,
        credits: true,
    },
    Plan {
        class: "enterprise-standard",
        label: "Gemini Enterprise Standard",
        aliases: &[],
        policy: "pooled-7d",
        summary: POOLED,
        models: Models::Gemini,
        credits: false,
    },
    Plan {
        class: "enterprise-plus",
        label: "Gemini Enterprise Plus",
        aliases: &[],
        policy: "pooled-7d",
        summary: POOLED,
        models: Models::Gemini,
        credits: false,
    },
    Plan {
        class: "enterprise-emerging",
        label: "Gemini Enterprise Standard Emerging Market",
        aliases: &[],
        policy: "none",
        summary: "Antigravity is available, but this plan includes no bundled coding quota.",
        models: Models::Gemini,
        credits: false,
    },
    Plan {
        class: "enterprise-payg",
        label: "Gemini Enterprise Pay-as-you-go",
        aliases: &[],
        policy: "metered",
        summary: "Gemini models use metered Antigravity consumption.",
        models: Models::Gemini,
        credits: false,
    },
    Plan {
        class: "enterprise-business",
        label: "Gemini Enterprise Business",
        aliases: &[],
        policy: "none",
        summary: NO_QUOTA,
        models: Models::Gemini,
        credits: false,
    },
    Plan {
        class: "code-assist-standard",
        label: "Gemini Code Assist Standard",
        aliases: &[],
        policy: "none",
        summary: CLI_QUOTA,
        models: Models::Flash,
        credits: false,
    },
    Plan {
        class: "code-assist-enterprise",
        label: "Gemini Code Assist Enterprise",
        aliases: &[],
        policy: "none",
        summary: CLI_QUOTA,
        models: Models::Flash,
        credits: false,
    },
    Plan {
        class: "workspace",
        label: "Google Workspace",
        aliases: &[
            "Google Workspace Business Starter",
            "Google Workspace Business Standard",
            "Google Workspace Business Plus",
            "Google Workspace Enterprise Starter",
            "Google Workspace Enterprise Standard",
            "Google Workspace Enterprise Plus",
            "Workspace Business Starter",
            "Workspace Business Standard",
            "Workspace Business Plus",
            "Workspace Enterprise Starter",
            "Workspace Enterprise Standard",
            "Workspace Enterprise Plus",
        ],
        policy: "weekly",
        summary: "Antigravity Starter Quota: Weekly quota only; no 5-hour refresh. Workspace plans add no higher limits or AI credits.",
        models: Models::Consumer,
        credits: false,
    },
];

/// plan.ts `key(value)`: `toLowerCase`, `[()_-]` to a space, whitespace runs to one space, trim.
/// Of all non-ASCII characters only U+212A KELVIN SIGN lowercases to ASCII ('k'); any other one
/// stays non-ASCII and so can never equal an (ASCII) alias key.
fn plan_key(value: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in value.chars() {
        let c = match c {
            '\u{212a}' => 'k',
            '(' | ')' | '_' | '-' => ' ',
            c if js_whitespace(c) => ' ',
            c => c.to_ascii_lowercase(),
        };
        if c == ' ' {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    out
}

/// plan.ts `known(value)`: the alias map is filled in `PLANS` order, so a later entry wins.
fn known(value: &str) -> Option<&'static Plan> {
    let key = plan_key(value);
    let mut found = None;
    for plan in PLANS {
        if std::iter::once(plan.label)
            .chain(plan.aliases.iter().copied())
            .any(|alias| plan_key(alias) == key)
        {
            found = Some(plan);
        }
    }
    found
}

/// `antigravityPlanDisplay(plan, sampledAt)` without a reported plan: the `plan` member and the
/// optional `antigravityPlan`. `clock` is TS's `Date.now()` for a reading without a time.
fn plan_display(plan: Option<String>, sampled: Option<i64>, clock: f64) -> (Value, Option<Value>) {
    match plan.as_deref().and_then(known) {
        None => (text_or_null(plan), None),
        Some(definition) => display_for(definition, sampled, clock),
    }
}

/// The `plan` and optional `antigravityPlan` for one known plan definition.
fn display_for(
    definition: &'static Plan,
    sampled: Option<i64>,
    clock: f64,
) -> (Value, Option<Value>) {
    #[allow(clippy::cast_precision_loss)]
    let observed = sampled.map_or(clock, |ms| ms as f64);
    let before_removal = observed < MODEL_REMOVAL_AT;
    let mut third_party: Vec<&str> = Vec::new();
    if matches!(
        definition.models,
        Models::PaidConsumer | Models::UltraConsumer
    ) {
        third_party.extend(PAID_CLAUDE_MODELS);
    }
    if before_removal
        && matches!(
            definition.models,
            Models::Consumer | Models::Trial | Models::PaidConsumer
        )
    {
        third_party.extend(RETIRING_MODELS);
    }
    if before_removal && definition.models == Models::UltraConsumer {
        third_party.push("GPT-OSS-120B");
    }
    let mut models: Vec<&str> = Vec::new();
    match definition.models {
        Models::PaidConsumer => models.extend(PRO_GEMINI_MODELS),
        Models::UltraConsumer => models.extend(ULTRA_GEMINI_MODELS),
        Models::Gemini => models.extend(ENTERPRISE_GEMINI_MODELS),
        Models::Consumer | Models::Trial | Models::Flash => models.extend(FLASH_MODELS),
    }
    models.extend(third_party.iter().copied());
    (
        json!(definition.label),
        Some(json!({
            "class": definition.class,
            "quotaPolicy": definition.policy,
            "summary": definition.summary,
            "models": models,
            "thirdPartyModels": !third_party.is_empty(),
            "creditsOverage": definition.credits,
        })),
    )
}

/// `antigravityPlanDisplay(displayText(plan, 80), verifiedAt).plan`: the plan's label when plan.ts
/// knows it, else the accepted text; null when the text is refused or absent.
pub fn public_plan(plan: Option<&Value>) -> Value {
    plan_display(display_text(plan, 80), None, 0.0).0
}

/// `timestamp(profile.verifiedAt)` for a stored ISO word: canonical `toISOString`, or null.
fn canonical_timestamp(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    if utf16_len(text) > 40 || !timestamp_shape(text) {
        return None;
    }
    let ms = crate::js::parse_date(text).ok()??;
    if !(YEAR_2000..YEAR_2201).contains(&ms) {
        return None;
    }
    aac_num::ecma::to_iso_string(ms)
}

/// `publicProfile(profile)` (`usage-normalization.ts`) for the runtime's own row: the public
/// inventory member with every capability gated on the verified identity. `verifiedAt` is a stored
/// ISO word (registry identities are normalized on save).
pub fn public_profile(input: &Value) -> Option<Value> {
    let id = safe_id(input.get("id"))?;
    let email = email(input.get("email"))?;
    let flag = |key: &str| is_true(input.get(key));
    let verified = flag("identityVerified");
    Some(json!({
        "id": id,
        "email": email,
        "plan": public_plan(input.get("plan")),
        "available": verified && flag("available"),
        "selected": verified && flag("selected"),
        "runtimeVerified": verified && flag("runtimeVerified"),
        "verifiedAt": canonical_timestamp(input.get("verifiedAt")).map_or(Value::Null, Value::String),
        "hostId": "ubuntu",
    }))
}

/// `publicRuntimeServiceProblem(value)` for the runtime layer: the probe's bounded row, or none.
pub fn runtime_problem_value(value: Option<&Value>) -> Option<Value> {
    runtime_problem(value)
}

/// `isNativeVersion(value)` for the runtime layer: the reviewed version word, or null.
pub fn native_version_word(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|text| native_version(text))
        .map(str::to_owned)
}

/// `displayText(value, maximum)` for the runtime layer.
pub fn display_text_value(value: Option<&Value>, maximum: usize) -> Option<String> {
    display_text(value, maximum)
}

/// `email(value)` for the runtime layer.
pub fn email_value(value: Option<&Value>) -> Option<String> {
    email(value)
}

/// `timestamp(value)` for the runtime layer: the canonical ISO word, or null.
pub fn timestamp_word(value: Option<&Value>) -> Option<String> {
    canonical_timestamp(value)
}

/// `poolWindow(value)` (`usage-normalization.ts`) for the runtime layer: the internal window.
pub fn pool_window_value(value: &Value) -> Option<Value> {
    pool_window_inner(value, &timestamp_pure)
        .ok()
        .flatten()
        .map(|(window, _)| Value::Object(window))
}

/// `antigravityPlanDisplay(plan, sampledAt, reported)`: a reported tier refines a generic plan
/// type; an unknown plan passes through as its text. `clock` is TS's `Date.now()`.
pub fn plan_at(
    plan: Option<&str>,
    sampled_at: Option<&Value>,
    reported: Option<&Value>,
    clock: f64,
) -> (Value, Option<Value>) {
    let reported = reported.and_then(Value::as_object);
    let tier = reported
        .and_then(|row| {
            row.get("paidTier")
                .filter(|tier| !tier.is_null())
                .or_else(|| row.get("currentTier"))
        })
        .and_then(Value::as_object);
    let tier_class = tier
        .and_then(|row| row.get("id"))
        .and_then(Value::as_str)
        .and_then(known)
        .or_else(|| {
            tier.and_then(|row| row.get("name"))
                .and_then(Value::as_str)
                .and_then(known)
        });
    let mut plan_class = reported
        .and_then(|row| row.get("planType"))
        .and_then(Value::as_str)
        .and_then(known)
        .or(tier_class)
        .or_else(|| plan.and_then(known));
    // An explicit trial or multiplier refines a generic plan type within its family.
    if let (Some(found), Some(tier)) = (plan_class, tier_class)
        && ((found.class == "ultra" && tier.class.starts_with("ultra-"))
            || (found.class == "pro" && tier.class == "pro-trial"))
    {
        plan_class = Some(tier);
    }
    let Some(definition) = plan_class else {
        return (text_or_null(plan.map(str::to_owned)), None);
    };
    let sampled = match sampled_at {
        Some(Value::String(text)) => crate::js::parse_date(text).ok().flatten(),
        Some(Value::Number(number)) => number.as_f64().map(|n| n as i64),
        _ => None,
    };
    display_for(definition, sampled, clock)
}

/// `publicDashboardAccount(value)` then `markPassedResets(account, now)`.
fn account(value: &Value, now: f64, clock: f64, time: TimeSource<'_>) -> Admit<Value> {
    let value = req(value.as_object())?;
    let capabilities = value.get("capabilities").and_then(Value::as_object);
    let id = req(safe_id(
        capabilities.and_then(|c| c.get("antigravityProfileId")),
    ))?;
    let email = req(email(value.get("email")))?;
    if value.get("id").and_then(Value::as_str) != Some(&format!("antigravity:profile:{id}"))
        || value.get("provider").and_then(Value::as_str) != Some("antigravity")
        || value.get("platform").and_then(Value::as_str) != Some("ubuntu")
    {
        return Err(Refuse);
    }
    let capabilities = req(capabilities)?;
    let text = |key: &str| value.get(key).and_then(Value::as_str);
    let status = text("status")
        .filter(|s| ["ok", "cached", "needs_sign_in", "unavailable", "error"].contains(s))
        .unwrap_or("error");
    let identity_unbound = status == "error" && text("statusReason") == Some("identity_unbound");
    let fetched_at = time(value.get("fetchedAt"))?;
    let sampled_at = time(value.get("sampledAt"))?;
    let observed = sampled_at
        .as_ref()
        .or(fetched_at.as_ref())
        .map(|(_, ms)| *ms);
    let (plan, plan_detail) = plan_display(display_text(value.get("plan"), 80), observed, clock);
    let cache_reason = text("cacheReason");
    let host_ids = match capabilities.get("antigravityHostIds") {
        None | Some(Value::Null) => false,
        Some(Value::Array(ids)) => ids.iter().any(|id| id.as_str() == Some("ubuntu")),
        Some(Value::String(ids)) => ids.contains("ubuntu"),
        // A value without an `includes` method makes TS throw.
        Some(_) => return Err(Refuse),
    };
    let mut windows = Vec::new();
    if let Some(Value::Array(list)) = value.get("windows") {
        for window in list.iter().take(MAX_WINDOWS) {
            if let Some((mut window, reset)) = pool_window_inner(window, time)? {
                // readingPredatesReset(resetAt, account.sampledAt, now); a window has no sampledAt.
                let passed = crate::js::reading_predates_reset(
                    reset,
                    sampled_at.as_ref().map(|(_, sampled)| *sampled),
                    now,
                );
                if passed {
                    window.insert("resetPassed".into(), json!(true));
                }
                windows.push(Value::Object(window));
            }
        }
    }
    let mut out = Object::new();
    out.insert("id".into(), json!(format!("antigravity:profile:{id}")));
    out.insert("provider".into(), json!("antigravity"));
    out.insert("providerLabel".into(), json!("Antigravity"));
    out.insert("label".into(), json!(email));
    out.insert("email".into(), json!(email));
    out.insert("plan".into(), plan);
    if let Some(detail) = plan_detail {
        out.insert("antigravityPlan".into(), detail);
    }
    out.insert("platform".into(), json!("ubuntu"));
    out.insert("source".into(), json!("Antigravity saved login on Ubuntu"));
    out.insert("status".into(), json!(status));
    if identity_unbound {
        out.insert("statusReason".into(), json!("identity_unbound"));
    }
    if status == "cached" {
        let reason = cache_reason
            .filter(|r| {
                [
                    "read_failed",
                    "rate_limited",
                    "refresh_pending",
                    "source_changed",
                ]
                .contains(r)
            })
            .unwrap_or("outcome_unknown");
        out.insert("cacheReason".into(), json!(reason));
    }
    let message = match status {
        "ok" => Value::Null,
        "cached" => json!(match cache_reason {
            Some("rate_limited") =>
                "Antigravity refresh is rate limited; showing the last successful reading.",
            Some("source_changed") =>
                "The new sign-in is being read; showing the last successful Antigravity reading.",
            Some("read_failed") =>
                "The Antigravity usage read failed; showing the last successful reading.",
            _ =>
                "The previous Antigravity read outcome was not recorded; showing the saved reading.",
        }),
        "needs_sign_in" => json!("This Antigravity profile needs to sign in again."),
        _ if identity_unbound => {
            json!("Antigravity usage could not be matched to this saved account.")
        }
        _ => json!("Antigravity usage is temporarily unavailable."),
    };
    out.insert("message".into(), message);
    out.insert("fetchedAt".into(), time_value(fetched_at.as_ref()));
    out.insert("sampledAt".into(), time_value(sampled_at.as_ref()));
    out.insert("isActive".into(), json!(is_true(value.get("isActive"))));
    out.insert("windows".into(), Value::Array(windows));
    out.insert(
        "capabilities".into(),
        json!({
            "codexProfile": null,
            "claudeProfileId": null,
            "claudePlatforms": [],
            "antigravityProfileId": id,
            "antigravityHostIds": if host_ids { json!(["ubuntu"]) } else { json!([]) },
        }),
    );
    Ok(Value::Object(out))
}

/// The usage answer from `{accounts, now, clock}` in the presentation envelope.
pub fn quotas(envelope: Envelope) -> Admit<Value> {
    let ctx = Context::new(envelope.presentation);
    let input = req(envelope.data.as_object())?;
    let now = req(input.get("now").and_then(Value::as_f64))?;
    let clock = req(input.get("clock").and_then(Value::as_f64))?;
    let accounts = req(input.get("accounts").and_then(Value::as_array))?
        .iter()
        .map(|value| account(value, now, clock, &|value| timestamp_fact(&ctx, value)))
        .collect::<Admit<Vec<Value>>>()?;
    if ctx.valid() {
        Ok(json!({ "schemaVersion": 1, "hostId": "ubuntu", "accounts": accounts }))
    } else {
        Err(Refuse)
    }
}

/// The usage answer from accounts whose timestamps are already canonical ISO words: the runtime
/// layer's own accounts, with no envelope facts to consult.
pub fn quotas_words(accounts: &[Value], now: f64, clock: f64) -> Option<Value> {
    let rows = accounts
        .iter()
        .map(|value| account(value, now, clock, &timestamp_pure))
        .collect::<Admit<Vec<Value>>>()
        .ok()?;
    Some(json!({ "schemaVersion": 1, "hostId": "ubuntu", "accounts": rows }))
}

fn pattern_text(value: Option<&Value>, ok: fn(&str) -> bool) -> Value {
    value
        .and_then(Value::as_str)
        .filter(|text| ok(text))
        .map_or(Value::Null, |text| json!(text))
}

/// `/^\d{1,2}\.\d{1,3}$/`.
fn minor(text: &str) -> bool {
    text.split_once('.').is_some_and(|(major, minor)| {
        (1..=2).contains(&major.len())
            && (1..=3).contains(&minor.len())
            && major
                .bytes()
                .chain(minor.bytes())
                .all(|b| b.is_ascii_digit())
    })
}

/// `publicRuntimeServiceProblem(value)`.
fn runtime_problem(value: Option<&Value>) -> Option<Value> {
    let problem = value?.as_object()?;
    let reason = problem.get("reason")?.as_str()?;
    if ![
        "missing-python-module",
        "parser-mismatch",
        "parser-import-failed",
        "service-failed",
    ]
    .contains(&reason)
    {
        return None;
    }
    let module = |text: &str| {
        ascii_word(
            text,
            |b| b.is_ascii_alphabetic() || b == b'_',
            |b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.',
            63,
        )
    };
    let exit = problem.get("exitStatus").filter(|v| {
        v.as_f64()
            .is_some_and(|n| n.fract() == 0.0 && (0.0..=255.0).contains(&n))
    });
    Some(json!({
        "reason": reason,
        "module": pattern_text(problem.get("module"), module),
        "python": pattern_text(problem.get("python"), minor),
        "builtFor": pattern_text(problem.get("builtFor"), minor),
        "exitStatus": or_null(exit),
    }))
}

/// `isNativeVersion`: `/^\d+\.\d+\.\d+(?:-[\w.-]+)?$/`, at most 128 characters.
fn native_version(text: &str) -> bool {
    let (core, tail) = text
        .split_once('-')
        .map_or((text, None), |(c, t)| (c, Some(t)));
    let parts: Vec<&str> = core.split('.').collect();
    text.len() <= 128
        && parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        && tail.is_none_or(|t| {
            !t.is_empty()
                && t.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        })
}

/// The public inventory from the runtime's inventory in the presentation envelope.
pub fn inventory(envelope: Envelope) -> Admit<Value> {
    let ctx = Context::new(envelope.presentation);
    let input = req(envelope.data.as_object())?;
    let rows = req(input.get("profiles").and_then(Value::as_array))?;
    if input.get("hostId").and_then(Value::as_str) != Some("ubuntu") || rows.len() > 16 {
        return Err(Refuse);
    }
    let mut out = Object::new();
    out.insert("schemaVersion".into(), json!(1));
    out.insert("hostId".into(), json!("ubuntu"));
    let paused = input.get("nativeUpdatePaused");
    if truthy(paused) {
        let installed = paused
            .and_then(Value::as_object)
            .and_then(|p| p.get("installedVersion"));
        out.insert(
            "nativeUpdatePaused".into(),
            json!({ "installedVersion": pattern_text(installed, native_version) }),
        );
    }
    if let Some(problem) = runtime_problem(input.get("runtimeServiceProblem")) {
        out.insert("runtimeServiceProblem".into(), problem);
    }
    let mut ids: Vec<&str> = Vec::new();
    let mut profiles = Vec::new();
    for row in rows {
        let row = req(row.as_object())?;
        let id = req(safe_id(row.get("id")))?;
        let email = req(email(row.get("email")))?;
        if ids.contains(&id) || row.get("hostId").and_then(Value::as_str) != Some("ubuntu") {
            return Err(Refuse);
        }
        ids.push(id);
        let verified = timestamp_fact(&ctx, row.get("verifiedAt"))?;
        profiles.push(json!({
            "id": id,
            "email": email,
            "plan": text_or_null(display_text(row.get("plan"), 80)),
            "hostId": "ubuntu",
            "available": is_true(row.get("available")),
            "selected": is_true(row.get("selected")),
            "runtimeVerified": is_true(row.get("runtimeVerified")),
            "verifiedAt": time_value(verified.as_ref()),
        }));
    }
    out.insert("profiles".into(), Value::Array(profiles));
    if ctx.valid() {
        Ok(Value::Object(out))
    } else {
        Err(Refuse)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_gates_match_display_text() {
        for (text, kept) in [
            ("  Pro ", Some("Pro")),
            ("Bearer\u{a0}x", None),
            ("bearer", Some("bearer")),
            ("SK_live", None),
            ("ſk-x", Some("ſk-x")),
            ("eyJabcdefgh", None),
            ("eyJabcdefg", Some("eyJabcdefg")),
            ("eyJabcd\u{e9}fgh", Some("eyJabcd\u{e9}fgh")),
            ("HTTPS://x", None),
            ("{x}", None),
            ("Client_Secret", None),
            ("\u{feff}\u{3000}", None),
            ("a\u{1f}", None),
            ("Plan \u{1F680}", Some("Plan \u{1F680}")),
        ] {
            assert_eq!(
                display_text(Some(&json!(text)), 80).as_deref(),
                kept,
                "{text:?}"
            );
        }
        for (text, shaped) in [
            ("2026-11-15T10:00:00Z", true),
            ("2026-11-15T10:00:00.123456+05:30", true),
            ("2026-11-15T10:00:00.Z", false),
            ("2026-11-15T10:00:00+0530", false),
            ("2026-11-15T10:00Z", false),
            ("2026-11-15 10:00:00Z", false),
        ] {
            assert_eq!(timestamp_shape(text), shaped, "{text}");
        }
        assert!(
            native_version("1.2.3")
                && native_version("10.0.0-rc.1_x")
                && !native_version("1.2")
                && !native_version("1.2.3-")
        );
        assert!(minor("3.12") && !minor("3.") && !minor("123.1"));
    }

    #[test]
    fn plan_keys_and_last_alias_wins() {
        assert_eq!(plan_key("  Google_AI-Pro (Trial) "), "google ai pro trial");
        assert_eq!(
            known("google ai pro TRIAL").map(|p| p.class),
            Some("pro-trial")
        );
        assert_eq!(
            known("Google Wor\u{212a}space").map(|p| p.class),
            Some("workspace")
        );
        assert!(known("Google Workspace\u{e9}").is_none());
        let (plan, detail) = plan_display(Some("pro".into()), Some(1_796_083_199_999), 0.0);
        assert_eq!(plan, json!("Google AI Pro"));
        assert_eq!(
            detail.expect("pro plan")["models"],
            json!([
                "Gemini 4 Neon",
                "Gemini 4 Argon",
                "Gemini 4 Hydrogen",
                "Orchestrator (Neon)",
                "Orchestrator (Argon)",
                "Gemini 3.8 Flash",
                "Claude Sonnet 5.5",
                "Claude Opus 5.5",
                "Claude Sonnet 4.6",
                "Claude Opus 4.6",
                "GPT-OSS-120B"
            ])
        );
    }

    /// plan.test.ts: the 2026-11-19 model table per tier, and the end-of-Nov-30 removal.
    #[test]
    fn tiers_follow_the_2026_11_19_model_table() {
        let models = |plan: &str, at: i64| {
            plan_display(Some(plan.into()), Some(at), 0.0)
                .1
                .expect(plan)
        };
        let before = 1_796_083_199_999;
        let after = 1_796_083_200_000;
        for plan in ["free", "plus", "pro-trial", "Google Workspace"] {
            assert_eq!(
                models(plan, before)["models"],
                json!([
                    "Gemini 3.8 Flash",
                    "Claude Sonnet 4.6",
                    "Claude Opus 4.6",
                    "GPT-OSS-120B"
                ])
            );
            assert_eq!(models(plan, after)["models"], json!(["Gemini 3.8 Flash"]));
            assert_eq!(models(plan, after)["thirdPartyModels"], json!(false));
        }
        assert_eq!(
            models("Google Workspace", after)["quotaPolicy"],
            json!("weekly")
        );
        for plan in ["ultra", "ultra-5x", "ultra-20x"] {
            assert_eq!(
                models(plan, after)["models"],
                json!([
                    "Gemini 4 Neon",
                    "Gemini 4 Carbon",
                    "Gemini 4 Argon",
                    "Gemini 4 Hydrogen",
                    "Orchestrator (Neon)",
                    "Orchestrator (Carbon)",
                    "Orchestrator (Argon)",
                    "Gemini 3.8 Flash",
                    "Claude Sonnet 5.5",
                    "Claude Opus 5.5"
                ])
            );
            assert_eq!(models(plan, before)["models"][10], json!("GPT-OSS-120B"));
        }
        assert_eq!(
            models("Gemini Enterprise Plus", after)["models"],
            json!([
                "Gemini 4 Neon",
                "Gemini 4 Carbon",
                "Gemini 4 Argon",
                "Gemini 4 Hydrogen",
                "Gemini 3.8 Flash",
                "Gemini 3.7 Flash",
                "Gemini 3.6 Flash",
                "Gemini 3.1 Pro"
            ])
        );
        assert_eq!(
            models("Gemini Code Assist Standard", after)["models"],
            json!(["Gemini 3.8 Flash"])
        );
        // An empty tier shows no plan at all, never a guessed one.
        assert_eq!(plan_display(None, Some(before), 0.0), (Value::Null, None));
        assert_eq!(public_plan(None), Value::Null);
        assert_eq!(public_plan(Some(&json!(""))), Value::Null);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod fact_tests {
    use super::*;
    use aac_status::read_context::Presentation;

    /// The envelope path still requires every ISO word's ISO fact (`profile_reads`' case).
    #[test]
    fn an_iso_word_without_its_fact_is_refused_by_the_inventory() {
        let data = json!({"schemaVersion":1,"hostId":"ubuntu","profiles":[{
            "id":"a","email":"a@example.com","plan":null,"hostId":"ubuntu",
            "available":true,"selected":false,"runtimeVerified":false,
            "verifiedAt":"2026-11-15T13:30:00+05:30"}]});
        // The epoch fact is present but its ISO word is not: the context marks the read invalid.
        let only_epoch: Presentation = serde_json::from_value(json!({
            "dates": {},
            "epochs": {"2026-11-15T13:30:00+05:30": 1_794_729_600_000_i64},
            "order": {}
        }))
        .unwrap();
        assert!(
            inventory(Envelope {
                data: data.clone(),
                presentation: only_epoch,
            })
            .is_err(),
            "a word without its ISO fact declines"
        );
        let facts: Presentation = serde_json::from_value(json!({
            "dates": {"1794729600000": {"iso": "2026-11-15T08:00:00.000Z"}},
            "epochs": {"2026-11-15T13:30:00+05:30": 1_794_729_600_000_i64},
            "order": {}
        }))
        .unwrap();
        let answered = inventory(Envelope {
            data,
            presentation: facts,
        })
        .unwrap();
        assert_eq!(
            answered["profiles"][0]["verifiedAt"],
            json!("2026-11-15T08:00:00.000Z")
        );
    }

    #[test]
    fn reported_plan_refines_the_class_like_the_ts_display() {
        let sampled = Some(json!(1_792_065_600_000_i64));
        let class = |plan: Option<&str>, reported: Value| {
            plan_at(plan, sampled.as_ref(), Some(&reported), 0.0)
                .1
                .map(|d| d["class"].as_str().unwrap_or("").to_owned())
        };
        assert_eq!(
            class(Some("Google AI Pro"), json!({"planType":"ultra"})).as_deref(),
            Some("ultra")
        );
        assert_eq!(
            class(
                Some("Google AI Ultra"),
                json!({"paidTier":{"id":"ultra-5x"}})
            )
            .as_deref(),
            Some("ultra-5x")
        );
        assert_eq!(
            class(
                Some("Google AI Pro"),
                json!({"currentTier":{"name":"pro-trial"}})
            )
            .as_deref(),
            Some("pro-trial")
        );
        assert_eq!(
            class(
                Some("Google AI Pro"),
                json!({"paidTier":{"id":"ultra-20x"},"planType":"pro"})
            )
            .as_deref(),
            Some("pro")
        );
        // `known` matches labels/aliases, not class keys: an unmatched planType leaves no class.
        assert_eq!(class(None, json!({"planType":"enterprise-plus"})), None);
        let (plan, detail) = plan_at(Some("Free"), sampled.as_ref(), None, 0.0);
        assert_eq!(plan, json!("Free"));
        assert_eq!(detail.expect("free")["class"], json!("free"));
    }
}
