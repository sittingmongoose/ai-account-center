//! Usage windows: the product visibility rules (visible-usage.mjs), their labels and periods, and the meter and amount
//! views (view-model.mjs `meterView`, `amountView`).
use super::memo::{Key, Part};
use super::{Acc, Ctx};
use crate::js::*;
use aac_api::home::{AmountView, MeterView};
use serde_json::{Map, Value};

static TRUE: Value = Value::Bool(true);

/// One window of an account, as the view sees it (Qwen's monthly windows may carry the subscription's end).
#[derive(Clone, Copy)]
pub struct Win<'a> {
    m: &'a Map<String, Value>,
    expires: Option<&'a Value>,
    /// The window whose `expiresAt` is `expires` (R1b: so a kept window list can be rebuilt).
    expires_idx: Option<usize>,
    plan_expiry: bool,
    /// Position in the account's `windows`: the window's identity.
    pub idx: usize,
}

/// R1b: a visible window by position, as the memo keeps it for a held account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WinSpec {
    idx: usize,
    expires_idx: Option<usize>,
    plan_expiry: bool,
}

impl Win<'_> {
    /// The window object (its address names it in the memo).
    pub fn object(&self) -> &Map<String, Value> {
        self.m
    }

    /// This window by position.
    pub fn spec(&self) -> WinSpec {
        WinSpec {
            idx: self.idx,
            expires_idx: self.expires_idx,
            plan_expiry: self.plan_expiry,
        }
    }
}

/// The windows `specs` names, rebuilt from the account they were taken from; `None` when the account no longer has
/// them (never for a held account, which is immutable).
pub fn from_specs<'a>(account: Acc<'a>, specs: &[WinSpec]) -> Option<Vec<Win<'a>>> {
    let windows = array(account.get("windows"));
    specs
        .iter()
        .map(|spec| {
            Some(Win {
                m: windows.get(spec.idx)?.as_object()?,
                expires: match spec.expires_idx {
                    Some(j) => Some(windows.get(j)?.as_object()?.get("expiresAt")?),
                    None => None,
                },
                expires_idx: spec.expires_idx,
                plan_expiry: spec.plan_expiry,
                idx: spec.idx,
            })
        })
        .collect()
}

impl<'a> Win<'a> {
    /// `w[k]`.
    pub fn get(&self, k: &str) -> Option<&'a Value> {
        match k {
            "expiresAt" if self.expires.is_some() => self.expires,
            "planExpiry" if self.plan_expiry => Some(&TRUE),
            _ => self.m.get(k),
        }
    }
    /// The window key (validated as text or absent).
    pub fn key(&self) -> Option<&'a str> {
        match self.m.get("key") {
            Some(Value::String(s)) => Some(s),
            _ => None,
        }
    }
    /// `text(w[k])`.
    pub fn text(&self, k: &str) -> &'a str {
        text(self.get(k))
    }
    fn num(&self, k: &str) -> Option<f64> {
        finite(self.get(k))
    }
}

/// A window object that is not one of the account's (antigravity-data.mjs column stand-ins).
pub fn stand_in(m: &Map<String, Value>) -> Win<'_> {
    Win {
        m,
        expires: None,
        expires_idx: None,
        plan_expiry: false,
        idx: usize::MAX,
    }
}

/// `${key} ${label}` with absent parts empty.
fn description(w: &Win) -> String {
    format!("{} {}", w.text("key"), w.text("label"))
}

fn chars(s: &str) -> Vec<char> {
    s.chars().collect()
}

/// `/(?:^|[\s_-])(?:plan[\s_-]*)?subscription(?:$|[\s_-])/i`.
fn subscription(w: &Win) -> bool {
    let d = chars(&description(w));
    let sep = |c: char| js_whitespace(c) || c == '_' || c == '-';
    let boundary_before = |at: usize| at == 0 || sep(d[at - 1]);
    (0..d.len()).any(|i| {
        if !ci_at(&d, i, "subscription") {
            return false;
        }
        let end = i + 12;
        if !(end == d.len() || sep(d[end])) {
            return false;
        }
        if boundary_before(i) {
            return true;
        }
        // "plan" followed by any separators, ending right before "subscription"
        let mut k = i;
        while k > 0 && sep(d[k - 1]) {
            k -= 1;
        }
        (k..=i).any(|stop| {
            stop >= 4
                && ci_at(&d, stop - 4, "plan")
                && boundary_before(stop - 4)
                && d[stop..i].iter().all(|c| sep(*c))
        })
    })
}

/// `/chat[\s_-]*pass/i`.
fn chat_pass(desc: &str) -> bool {
    let d = chars(desc);
    (0..d.len()).any(|i| {
        if !ci_at(&d, i, "chat") {
            return false;
        }
        let mut j = i + 4;
        while j < d.len() && (js_whitespace(d[j]) || d[j] == '_' || d[j] == '-') {
            j += 1;
        }
        ci_at(&d, j, "pass")
    })
}

/// `/packs?|reset[\s_-]*(?:cards?|credits?)/i`.
fn zai_pack_like(desc: &str) -> bool {
    let d = chars(desc);
    (0..d.len()).any(|i| {
        if ci_at(&d, i, "pack") {
            return true;
        }
        if !ci_at(&d, i, "reset") {
            return false;
        }
        let mut j = i + 5;
        while j < d.len() && (js_whitespace(d[j]) || d[j] == '_' || d[j] == '-') {
            j += 1;
        }
        ci_at(&d, j, "card") || ci_at(&d, j, "credit")
    })
}

/// `/^reset-packs-(?:5h|weekly)$/`.
pub fn reset_pack_summary(key: &str) -> bool {
    key == "reset-packs-5h" || key == "reset-packs-weekly"
}

/// visible-usage.mjs `visibleUsageWindows`.
pub fn visible<'a>(provider: Option<&str>, account: Acc<'a>) -> R<Vec<Win<'a>>> {
    let rows: Vec<Win> = array(account.get("windows"))
        .iter()
        .enumerate()
        .filter_map(|(idx, v)| match v {
            Value::Object(m) => Some(Win {
                m,
                expires: None,
                expires_idx: None,
                plan_expiry: false,
                idx,
            }),
            _ => None,
        })
        .collect();
    let qwen = provider == Some("qwen");
    let mut expiration = None;
    let mut expiration_idx = None;
    if qwen {
        for w in &rows {
            if subscription(w) && valid_date(w.get("expiresAt"))?.is_some() {
                expiration = w.get("expiresAt");
                expiration_idx = Some(w.idx);
                break;
            }
        }
    }
    let mut out = Vec::new();
    for w in rows {
        // Made only where a provider's rule below reads it (R1: Home is built per request).
        let desc = || description(&w);
        let keep =
            if (provider == Some("codex") && chat_pass(&desc())) || (qwen && subscription(&w)) {
                false
            } else if provider == Some("zai") && zai_pack_like(&desc()) {
                let summary = reset_pack_summary(w.key().unwrap_or(""));
                let mut any = false;
                for k in ["used", "limit", "remaining"] {
                    if let Some(x) = w.num(k)
                        && (if summary { x > 0.0 } else { x != 0.0 })
                    {
                        any = true;
                        break;
                    }
                }
                any || valid_date(w.get("expiresAt"))?.is_some()
                    || valid_date(w.get("resetAt"))?.is_some()
            } else {
                true
            };
        if !keep {
            continue;
        }
        // `expiration &&`: the found value is a valid (so non-empty) date string
        if qwen
            && expiration.is_some()
            && ci_contains(&desc(), "month")
            && valid_date(w.get("expiresAt"))?.is_none()
        {
            out.push(Win {
                expires: expiration,
                expires_idx: expiration_idx,
                plan_expiry: true,
                ..w
            });
        } else {
            out.push(w);
        }
    }
    Ok(out)
}

/// `/^seven_day_(.+)$/.exec(key)?.[1]`.
pub fn weekly_scoped_key(key: &str) -> Option<&str> {
    let rest = key.strip_prefix("seven_day_")?;
    (!rest.is_empty() && !rest.chars().any(is_line_terminator)).then_some(rest)
}
/// `w.key === 'seven_day_fable'`.
pub fn is_fable(w: &Win) -> bool {
    w.key() == Some("seven_day_fable")
}
/// A weekly limit scoped to one model family, or Fable.
pub fn is_weekly_scoped(w: &Win) -> bool {
    w.key().and_then(weekly_scoped_key).is_some() || is_fable(w)
}

/// visible-usage.mjs `wordsOf`: `_`/`-` runs become spaces, then whitespace splits.
fn slug_words(value: &str) -> Vec<String> {
    let mut s = String::new();
    let mut in_run = false;
    for c in value.chars() {
        if c == '_' || c == '-' {
            if !in_run {
                s.push(' ');
            }
            in_run = true;
        } else {
            s.push(c);
            in_run = false;
        }
    }
    split_ws(&s).into_iter().map(String::from).collect()
}

fn title_words(value: &str) -> R<String> {
    Ok(slug_words(value)
        .iter()
        .map(|w| title_word(w))
        .collect::<R<Vec<_>>>()?
        .join(" "))
}

/// The group of `/^weekly\s+(.+?)\s+usage$/i` on a trimmed label (see the module tests for the backtracking cases).
fn weekly_usage_group(label: &str) -> Option<String> {
    let c = chars(label);
    if c.len() < 14 || !ci_at(&c, 0, "weekly") || !ci_at(&c, c.len() - 5, "usage") {
        return None;
    }
    let mid = &c[6..c.len() - 5];
    let lead = mid.iter().take_while(|ch| js_whitespace(**ch)).count();
    let trail = mid
        .iter()
        .rev()
        .take_while(|ch| js_whitespace(**ch))
        .count();
    if lead == 0 || trail == 0 {
        return None;
    }
    if lead == mid.len() {
        // all whitespace: one character at the latest position that leaves whitespace on both sides
        return (1..=mid.len() - 2)
            .rev()
            .find(|&p| !is_line_terminator(mid[p]))
            .map(|p| mid[p].to_string());
    }
    let core = &mid[lead..mid.len() - trail];
    (!core.iter().any(|ch| is_line_terminator(*ch))).then(|| core.iter().collect())
}

/// visible-usage.mjs `scopedModelLabel`.
pub fn scoped_model_label(w: &Win) -> R<String> {
    let label = trim(w.text("label"));
    if let Some(group) = weekly_usage_group(label) {
        return title_words(&group);
    }
    let key = w.key().unwrap_or("");
    let slug = weekly_scoped_key(key)
        .filter(|s| !s.is_empty())
        .unwrap_or(key);
    let words = title_words(slug)?;
    Ok(if !words.is_empty() {
        words
    } else if !label.is_empty() {
        label.to_string()
    } else {
        "Usage".into()
    })
}

fn clear_word(name: &str) -> R<String> {
    Ok(match name {
        "five_hour" => "session".into(),
        "seven_day" => "weekly".into(),
        "seven_day_overage_included" => "Fable included usage".into(),
        "seven_day_fable" => String::new(),
        _ => match weekly_scoped_key(name) {
            Some(slug) => format!("{} weekly", title_words(slug)?),
            None => slug_words(name).join(" "),
        },
    })
}

/// visible-usage.mjs `clearsText`.
fn clears_text(w: &Win) -> R<String> {
    let mut words = Vec::new();
    for target in array(w.get("clears")) {
        if let Value::String(name) = target {
            let word = clear_word(name)?;
            if !word.is_empty() {
                words.push(word);
            }
        }
    }
    Ok(if words.is_empty() {
        String::new()
    } else {
        format!("refills {}", words.join(" + "))
    })
}

/// view-model.mjs `isMeterWindow`.
pub fn is_meter(w: &Win) -> bool {
    !["balance", "extra_usage", "spend"]
        .iter()
        .any(|k| is_str(w.get("kind"), k))
        && !is_true(w.get("unlimited"))
}

/// view-model.mjs `usedPercent`.
pub fn used_percent(w: &Win) -> Option<f64> {
    match (w.num("usedPercent"), w.num("remainingPercent")) {
        (Some(u), _) if u >= 0.0 => Some(u),
        (_, Some(r)) if (0.0..=100.0).contains(&r) => Some(100.0 - r),
        _ => None,
    }
}

/// `/five.?hour|5.?hour|\b5h\b|5 hours|rolling/i` and the weekly and monthly words, on the window's words.
fn has_with_gap(d: &[char], a: &str, b: &str) -> bool {
    let n = a.chars().count();
    (0..d.len()).any(|i| {
        ci_at(d, i, a)
            && (ci_at(d, i + n, b) || (i + n < d.len() && dot(d[i + n]) && ci_at(d, i + n + 1, b)))
    })
}
fn bounded_word(d: &[char], w: &str) -> bool {
    let n = w.chars().count();
    (0..d.len()).any(|i| {
        ci_at(d, i, w) && (i == 0 || !is_word(d[i - 1])) && (i + n == d.len() || !is_word(d[i + n]))
    })
}

/// view-model.mjs `period`.
pub fn period(w: &Win) -> &'static str {
    let explicit_null = matches!(w.get("windowMinutes"), Some(Value::Null));
    let d = if explicit_null {
        w.text("key").to_string()
    } else {
        description(w)
    };
    let c = chars(&d);
    let minutes = w.num("windowMinutes");
    if minutes == Some(300.0)
        || has_with_gap(&c, "five", "hour")
        || has_with_gap(&c, "5", "hour")
        || bounded_word(&c, "5h")
        || ci_contains(&d, "5 hours")
        || ci_contains(&d, "rolling")
    {
        return "5h";
    }
    if minutes == Some(10080.0)
        || has_with_gap(&c, "seven", "day")
        || ci_contains(&d, "weekly")
        || bounded_word(&c, "week")
        || has_with_gap(&c, "7", "day")
    {
        return "week";
    }
    if ci_contains(&d, "month") {
        return "month";
    }
    "other"
}

/// Google keeps separate Antigravity limits for the CLI that T3 runs (keys "cli-...") and for
/// the Antigravity app. T3's come first: they are what T3 spends. Never added together.
const ANTIGRAVITY_COLUMNS: [(&str, &str); 8] = [
    ("cli-gemini-5h", "Gemini 5-hour, in T3 (Antigravity CLI)"),
    (
        "cli-gemini-weekly",
        "Gemini weekly, in T3 (Antigravity CLI)",
    ),
    (
        "cli-3p-5h",
        "Claude and GPT 5-hour, in T3 (Antigravity CLI)",
    ),
    (
        "cli-3p-weekly",
        "Claude and GPT weekly, in T3 (Antigravity CLI)",
    ),
    ("gemini-5h", "Gemini 5-hour, in the Antigravity app"),
    ("gemini-weekly", "Gemini weekly, in the Antigravity app"),
    ("3p-5h", "Claude and GPT 5-hour, in the Antigravity app"),
    ("3p-weekly", "Claude and GPT weekly, in the Antigravity app"),
];

/// antigravity-data.mjs `antigravityService`: which Google quota service a window's key names.
pub fn antigravity_service(key: Option<&str>) -> &'static str {
    match key {
        Some(k) if k.starts_with("cli-") => "cli",
        _ => "app",
    }
}

/// antigravity-data.mjs `antigravityWindowLabel`.
pub fn antigravity_window_label(w: &Win) -> String {
    if w.key() == Some("google-ai-credits") {
        return "AI credits (overage)".into();
    }
    if let Some((_, label)) = ANTIGRAVITY_COLUMNS
        .iter()
        .find(|(k, _)| Some(*k) == w.key())
    {
        return (*label).into();
    }
    let label = w.text("label");
    if label.is_empty() {
        "Usage".into()
    } else {
        label.into()
    }
}

/// Whether the key is one of the four canonical Antigravity buckets.
pub fn antigravity_canonical(key: &str) -> bool {
    ANTIGRAVITY_COLUMNS.iter().any(|(k, _)| *k == key)
}
/// The canonical Antigravity columns, in order.
pub fn antigravity_columns() -> impl Iterator<Item = (&'static str, &'static str)> {
    ANTIGRAVITY_COLUMNS.into_iter()
}

fn or_text(first: Option<&str>, rest: &str, last: &str) -> String {
    match first {
        Some(s) if !s.is_empty() => s.into(),
        _ if !rest.is_empty() => rest.into(),
        _ => last.into(),
    }
}

/// view-model.mjs `windowLabel`.
pub fn window_label(provider: Option<&str>, w: &Win) -> R<String> {
    if is_fable(w) {
        return Ok("Fable".into());
    }
    if is_weekly_scoped(w) {
        return scoped_model_label(w);
    }
    let p = period(w);
    let key = w.key().unwrap_or("undefined");
    let label = w.text("label");
    Ok(match provider {
        Some("antigravity") => antigravity_window_label(w),
        Some("cursor") => or_text(
            lookup(
                &[
                    ("plan-reported", "Included"),
                    ("autoPercentUsed", "Cursor models"),
                    ("apiPercentUsed", "Other models"),
                ],
                key,
            )?,
            label,
            "Usage",
        ),
        Some("zai") => or_text(
            lookup(
                &[
                    ("usage-1", "5h tokens"),
                    ("usage-2", "Weekly tokens"),
                    ("usage-3", "Monthly requests"),
                ],
                key,
            )?,
            label,
            "Usage",
        ),
        Some("codex" | "claude" | "kimi-code" | "opencode-go" | "muse" | "qwen") => or_text(
            match p {
                "5h" => Some("5-hour"),
                "week" => Some("Weekly"),
                "month" => Some("Monthly"),
                _ => None,
            },
            label,
            "Usage",
        ),
        _ => or_text(None, label, "Usage"),
    })
}

/// view-model.mjs `shortWindowLabel`.
pub fn short_window_label(provider: Option<&str>, w: &Win) -> R<String> {
    if provider != Some("antigravity") {
        return window_label(provider, w);
    }
    let short = lookup(
        &[
            ("cli-gemini-5h", "T3 Gemini 5hr"),
            ("cli-gemini-weekly", "T3 Gemini wk"),
            ("cli-3p-5h", "T3 Claude GPT 5hr"),
            ("cli-3p-weekly", "T3 Claude GPT wk"),
            ("gemini-5h", "App Gemini 5hr"),
            ("gemini-weekly", "App Gemini wk"),
            ("3p-5h", "App Claude GPT 5hr"),
            ("3p-weekly", "App Claude GPT wk"),
        ],
        w.key().unwrap_or("undefined"),
    )?;
    match short {
        Some(s) => Ok(s.into()),
        None => window_label(provider, w),
    }
}

/// view-model.mjs `fullWindowLabel`.
pub fn full_window_label(provider: Option<&str>, w: &Win) -> R<String> {
    if is_fable(w) {
        return Ok("Fable weekly".into());
    }
    let label = w.text("label");
    if provider == Some("antigravity") && w.key() == Some("google-ai-credits") {
        return Ok(antigravity_window_label(w));
    }
    if provider == Some("codex") {
        let mapped = lookup(&[("week", "Weekly"), ("5h", "5-hour")], label)?;
        return match mapped {
            Some(s) => Ok(s.into()),
            None if !label.is_empty() => Ok(label.into()),
            None => window_label(provider, w),
        };
    }
    if provider == Some("claude") {
        return Ok(or_text(
            lookup(&[("Five-hour usage", "5-hour usage")], label)?,
            label,
            "Usage",
        ));
    }
    Ok(or_text(None, label, "Usage"))
}

/// view-model.mjs `pendingReset`.
pub fn pending_reset(cx: &Ctx, account: Acc, w: Option<&Win>) -> R<bool> {
    let Some(w) = w else { return Ok(false) };
    if !is_meter(w) || is_false(w.get("enabled")) || used_percent(w).is_none() {
        return Ok(false);
    }
    let Some(reset) = valid_date(w.get("resetAt"))? else {
        return Ok(false);
    };
    if reset > cx.fmt.now {
        return Ok(false);
    }
    let at = match valid_date(w.get("sampledAt"))? {
        Some(t) => Some(t),
        None => valid_date(account.get("sampledAt"))?,
    };
    Ok(at.is_none_or(|t| t < reset))
}

/// view-model.mjs `resetClock`.
fn reset_clock(cx: &Ctx, at: i64) -> R<String> {
    if cx.fmt.same_day(at, cx.fmt.now)? {
        cx.fmt.clock(at)
    } else {
        cx.fmt.day_clock(at)
    }
}

/// view-model.mjs `exact`: `prefix` and the time, or `''`.
pub fn exact(cx: &Ctx, value: Option<&Value>, prefix: &str) -> R<String> {
    Ok(match valid_date(value)? {
        Some(t) => format!("{prefix}{}", cx.fmt.date_time(t)?),
        None => String::new(),
    })
}

/// view-model.mjs `expiryText`.
pub fn expiry_text(cx: &Ctx, value: Option<&Value>, prefix: &str) -> R<String> {
    let passed = valid_date(value)?.is_some_and(|t| t <= cx.fmt.now);
    exact(
        cx,
        value,
        if passed {
            "Reported expiry passed "
        } else {
            prefix
        },
    )
}

/// view-model.mjs `unitAmount`.
pub fn unit_amount(value: Option<f64>, unit: Option<&str>) -> R<String> {
    let Some(x) = value else {
        return Ok(String::new());
    };
    if unit == Some("USD") {
        return money(x);
    }
    let u = match unit {
        Some(u) if !u.is_empty() => {
            if x == 1.0 {
                u.strip_suffix('s').unwrap_or(u)
            } else {
                u
            }
        }
        _ => "",
    };
    Ok(if u.is_empty() {
        nf2(x)?
    } else {
        format!("{} {u}", nf2(x)?)
    })
}

fn compact_amount(x: f64) -> R<String> {
    if x.abs() >= 1e6 { compact(x) } else { nf2(x) }
}

fn sampled_text(cx: &Ctx, account: Acc, w: Option<&Win>) -> R<String> {
    let at = match valid_date(w.and_then(|w| w.get("sampledAt")))? {
        Some(t) => Some(t),
        None => valid_date(account.get("sampledAt"))?,
    };
    Ok(match at {
        Some(t) => format!("{} · {}", cx.date_time(t)?, cx.relative_ms(t)),
        None => String::new(),
    })
}

/// R1b: a window part kept in the memo when `w` is a window of the held generation, else computed.
fn kept(cx: &Ctx, w: Option<&Win>, part: Part, compute: impl FnOnce() -> R<String>) -> R<String> {
    match (&cx.memo, w) {
        (Some(memo), Some(w)) => match memo.window(w.object()) {
            Some((a, i)) => {
                let spec = w.spec();
                let expires = spec
                    .expires_idx
                    .and_then(|j| u32::try_from(j).ok())
                    .unwrap_or(u32::MAX);
                memo.text(Key::Window(part, a, i, expires, spec.plan_expiry), compute)
            }
            None => compute(),
        },
        _ => compute(),
    }
}

/// Options of `meterView`.
pub struct MeterOpts<'s> {
    /// `naText`.
    pub na_text: &'s str,
    /// The label, `''` for the window's own.
    pub label: &'s str,
}

impl Default for MeterOpts<'_> {
    fn default() -> Self {
        MeterOpts {
            na_text: "Unavailable",
            label: "",
        }
    }
}

/// view-model.mjs `meterView`.
pub fn meter_view(cx: &Ctx, account: Acc, w: Option<&Win>, opts: MeterOpts) -> R<MeterView> {
    let provider = str_field(account.get("provider"))?;
    let pending = pending_reset(cx, account, w)?;
    let value = if pending {
        None
    } else {
        w.and_then(used_percent)
    };
    let has_value = value.is_some() && !w.is_some_and(|w| is_true(w.get("unlimited")));
    let unit = w.map(|w| w.text("unit")).unwrap_or("");
    let num = |k: &str| w.and_then(|w| w.num(k));
    let restates = w.is_some_and(
        |w| matches!(w.get("limit"), Some(Value::Number(n)) if n.as_f64() == Some(100.0)),
    ) && unit.is_empty();
    let amount = match (num("used"), num("limit")) {
        (Some(used), Some(limit)) if !pending && !restates && limit > 0.0 => {
            kept(cx, w, Part::Amount, || {
                Ok(format!(
                    "{} of {}{}",
                    compact(used)?,
                    compact_amount(limit)?,
                    if unit.is_empty() {
                        String::new()
                    } else {
                        format!(" {unit}")
                    }
                ))
            })?
        }
        _ => String::new(),
    };
    let reset_at = w.and_then(|w| w.get("resetAt"));
    let reset_ms = valid_date(reset_at)?;
    let label = if !opts.label.is_empty() {
        opts.label.to_string()
    } else {
        match w {
            Some(w) => kept(cx, Some(w), Part::Label, || window_label(provider, w))?,
            None => "Usage".into(),
        }
    };
    let full_label = match w {
        Some(w) => kept(cx, Some(w), Part::FullLabel, || {
            full_window_label(provider, w)
        })?,
        None if !opts.label.is_empty() => opts.label.into(),
        None => "Usage".into(),
    };
    let v = value.unwrap_or(0.0);
    let pending_clock = match (pending, reset_ms) {
        (true, Some(t)) => Some(reset_clock(cx, t)?),
        _ => None,
    };
    let id = text(account.get("id"));
    let key = match w.map(|w| w.text("key")) {
        Some(k) if !k.is_empty() => k,
        _ => "missing",
    };
    Ok(MeterView {
        key: format!("{id}|{key}"),
        label,
        full_label,
        has_value,
        value: Some(if has_value { v } else { 0.0 }),
        value_text: if has_value { cx.nf2(v)? } else { String::new() },
        over_text: if has_value && v > 100.0 {
            cx.nf2(v - 100.0)?
        } else {
            String::new()
        },
        reset: if has_value {
            let r = match reset_ms {
                Some(t) => cx.reset_in(t),
                None => String::new(),
            };
            if r.is_empty() {
                "no reset reported".into()
            } else {
                r
            }
        } else {
            String::new()
        },
        reset_exact: match &pending_clock {
            Some(clock) => format!("Reset at {clock} · new reading pending"),
            None => kept(cx, w, Part::ResetExact, || exact(cx, reset_at, "Resets "))?,
        },
        reset_soon: reset_ms.is_some_and(|t| t - cx.fmt.now < 7_200_000 && t > cx.fmt.now),
        na_text: if has_value {
            String::new()
        } else {
            match &pending_clock {
                Some(clock) => format!("Reset at {clock}"),
                None => opts.na_text.into(),
            }
        },
        na_sub: if has_value || !pending {
            String::new()
        } else {
            "New reading pending".into()
        },
        amount,
        left: match num("remaining") {
            Some(r) if !pending && !unit.is_empty() => kept(cx, w, Part::Left, || {
                Ok(format!("{} left", unit_amount(Some(r), Some(unit))?))
            })?,
            _ => String::new(),
        },
        sampled: sampled_text(cx, account, w)?,
        source: kept(cx, w, Part::Source, || {
            Ok([
                or_text(str_field(account.get("source"))?, "", "Unknown source"),
                cx.host_label(account)?,
                if w.is_some_and(|w| is_str(w.get("status"), "cached")) {
                    "Cached window".into()
                } else {
                    status_word(account)?.into()
                },
            ]
            .join(" · "))
        })?,
        caption: String::new(),
        caption_tip: String::new(),
    })
}

/// view-model.mjs `emptyCell`.
pub fn empty_cell() -> MeterView {
    MeterView {
        value: Some(0.0),
        ..MeterView::default()
    }
}

/// view-model.mjs `statusWord`.
pub fn status_word(account: Acc) -> R<&'static str> {
    let status = match str_field(account.get("status"))? {
        Some(s) => lookup(
            &[
                ("ok", "Live"),
                ("cached", "Cached"),
                ("needs_sign_in", "Sign-in needed"),
                ("error", "Refresh failed"),
                ("unavailable", "Unavailable"),
            ],
            s,
        )?,
        None => None,
    };
    Ok(status.unwrap_or("Unavailable"))
}

/// A Z.ai row that counts all reset packs of one kind.
pub fn rolled_up_packs(account: Acc, w: &Win) -> bool {
    is_str(account.get("provider"), "zai") && reset_pack_summary(w.text("key"))
}

/// view-model.mjs `cacheProvenance`.
pub fn cache_provenance(cx: &Ctx, account: Acc, w: &Win) -> R<String> {
    if !is_str(w.get("status"), "cached") {
        return Ok(String::new());
    }
    let at = match valid_date(w.get("sampledAt"))? {
        Some(t) => Some(t),
        None => valid_date(account.get("sampledAt"))?,
    };
    Ok(match at {
        Some(t) => format!("cached, sampled {}", cx.relative_ms(t)),
        None => "cached".into(),
    })
}

fn joined(parts: &[String]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// `w.remaining > 1` with JavaScript's comparison of a JSON value.
fn more_than_one(v: Option<&Value>) -> R<bool> {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(_)) => Ok(false),
        Some(Value::Number(n)) => Ok(n.as_f64().is_some_and(|x| x > 1.0)),
        _ => Err(Decline("coercion")),
    }
}

fn strip_prefix_ci<'s>(s: &'s str, prefix: &str) -> Option<&'s str> {
    let c = chars(s);
    ci_at(&c, 0, prefix).then(|| {
        let n: usize = c[..prefix.chars().count()]
            .iter()
            .map(|ch| ch.len_utf8())
            .sum();
        &s[n..]
    })
}

/// view-model.mjs `amountView`.
pub fn amount_view(cx: &Ctx, account: Acc, w: &Win, no_expiry: bool) -> R<AmountView> {
    let unit = w.text("unit");
    let unit_opt = Some(unit);
    let mut label = w.text("label").to_string();
    for (from, to) in [
        ("Additional credit pack", "Credit pack"),
        ("Listed active credit packs", "Active packs listed"),
        ("Rate-limit resets available", "Rate-limit resets left"),
    ] {
        if let Some(rest) = strip_prefix_ci(&label, from) {
            label = format!("{to}{rest}");
        }
    }
    let expires = if no_expiry {
        String::new()
    } else {
        expiry_text(cx, w.get("expiresAt"), "Expires ")?
    };
    let num = |k: &str| w.num(k);
    let value: String;
    let (mut suffix, mut sub, mut icon, mut spent) =
        (String::new(), String::new(), "wallet", false);
    let remaining_zero =
        matches!(w.get("remaining"), Some(Value::Number(n)) if n.as_f64() == Some(0.0));
    let or_unavailable = |s: String| {
        if s.is_empty() {
            "Unavailable".to_string()
        } else {
            s
        }
    };
    if rolled_up_packs(account, w) && num("limit").is_some() {
        icon = "pack";
        value = or_unavailable(unit_amount(num("remaining"), None)?);
        suffix = format!("of {} left", unit_amount(num("limit"), unit_opt)?);
        spent = remaining_zero;
        sub = joined(&[
            match num("used") {
                Some(u) if u > 0.0 => format!("{} used", nf2(u)?),
                _ => String::new(),
            },
            if no_expiry {
                String::new()
            } else {
                let prefix = if more_than_one(w.get("remaining"))? {
                    "Next expires "
                } else {
                    "Expires "
                };
                expiry_text(cx, w.get("expiresAt"), prefix)?
            },
        ]);
    } else if ci_contains(w.text("label"), "pack") && num("limit").is_some() {
        icon = "pack";
        value = or_unavailable(unit_amount(num("remaining"), None)?);
        suffix = format!("of {} left", unit_amount(num("limit"), unit_opt)?);
        spent = remaining_zero;
        sub = joined(&[
            if spent {
                "Used up".into()
            } else {
                String::new()
            },
            expires,
        ]);
    } else if is_str(w.get("kind"), "spend") {
        value = or_unavailable(unit_amount(num("used"), unit_opt)?);
        suffix = match num("limit") {
            Some(l) => format!("of {} spent", unit_amount(Some(l), unit_opt)?),
            None => "spent".into(),
        };
        sub = exact(cx, w.get("resetAt"), "Resets ")?;
    } else if is_str(w.get("kind"), "extra_usage") {
        icon = "zap";
        if is_false(w.get("enabled")) {
            value = "Off".into();
            sub = match (num("limit"), num("used")) {
                (Some(l), _) => format!("Limit {}", unit_amount(Some(l), unit_opt)?),
                (None, Some(u)) if !unit.is_empty() => {
                    format!("{} used", unit_amount(Some(u), unit_opt)?)
                }
                _ => String::new(),
            };
        } else {
            value = match num("used") {
                Some(u) => unit_amount(Some(u), unit_opt)?,
                None => "Unavailable".into(),
            };
            suffix = if num("used").is_some() {
                "used".into()
            } else {
                String::new()
            };
            sub = match num("limit") {
                Some(l) => format!("of {}", unit_amount(Some(l), unit_opt)?),
                None => String::new(),
            };
        }
    } else if unit == "resets" {
        icon = "rotate";
        if let (Some(u), Some(l)) = (num("used"), num("limit")) {
            value = nf2(u)?;
            suffix = format!("of {} used", unit_amount(Some(l), Some("resets"))?);
        } else {
            value = or_unavailable(unit_amount(num("remaining"), Some("resets"))?);
        }
        sub = joined(&[
            expires,
            clears_text(w)?,
            if w.key() == Some("session_reset_allowance") {
                exact(cx, w.get("resetAt"), "Next available ")?
            } else {
                String::new()
            },
        ]);
    } else if unit == "packs" {
        icon = "pack";
        value = or_unavailable(unit_amount(num("remaining"), Some("packs"))?);
    } else {
        value = match (num("remaining"), num("used")) {
            (Some(r), _) => unit_amount(Some(r), unit_opt)?,
            (None, Some(u)) => format!("{} used", unit_amount(Some(u), unit_opt)?),
            _ => "Unavailable".into(),
        };
        sub = joined(&[
            if is_false(w.get("enabled")) {
                "Off".into()
            } else {
                String::new()
            },
            expires,
        ]);
    }
    let credits =
        is_str(account.get("provider"), "antigravity") && w.key() == Some("google-ai-credits");
    Ok(AmountView {
        key: format!("{}|{}", text(account.get("id")), w.text("key")),
        label: if credits {
            antigravity_window_label(w)
        } else if label.is_empty() {
            "Balance".into()
        } else {
            label
        },
        value,
        unit: suffix,
        sub: joined(&[
            sub,
            if credits {
                super::antigravity::credits_hint(account).into()
            } else {
                String::new()
            },
            cache_provenance(cx, account, w)?,
        ]),
        icon: icon.into(),
        spent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_weekly_usage_label_follows_the_regex_backtracking() {
        assert_eq!(
            weekly_usage_group("Weekly Opus usage").as_deref(),
            Some("Opus")
        );
        assert_eq!(
            weekly_usage_group("weekly  Sonnet 4  usage").as_deref(),
            Some("Sonnet 4")
        );
        assert_eq!(
            weekly_usage_group("Weekly a usage usage").as_deref(),
            Some("a usage")
        );
        assert_eq!(weekly_usage_group("Weekly a\nb usage"), None);
        assert_eq!(
            weekly_usage_group("Weekly\n opus usage").as_deref(),
            Some("opus")
        );
        assert_eq!(weekly_usage_group("weekly   usage").as_deref(), Some(" "));
        assert_eq!(weekly_usage_group("Weeklyopus usage"), None);
    }

    #[test]
    fn subscription_words_match_the_regex() {
        let w = |label: &str| {
            let m = serde_json::json!({ "key": "x", "label": label });
            let Value::Object(m) = m else { unreachable!() };
            m
        };
        for (label, want) in [
            ("Plan subscription", true),
            ("plan_subscription ends", true),
            ("subscription", true),
            ("plansubscription", true),
            ("xplansubscription", false),
            ("Subscriptions", false),
            ("my-subscription", true),
            ("xplan subscription", true),
        ] {
            let m = w(label);
            let win = Win {
                m: &m,
                expires: None,
                expires_idx: None,
                plan_expiry: false,
                idx: 0,
            };
            assert_eq!(subscription(&win), want, "{label}");
        }
    }
}
