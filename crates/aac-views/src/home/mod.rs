//! Home: view-model.mjs `dashboardViewModel` (view model v3), built from the same three server reads the page makes
//! (GET /api/accounts/dashboard, /api/claude/desktop-profiles and /api/antigravity/profiles) and the page's own
//! context (its clock, display time zone, platform, refresh interval, user, host and format probe), carried in the
//! M3 presentation envelope with the display-zone clock and day words and the collation ranks it needs
//! (`src/web-server/views/home-context.ts` `homeReadInput`). No Claude Open is in progress: while one runs the page
//! draws its own view (the progress line lives only in the browser).
mod antigravity;
mod signins;
pub mod windows;

use crate::js::*;
use crate::spread::{Admit, Refuse};
use aac_api::home::*;
use aac_status::read_context::{Context, Envelope};
use serde::Deserialize;
use serde_json::{Map, Value};
use windows::{MeterOpts, Win};

pub(crate) type Acc<'a> = &'a Map<String, Value>;

const COMPUTERS_JSON: &str = include_str!("../../../../web-dashboard/public/computers.json");

/// The provider registry (view-model.mjs `PROVIDER_REGISTRY`): id, label, long label, sign-in, multi.
const REGISTRY: [(&str, &str, &str, &str, bool); 9] = [
    ("claude", "Claude", "Claude", "Desktop profile", true),
    ("codex", "Codex", "Codex", "Device-code sign-in", true),
    (
        "antigravity",
        "Antigravity",
        "Google Antigravity CLI",
        "Supervised CLI login",
        true,
    ),
    ("cursor", "Cursor", "Cursor", "Desktop app session", false),
    (
        "muse",
        "Muse Code",
        "Muse Code",
        "Desktop app session",
        false,
    ),
    ("kimi-code", "Kimi Code", "Kimi Code", "API key", false),
    (
        "qwen",
        "Qwen Token Plan",
        "Qwen Token Plan",
        "API key, web extras by browser extension",
        false,
    ),
    (
        "zai",
        "Z.ai Coding Plan",
        "Z.ai Coding Plan",
        "API key",
        false,
    ),
    (
        "opencode-go",
        "OpenCode Go",
        "OpenCode Go",
        "API key, console wallet by browser session",
        true,
    ),
];
const CARD_PROVIDERS: [&str; 6] = ["cursor", "muse", "kimi-code", "qwen", "zai", "opencode-go"];
const STALE_MS: i64 = 30 * 60_000;

/// What every part of the build shares: the formatters and the computer registry.
pub struct Ctx {
    /// en-US formatters in the display zone.
    pub fmt: Fmt,
    /// `computers.json` ids and labels, in file order.
    pub computers: Vec<(String, String)>,
}

fn duration(ms: i64) -> String {
    let m = ms.max(0) / 60_000;
    let (h, d) = (m / 60, m / 60 / 24);
    if d >= 1 {
        format!("{d}d {}h", h % 24)
    } else if h >= 1 {
        format!("{h}h {}m", m % 60)
    } else {
        format!("{}m", m.max(1))
    }
}

impl Ctx {
    /// view-model.mjs `relative` of a known time.
    pub fn relative_ms(&self, t: i64) -> String {
        let delta = self.fmt.now - t;
        if delta < 10_000 {
            "just now".into()
        } else {
            format!("{} ago", duration(delta))
        }
    }
    /// `relative(value, now)` of a text field (`''` when it does not parse).
    pub fn relative(&self, v: Option<&str>) -> R<String> {
        Ok(match v.map(parse_date).transpose()?.flatten() {
            Some(t) => self.relative_ms(t),
            None => String::new(),
        })
    }
    /// view-model.mjs `resetIn` of a known time.
    pub fn reset_in(&self, t: i64) -> String {
        let delta = t - self.fmt.now;
        if delta <= 0 {
            "reset due".into()
        } else {
            format!("resets in {}", duration(delta))
        }
    }
    /// view-model.mjs `platformLabel`.
    pub fn platform_label(&self, id: Option<&str>) -> R<String> {
        let Some(id) = id else {
            return Ok("Unknown".into());
        };
        lookup(&[], id)?;
        // `{ ...HOST_LABELS, linux: 'Linux' }`
        if id == "linux" {
            return Ok("Linux".into());
        }
        if let Some((_, label)) = self.computers.iter().find(|(k, _)| k == id)
            && !label.is_empty()
        {
            return Ok(label.clone());
        }
        if id.is_empty() {
            return Ok("Unknown".into());
        }
        title_word(id)
    }
    /// view-model.mjs `hostLabel`: the display-only `host`, else the platform (`??`, so an empty host stays).
    pub fn host_label(&self, account: Acc) -> R<String> {
        let host = str_field(account.get("host"))?;
        self.platform_label(match host {
            Some(h) => Some(h),
            None => str_field(account.get("platform"))?,
        })
    }
}

/// `run(value, strong, tone)`.
fn run(text: impl Into<String>, strong: bool, tone: &str) -> Run {
    Run {
        text: text.into(),
        strong,
        tone: Some(tone.into()),
    }
}
fn plain(text: impl Into<String>) -> Run {
    run(text, false, "")
}

/// view-model.mjs `wordsOf`.
fn words_of(runs: &[Run]) -> Vec<Run> {
    runs.iter()
        .flat_map(|r| {
            split_ws(&r.text)
                .into_iter()
                .map(|w| run(w, r.strong, r.tone.as_deref().unwrap_or("")))
                .collect::<Vec<_>>()
        })
        .collect()
}
fn runs_text(runs: &[Run]) -> String {
    runs.iter().map(|r| r.text.as_str()).collect()
}
fn no_foot() -> FootView {
    FootView::default()
}
fn joined(parts: &[String]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ")
}
fn plural(n: usize) -> &'static str {
    if n == 1 { "account" } else { "accounts" }
}

/// view-model.mjs `planLabel` (`provider` only matters for Cursor).
fn plan_label(plan: &str, provider: Option<&str>) -> R<String> {
    if provider == Some("cursor") {
        let x = [
            ("plus", "X Plus"),
            ("premium", "X Premium"),
            ("super", "X Super"),
            ("unified_ultra", "X Ultra"),
        ];
        let legacy = [
            ("free", "Free"),
            ("pro", "Pro"),
            ("pro_plus", "Pro+"),
            ("ultra", "Cursor Ultra"),
            ("team", "Team"),
            ("enterprise", "Enterprise"),
        ];
        return Ok(x
            .iter()
            .chain(legacy.iter())
            .find(|(k, _)| *k == plan)
            .map(|(_, v)| (*v).to_string())
            .unwrap_or_else(|| plan.to_string()));
    }
    if plan.is_empty() {
        return Ok(String::new());
    }
    if utf16_len(plan) <= 5 {
        title_word(plan)
    } else {
        Ok(plan.into())
    }
}

/// cursor-data.mjs `cursorPlanNote`.
fn cursor_plan_note(account: Acc) -> &'static str {
    let plan = text(account.get("plan"));
    if !is_str(account.get("provider"), "cursor")
        || !["plus", "premium", "super", "unified_ultra"].contains(&plan)
    {
        return "";
    }
    let pool = array(account.get("windows")).iter().find(|w| {
        matches!(get(Some(w), "key"), Some(Value::String(k)) if k.starts_with("grok-shared-"))
            && !is_str(get(Some(w), "kind"), "spend")
            && !is_str(get(Some(w), "kind"), "balance")
    });
    let weekly =
        matches!(get(pool, "windowMinutes"), Some(Value::Number(n)) if n.as_f64() == Some(10080.0));
    match (weekly, plan == "plus") {
        (true, true) => "Weekly Grok usage is shared across Cursor, Grok and X.",
        (false, true) => "Grok usage is shared across Cursor, Grok and X.",
        (true, false) => {
            "Weekly Grok and Composer usage is shared across Cursor, Grok and X. Other models use separately purchased top-ups."
        }
        (false, false) => {
            "Grok and Composer usage is shared across Cursor, Grok and X. Other models use separately purchased top-ups."
        }
    }
}

/// view-model.mjs `isMaxPlan`.
fn is_max_plan(account: Acc) -> R<bool> {
    if !is_str(account.get("provider"), "claude") {
        return Ok(false);
    }
    match account.get("planClass") {
        None | Some(Value::Null) => {}
        Some(v) => return Ok(is_str(Some(v), "max")),
    }
    let plan: Vec<char> = str_field(account.get("plan"))?
        .unwrap_or("")
        .chars()
        .map(|c| if c == '_' || c == '-' { ' ' } else { c })
        .collect();
    let end_or_ws = |at: usize| at == plan.len() || js_whitespace(plan[at]);
    Ok((0..plan.len()).any(|i| {
        if !ci_at(&plan, i, "max") || !(i == 0 || js_whitespace(plan[i - 1])) {
            return false;
        }
        let j = i + 3;
        if end_or_ws(j) {
            return true;
        }
        let mut k = j;
        while k < plan.len() && js_whitespace(plan[k]) {
            k += 1;
        }
        let digits = if ci_at(&plan, k, "5x") {
            2
        } else if ci_at(&plan, k, "20x") {
            3
        } else {
            return false;
        };
        end_or_ws(k + digits)
    }))
}

/// view-model.mjs `claudeSignInNeeded`.
fn claude_sign_in_needed(account: Acc) -> String {
    let list = array(account.get("signInNeeded"));
    let has = |p: &str| list.iter().any(|v| is_str(Some(v), p));
    [("mac", "Mac"), ("windows", "Windows")]
        .iter()
        .filter(|(p, _)| has(p))
        .map(|(_, w)| *w)
        .collect::<Vec<_>>()
        .join(" and ")
}

/// `text(email) || text(label) || fallback`.
fn identity(account: Acc, fallback: &str) -> String {
    let email = text(account.get("email"));
    let label = text(account.get("label"));
    if !email.is_empty() {
        email.into()
    } else if !label.is_empty() {
        label.into()
    } else {
        fallback.into()
    }
}

/// `account.sampledAt || account.fetchedAt`.
fn sampled_at(account: Acc<'_>) -> R<Option<&str>> {
    Ok(match str_field(account.get("sampledAt"))? {
        Some(s) if !s.is_empty() => Some(s),
        _ => str_field(account.get("fetchedAt"))?,
    })
}

fn normal(account: Acc) -> bool {
    is_str(account.get("status"), "ok") || is_str(account.get("status"), "cached")
}

/// Each provider section's age-or-status word.
fn age_or_status(cx: &Ctx, account: Acc) -> R<String> {
    if normal(account) {
        cx.relative(sampled_at(account)?)
    } else {
        Ok(windows::status_word(account)?.into())
    }
}

fn meters_of(account: Acc<'_>) -> R<Vec<Win<'_>>> {
    let provider = str_field(account.get("provider"))?;
    Ok(windows::visible(provider, account)?
        .into_iter()
        .filter(windows::is_meter)
        .collect())
}
fn amounts_of(account: Acc<'_>) -> R<Vec<Win<'_>>> {
    let provider = str_field(account.get("provider"))?;
    Ok(windows::visible(provider, account)?
        .into_iter()
        .filter(|w| !windows::is_meter(w))
        .collect())
}

fn meter(cx: &Ctx, account: Acc, w: Option<&Win>, label: &str) -> R<MeterView> {
    windows::meter_view(
        cx,
        account,
        w,
        MeterOpts {
            label,
            ..MeterOpts::default()
        },
    )
}

// ---------------------------------------------------------------- sections

fn five_of<'a>(meters: &[Win<'a>]) -> Option<Win<'a>> {
    meters
        .iter()
        .find(|w| {
            !windows::is_fable(w) && !windows::is_weekly_scoped(w) && windows::period(w) == "5h"
        })
        .copied()
}

fn claude_section(
    cx: &Ctx,
    data: &Value,
    accounts: &[Acc],
    profiles: &Value,
    platform: &str,
) -> R<SectionView> {
    let mut any_max = false;
    for a in accounts {
        any_max |= is_max_plan(a)?;
    }
    let by_account: Vec<Vec<Win>> = accounts.iter().map(|a| meters_of(a)).collect::<R<_>>()?;
    let has_five = by_account.iter().any(|m| five_of(m).is_some());
    let has_weekly = by_account
        .iter()
        .any(|m| m.iter().any(|w| w.key() == Some("seven_day")));
    let mut scoped: Vec<&str> = Vec::new();
    for meters in &by_account {
        for w in meters {
            if windows::is_weekly_scoped(w)
                && !windows::is_fable(w)
                && let Some(k) = w.key()
                && !scoped.contains(&k)
            {
                scoped.push(k);
            }
        }
    }
    let mut columns = Vec::new();
    if has_five {
        columns.push(ColumnView {
            key: "five".into(),
            label: "5-hour".into(),
            short_label: String::new(),
        });
    }
    if has_weekly {
        columns.push(ColumnView {
            key: "weekly".into(),
            label: "Weekly".into(),
            short_label: String::new(),
        });
    }
    for key in &scoped {
        let sample = by_account.iter().flatten().find(|w| w.key() == Some(key));
        let label = match sample {
            Some(w) => windows::scoped_model_label(w)?,
            None => "Usage".into(),
        };
        columns.push(ColumnView {
            key: (*key).into(),
            label,
            short_label: String::new(),
        });
    }
    if any_max {
        columns.push(ColumnView {
            key: "seven_day_fable".into(),
            label: "Fable".into(),
            short_label: String::new(),
        });
    }
    columns.truncate(6);
    let profiles = array(Some(profiles));
    let mut rows = Vec::new();
    for (account, meters) in accounts.iter().zip(&by_account) {
        let five = five_of(meters);
        let weekly = meters
            .iter()
            .find(|w| w.key() == Some("seven_day"))
            .copied();
        let fable = meters.iter().find(|w| windows::is_fable(w)).copied();
        let cap = get(account.get("capabilities"), "claudeProfileId");
        let mut launcher = None;
        for p in profiles {
            if p.is_null() {
                return Err(Decline("shape"));
            }
            if strict_eq(get(Some(p), "id"), cap) {
                launcher = Some(p);
                break;
            }
        }
        let mut cells = Vec::new();
        for col in &columns {
            cells.push(match col.key.as_str() {
                "five" => match &five {
                    Some(w) => meter(cx, account, Some(w), "5-hour")?,
                    None => windows::empty_cell(),
                },
                "weekly" => match &weekly {
                    Some(w) => meter(cx, account, Some(w), "Weekly")?,
                    None => windows::empty_cell(),
                },
                "seven_day_fable" => {
                    if is_max_plan(account)? {
                        windows::meter_view(
                            cx,
                            account,
                            fable.as_ref(),
                            MeterOpts {
                                label: "Fable",
                                na_text: "Not reported yet",
                            },
                        )?
                    } else {
                        windows::empty_cell()
                    }
                }
                key => match meters.iter().find(|w| w.key() == Some(key)) {
                    Some(w) => meter(cx, account, Some(w), &col.label)?,
                    None => windows::empty_cell(),
                },
            });
        }
        let sign_in = claude_sign_in_needed(account);
        let renewal = signins::line(cx, data, account.get("email"))?;
        let plan = text(account.get("plan"));
        let middle = if normal(account) {
            cx.relative(sampled_at(account)?)?
        } else if !sign_in.is_empty() {
            String::new()
        } else {
            windows::status_word(account)?.into()
        };
        let signed = if sign_in.is_empty() {
            String::new()
        } else {
            format!("Sign-in needed on {sign_in}")
        };
        let mac = get(launcher, "mac");
        let win = get(launcher, "windows");
        rows.push(RowView {
            id: text(account.get("id")).into(),
            provider: "claude".into(),
            profile: str_field(cap)?.unwrap_or("").into(),
            email: identity(account, "Account identity unavailable"),
            plan: plan.into(),
            meta: joined(&[plan_label(plan, None)?, middle, signed.clone(), renewal]),
            status: if signed.is_empty() {
                windows::status_word(account)?.into()
            } else {
                signed
            },
            note: text(account.get("message")).into(),
            platform: text(account.get("platform")).into(),
            active: false,
            active_label: String::new(),
            setup: false,
            can_mac: truthy(get(mac, "canOpen")),
            can_windows: is_true(get(win, "canOpen"))
                || (platform == "windows" && truthy(get(win, "launchUri"))),
            amounts_line: String::new(),
            amounts_runs: Vec::new(),
            amounts_words: Vec::new(),
            cells,
        });
    }
    let needs = signins::needs_count(data, None)?;
    let mut meta_runs = vec![
        run(rows.len().to_string(), true, ""),
        plain(format!(" {} · desktop profiles", plural(rows.len()))),
    ];
    if needs > 0 {
        meta_runs.push(plain(" · "));
        meta_runs.push(run(
            format!(
                "{needs} sign-in {} you",
                if needs == 1 {
                    "step needs"
                } else {
                    "steps need"
                }
            ),
            true,
            "warn",
        ));
    }
    Ok(SectionView {
        id: "claude".into(),
        kind: "claude".into(),
        label: "Claude".into(),
        long_label: "Claude".into(),
        meta: runs_text(&meta_runs),
        meta_words: words_of(&meta_runs),
        meta_runs,
        foot: no_foot(),
        active_id: String::new(),
        active_label: String::new(),
        empty: "No Claude accounts are reported yet.".into(),
        columns,
        rows,
    })
}

fn short_identity(account: Acc) -> String {
    let id = identity(account, "Account");
    match id.find('@') {
        None => id,
        Some(at) => {
            // `/@.*/`: from the first @ up to a line terminator
            let rest = &id[at..];
            let stop = rest.find(is_line_terminator).unwrap_or(rest.len());
            format!("{}{}", &id[..at], &rest[stop..])
        }
    }
}

/// view-model.mjs `codexAmountsRuns`.
fn codex_amounts(cx: &Ctx, account: Acc) -> R<Vec<Run>> {
    let ws = windows::visible(Some("codex"), account)?;
    let credits = ws
        .iter()
        .find(|w| {
            w.key() == Some("credits_balance")
                || (is_str(w.get("kind"), "balance") && is_str(w.get("unit"), "credits"))
        })
        .copied();
    let banked: Vec<Win> = ws
        .iter()
        .filter(|w| {
            let k: Vec<char> = w.text("key").chars().collect();
            ci_at(&k, 0, "banked") && {
                let at = if k.get(6).is_some_and(|c| *c == '_' || *c == '-') {
                    7
                } else {
                    6
                };
                ci_at(&k, at, "resets") || ci_at(&k, 6, "resets")
            }
        })
        .copied()
        .collect();
    let total = if !banked.is_empty()
        && banked
            .iter()
            .all(|w| finite(w.get("remaining")).is_some_and(|r| r >= 0.0))
    {
        Some(
            banked
                .iter()
                .fold(0.0, |t, w| t + finite(w.get("remaining")).unwrap_or(0.0)),
        )
    } else {
        None
    };
    let mut parts: Vec<Vec<Run>> = Vec::new();
    if let Some(c) = &credits {
        if is_false(c.get("enabled")) {
            parts.push(vec![plain("Credits off")]);
        } else if let Some(r) = finite(c.get("remaining")) {
            parts.push(vec![
                run(if r >= 10000.0 { compact(r)? } else { nf2(r)? }, true, ""),
                plain(" credits"),
            ]);
        }
    }
    if let Some(t) = total {
        parts.push(vec![run(nf2(t)?, true, ""), plain(" banked")]);
    }
    let mut notes: Vec<String> = Vec::new();
    for w in credits.iter().chain(banked.iter()) {
        let note = windows::cache_provenance(cx, account, w)?;
        if !note.is_empty() && !notes.contains(&note) {
            notes.push(note);
        }
    }
    for note in notes {
        parts.push(vec![plain(note)]);
    }
    let mut out = Vec::new();
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            out.push(plain(" · "));
        }
        out.extend(part);
    }
    Ok(out)
}

fn codex_section(cx: &Ctx, accounts: &[Acc], all: &[Acc]) -> R<SectionView> {
    let hidden: Vec<&str> = all
        .iter()
        .copied()
        .filter(|a: &Acc| !accounts.iter().any(|b| std::ptr::eq(*a, *b)))
        .map(|a| text(a.get("id")))
        .collect();
    let mut has_five = false;
    for a in accounts {
        has_five |= windows::visible(Some("codex"), a)?
            .iter()
            .any(|w| w.key() == Some("five_hour") && windows::used_percent(w).is_some());
    }
    let mut columns = Vec::new();
    if has_five {
        columns.push(ColumnView {
            key: "five".into(),
            label: "5-hour".into(),
            short_label: String::new(),
        });
    }
    columns.push(ColumnView {
        key: "weekly".into(),
        label: "Weekly".into(),
        short_label: String::new(),
    });
    let active = all.iter().find(|a| is_true(a.get("isActive"))).copied();
    let active_hidden = active.is_some_and(|a| hidden.contains(&text(a.get("id"))));
    let suffix = if active_hidden { " (hidden)" } else { "" };
    let active_name = active
        .map(|a| format!("{}{suffix}", short_identity(a)))
        .unwrap_or_default();
    let mut rows = Vec::new();
    for account in accounts {
        let ws = windows::visible(Some("codex"), account)?;
        let five = ws.iter().find(|w| w.key() == Some("five_hour"));
        let weekly = ws.iter().find(|w| w.key() == Some("seven_day"));
        let mut cells = Vec::new();
        if has_five {
            cells.push(match five {
                Some(w) if windows::used_percent(w).is_some() => {
                    meter(cx, account, Some(w), "5-hour")?
                }
                _ => windows::empty_cell(),
            });
        }
        let mut weekly_cell = meter(cx, account, weekly, "Weekly")?;
        if weekly.is_none() {
            weekly_cell.key = format!("{}|seven_day", text(account.get("id")));
        }
        cells.push(weekly_cell);
        let amounts = codex_amounts(cx, account)?;
        let plan = text(account.get("plan"));
        rows.push(RowView {
            id: text(account.get("id")).into(),
            provider: "codex".into(),
            profile: str_field(get(account.get("capabilities"), "codexProfile"))?
                .unwrap_or("")
                .into(),
            email: identity(account, "Account identity unavailable"),
            plan: plan.into(),
            meta: joined(&[
                plan_label(plan, None)?,
                cx.host_label(account)?,
                age_or_status(cx, account)?,
            ]),
            status: windows::status_word(account)?.into(),
            note: text(account.get("message")).into(),
            platform: cx.host_label(account)?,
            active: is_true(account.get("isActive")),
            active_label: "on Ubuntu".into(),
            setup: false,
            can_mac: false,
            can_windows: false,
            amounts_line: runs_text(&amounts),
            amounts_words: words_of(&amounts),
            amounts_runs: amounts,
            cells,
        });
    }
    let n = rows.len();
    let mut meta_runs = vec![
        run(n.to_string(), true, ""),
        plain(format!(" {} · ", plural(n))),
    ];
    if active.is_some() {
        meta_runs.push(run(active_name.clone(), true, "good"));
        meta_runs.push(plain(" active"));
    } else {
        meta_runs.push(plain("none active"));
    }
    Ok(SectionView {
        id: "codex".into(),
        kind: "accounts".into(),
        label: "Codex".into(),
        long_label: "Codex".into(),
        meta: format!(
            "{n} {}{}",
            plural(n),
            if active.is_some() {
                format!(" · {active_name} active")
            } else {
                String::new()
            }
        ),
        meta_words: words_of(&meta_runs),
        meta_runs,
        foot: no_foot(),
        active_id: active
            .map(|a| text(a.get("id")).to_string())
            .unwrap_or_default(),
        active_label: active
            .map(|a| {
                let email = text(a.get("email"));
                let name = if email.is_empty() {
                    text(a.get("label"))
                } else {
                    email
                };
                format!("{name}{suffix}")
            })
            .unwrap_or_default(),
        empty: "No Codex accounts are reported yet.".into(),
        columns,
        rows,
    })
}

fn antigravity_section(
    cx: &Ctx,
    every: &[Value],
    accounts: &[Acc],
    inventory: &Value,
) -> R<SectionView> {
    let native = antigravity::native(cx, every, Some(inventory))?;
    let meters: Vec<Vec<Win>> = accounts.iter().map(|a| meters_of(a)).collect::<R<_>>()?;
    // antigravity-data.mjs `antigravityColumns`
    let flat: Vec<&Win> = meters.iter().flatten().collect();
    for w in &flat {
        if w.key().is_none() {
            return Err(Decline("shape"));
        }
    }
    let canonical = flat
        .iter()
        .any(|w| w.key().is_some_and(windows::antigravity_canonical));
    let mut cols: Vec<(String, String)> = if canonical {
        windows::antigravity_columns()
            .map(|(k, l)| (k.to_string(), l.to_string()))
            .collect()
    } else {
        Vec::new()
    };
    for w in &flat {
        let key = w.key().unwrap_or("");
        if !cols.iter().any(|(k, _)| k == key) {
            cols.push((key.into(), windows::antigravity_window_label(w)));
        }
    }
    let mut columns = Vec::new();
    for (key, label) in &cols {
        let short = match flat.iter().find(|w| w.key() == Some(key.as_str())) {
            Some(w) => windows::short_window_label(Some("antigravity"), w)?,
            None => {
                let stand_in = serde_json::json!({ "key": key, "label": label });
                let Value::Object(m) = &stand_in else {
                    return Err(Decline("shape"));
                };
                let w = windows::stand_in(m);
                windows::short_window_label(Some("antigravity"), &w)?
            }
        };
        columns.push(ColumnView {
            key: key.clone(),
            label: label.clone(),
            short_label: short,
        });
    }
    let mut rows = Vec::new();
    for (account, own) in accounts.iter().zip(&meters) {
        let id = text(account.get("id"));
        let bound = native.accounts.iter().rev().find(|b| b.id == id);
        let active = bound.is_some_and(|b| b.selected);
        let no_quota = if antigravity::no_quota(account) {
            "No Antigravity quota on this plan"
        } else {
            ""
        };
        let mut cells = Vec::new();
        for col in &columns {
            if !no_quota.is_empty() {
                cells.push(windows::empty_cell());
                continue;
            }
            let w = own.iter().find(|w| w.key() == Some(col.key.as_str()));
            let na = match w {
                Some(_) => "Unavailable",
                None => antigravity::missing_window(account, &col.key)?,
            };
            let mut cell = windows::meter_view(
                cx,
                account,
                w,
                MeterOpts {
                    label: &col.label,
                    na_text: na,
                },
            )?;
            if w.is_none() {
                cell.key = format!("{id}|{}", col.key);
            }
            cells.push(cell);
        }
        let plan = text(account.get("plan"));
        let profile = match bound.map(|b| b.profile.as_str()) {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => str_field(get(account.get("capabilities"), "antigravityProfileId"))?
                .unwrap_or("")
                .into(),
        };
        let quota_runs = if no_quota.is_empty() {
            Vec::new()
        } else {
            vec![plain(no_quota)]
        };
        rows.push(RowView {
            id: id.into(),
            provider: "antigravity".into(),
            profile,
            email: identity(account, "Account identity unavailable"),
            plan: plan.into(),
            meta: joined(&[
                plan.to_string(),
                cx.host_label(account)?,
                age_or_status(cx, account)?,
            ]),
            status: match bound {
                Some(b) if !b.status.is_empty() => b.status.clone(),
                _ => windows::status_word(account)?.into(),
            },
            note: match bound {
                Some(b) if !b.note.is_empty() => b.note.clone(),
                _ => text(account.get("message")).into(),
            },
            platform: cx.host_label(account)?,
            active,
            active_label: match bound {
                Some(b) if active && b.runtime_verified => "on Ubuntu".into(),
                Some(_) if active => "on Ubuntu, running unverified".into(),
                _ => String::new(),
            },
            setup: false,
            can_mac: false,
            can_windows: false,
            amounts_line: no_quota.into(),
            amounts_words: words_of(&quota_runs),
            amounts_runs: quota_runs,
            cells,
        });
    }
    let n = rows.len();
    let active_row = rows.iter().find(|r| r.active);
    let meta_runs = vec![
        plain("Google Antigravity CLI · "),
        run(n.to_string(), true, ""),
        plain(format!(" {}", plural(n))),
    ];
    let problem = native.service_problem.or(native.update_paused);
    Ok(SectionView {
        id: "antigravity".into(),
        kind: "accounts".into(),
        label: "Antigravity".into(),
        long_label: "Google Antigravity CLI".into(),
        meta: format!("Google Antigravity CLI · {n} {}", plural(n)),
        meta_words: words_of(&meta_runs),
        meta_runs,
        foot: match problem {
            Some(p) => {
                let runs = vec![plain(p)];
                FootView {
                    shown: true,
                    warn: true,
                    words: words_of(&runs),
                    runs,
                    when: String::new(),
                }
            }
            None => no_foot(),
        },
        active_id: active_row.map(|r| r.id.clone()).unwrap_or_default(),
        active_label: active_row.map(|r| r.email.clone()).unwrap_or_default(),
        empty: "No Antigravity accounts are reported yet.".into(),
        columns,
        rows,
    })
}

// ---------------------------------------------------------------- cards

/// `text(plan).replace(new RegExp('^' + escaped(label) + '\\s*', 'i'), '')` for the ASCII registry labels.
fn without_label<'p>(plan: &'p str, label: &str) -> &'p str {
    let c: Vec<char> = plan.chars().collect();
    if !ci_at(&c, 0, label) {
        return plan;
    }
    let n = label.chars().count();
    let skip = c[n..].iter().take_while(|ch| js_whitespace(**ch)).count();
    let bytes: usize = c[..n + skip].iter().map(|ch| ch.len_utf8()).sum();
    &plan[bytes..]
}

fn provider_cards(cx: &Ctx, accounts: &[Acc], hidden: &[&str]) -> R<Vec<CardView>> {
    let order = |p: &str| CARD_PROVIDERS.iter().position(|c| *c == p);
    let mut list: Vec<(usize, Acc)> = accounts
        .iter()
        .filter_map(|a| {
            let p = text(a.get("provider"));
            order(p).filter(|_| !hidden.contains(&p)).map(|i| (i, *a))
        })
        .collect();
    let mut failed = None;
    list.sort_by(|(ia, a), (ib, b)| {
        ia.cmp(ib).then_with(
            || match cx.fmt.compare(text(a.get("id")), text(b.get("id"))) {
                Ok(o) => o,
                Err(d) => {
                    failed = Some(d);
                    std::cmp::Ordering::Equal
                }
            },
        )
    });
    if let Some(d) = failed {
        return Err(d);
    }
    let mut cards = Vec::new();
    for (index, account) in list {
        let provider = CARD_PROVIDERS[index];
        let label = REGISTRY
            .iter()
            .find(|r| r.0 == provider)
            .map(|r| r.1)
            .unwrap_or("");
        let all_meters = meters_of(account)?;
        let meters = all_meters
            .iter()
            .take(3)
            .map(|w| windows::meter_view(cx, account, Some(w), MeterOpts::default()))
            .collect::<R<Vec<_>>>()?;
        let mut plan_expiry = None;
        for w in &all_meters {
            if truthy(w.get("planExpiry")) && valid_date(w.get("expiresAt"))?.is_some() {
                plan_expiry = Some(*w);
                break;
            }
        }
        let amount_windows = amounts_of(account)?;
        let mut packs: Vec<Win> = Vec::new();
        for w in &amount_windows {
            if ci_contains(w.text("label"), "pack")
                && valid_date(w.get("expiresAt"))?.is_some()
                && !windows::rolled_up_packs(account, w)
            {
                packs.push(*w);
            }
        }
        let same_expiry = packs.len() > 1
            && packs
                .iter()
                .all(|w| strict_eq(w.get("expiresAt"), packs[0].get("expiresAt")));
        let sampled = sampled_at(account)?;
        let sampled_ms = sampled.map(parse_date).transpose()?.flatten();
        let stale = sampled_ms.is_some_and(|t| cx.fmt.now - t > STALE_MS);
        let status = windows::status_word(account)?;
        let plan = plan_label(
            without_label(text(account.get("plan")), label),
            Some(provider),
        )?;
        let source = joined(&[
            if is_str(account.get("status"), "cached") {
                format!("Cached · sampled {}", cx.relative(sampled)?)
            } else {
                status.into()
            },
            text(account.get("source")).into(),
        ]);
        let plan_note = joined(&[
            cursor_plan_note(account).into(),
            match plan_expiry {
                Some(w) => windows::exact(cx, w.get("expiresAt"), "Plan subscription ends ")?,
                None => String::new(),
            },
        ]);
        let packs_note = if same_expiry {
            windows::expiry_text(
                cx,
                packs[0].get("expiresAt"),
                &format!("All {} packs expire ", packs.len()),
            )?
        } else {
            String::new()
        };
        let message = text(account.get("message"));
        let mut amounts = Vec::new();
        for w in &amount_windows {
            let in_packs = packs.iter().any(|p| p.idx == w.idx);
            amounts.push(windows::amount_view(
                cx,
                account,
                w,
                same_expiry && in_packs,
            )?);
        }
        cards.push(CardView {
            id: text(account.get("id")).into(),
            provider: provider.into(),
            account_id: text(account.get("id")).into(),
            label: label.into(),
            identity: identity(account, "Identity unavailable"),
            plan,
            status: status.into(),
            source,
            flag: if !normal(account) {
                status.into()
            } else if stale {
                "Stale".into()
            } else {
                String::new()
            },
            sampled: match sampled_ms {
                Some(t) => format!("sampled {}", cx.relative_ms(t)),
                None => "never sampled".into(),
            },
            platform: cx.host_label(account)?,
            plan_note,
            packs_note,
            note: if !message.is_empty() {
                message.into()
            } else if meters.is_empty() {
                "Usage unavailable".into()
            } else {
                String::new()
            },
            meters,
            amounts,
        });
    }
    Ok(cards)
}

// ---------------------------------------------------------------- chrome

fn interval_label(seconds: u32) -> String {
    if seconds < 60 {
        format!("{seconds} s")
    } else if seconds.is_multiple_of(60) {
        format!("{} min", seconds / 60)
    } else {
        format!("{} min {} s", seconds / 60, seconds % 60)
    }
}

fn chrome(cx: &Ctx, data: &Value, every: &[Value], ctx: &HomeContext) -> R<ChromeView> {
    let cached = every
        .iter()
        .any(|a| is_str(get(Some(a), "status"), "cached"));
    let updated = valid_date(data.get("updatedAt"))?;
    let base = ChromeView {
        refreshing: false,
        username: ctx.username.clone(),
        host: ctx.host.clone(),
        ..ChromeView::default()
    };
    let Some(updated) = updated else {
        return Ok(ChromeView {
            status_lead: "Account data unavailable".into(),
            ..base
        });
    };
    let more = if cached { "cached readings" } else { "live" };
    Ok(ChromeView {
        status_lead: "Updated".into(),
        status_strong: cx.relative_ms(updated),
        status_more: more.into(),
        status_tip: format!(
            "Last refresh {} · {more} · every {}",
            cx.fmt.date_time(updated)?,
            interval_label(ctx.interval_seconds)
        ),
        ..base
    })
}

// ---------------------------------------------------------------- the view

fn validate(data: &Value) -> R<&[Value]> {
    let Some(Value::Array(accounts)) = data.get("accounts") else {
        return Err(Decline("shape"));
    };
    for a in accounts {
        match a {
            Value::Null | Value::Array(_) => return Err(Decline("shape")),
            Value::Object(m) => {
                if !matches!(m.get("id"), Some(Value::String(_))) {
                    return Err(Decline("type"));
                }
                for k in [
                    "provider",
                    "email",
                    "label",
                    "plan",
                    "status",
                    "message",
                    "source",
                    "platform",
                    "host",
                    "sampledAt",
                    "fetchedAt",
                    "providerLabel",
                ] {
                    str_field(m.get(k))?;
                }
                match m.get("capabilities") {
                    None | Some(Value::Null) => {}
                    Some(Value::Object(c)) => {
                        for k in ["claudeProfileId", "codexProfile", "antigravityProfileId"] {
                            str_field(c.get(k))?;
                        }
                    }
                    Some(_) => return Err(Decline("type")),
                }
                for w in array(m.get("windows")) {
                    match w {
                        Value::Array(_) => return Err(Decline("shape")),
                        Value::Object(wm) => {
                            str_field(wm.get("key"))?;
                            str_field(wm.get("label"))?;
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(accounts)
}

fn computers() -> R<Vec<(String, String)>> {
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(COMPUTERS_JSON) else {
        return Err(Decline("registry"));
    };
    Ok(map
        .iter()
        .map(|(id, row)| (id.clone(), text(row.get("label")).to_string()))
        .collect())
}

/// GET /api/views/home's Rust candidate for the shared read dispatch: the view, or [`Refuse`] (the page builds its
/// own view).
pub fn compose(envelope: Envelope) -> Admit<Value> {
    let view = build(envelope).map_err(|_| Refuse)?;
    serde_json::to_value(view).map_err(|_| Refuse)
}

/// The envelope's data: the page's context and the three answers, exactly these keys.
fn inputs(data: &Value) -> R<(HomeContext, &Value, &Value, &Value)> {
    let Some(map) = data.as_object() else {
        return Err(Decline("input"));
    };
    let keys = ["ctx", "dashboard", "profiles", "antigravity"];
    if map.len() != keys.len() || !keys.iter().all(|k| map.contains_key(*k)) {
        return Err(Decline("input"));
    }
    let ctx = HomeContext::deserialize(&map["ctx"]).map_err(|_| Decline("input"))?;
    Ok((
        ctx,
        &map["dashboard"],
        &map["profiles"],
        &map["antigravity"],
    ))
}

/// The finished Home view, or the reason it cannot be proven equal to the browser's.
pub fn build(envelope: Envelope) -> R<HomeView> {
    let (ctx, data, profiles, antigravity) = inputs(&envelope.data)?;
    let style = Fmt::proven_style(&ctx.format_probe)?;
    let cx = Ctx {
        fmt: Fmt::new(
            Context::new(envelope.presentation),
            style,
            Zone::proven(&ctx.zone)?,
            ctx.now,
        ),
        computers: computers()?,
    };
    let built = view(&cx, &ctx, data, profiles, antigravity)?;
    // a fact the envelope did not have was read as missing: nothing built from it can be served
    if cx.fmt.valid() {
        Ok(built)
    } else {
        Err(Decline("fact"))
    }
}

fn view(
    cx: &Ctx,
    ctx: &HomeContext,
    data: &Value,
    profiles: &Value,
    antigravity: &Value,
) -> R<HomeView> {
    if !data.is_object() {
        return Err(Decline("shape"));
    }
    let raw = validate(data)?;
    // A Claude Open the server reports as running puts its progress on the row (claude-open.mjs `openProgress`);
    // that line belongs to the page, so the page draws its own view then.
    for p in array(Some(profiles)) {
        let op = get(Some(p), "openOperation");
        if matches!(get(Some(p), "id"), Some(Value::String(_)))
            && matches!(op, Some(Value::Object(_) | Value::Array(_)))
            && !["opened", "failed", "blocked_uncertain"]
                .iter()
                .any(|t| is_str(get(op, "state"), t))
        {
            return Err(Decline("open-progress"));
        }
    }
    let settings = data.get("settings");
    let hidden_ids = array(get(settings, "hiddenAccountIds"));
    let every: Vec<Acc> = raw
        .iter()
        .filter_map(|a| match a {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .collect();
    let accounts: Vec<Acc> = every
        .iter()
        .filter(|a| {
            !hidden_ids.iter().any(|h| strict_eq(Some(h), a.get("id"))) && !is_true(a.get("hidden"))
        })
        .copied()
        .collect();
    let hidden: Vec<&str> = array(get(settings, "hiddenProviders"))
        .iter()
        .filter_map(|v| match v {
            Value::String(s) if REGISTRY.iter().any(|r| r.0 == s) => Some(s.as_str()),
            _ => None,
        })
        .collect();
    let of = |p: &str| -> Vec<Acc> {
        accounts
            .iter()
            .filter(|a| is_str(a.get("provider"), p))
            .copied()
            .collect()
    };
    let mut sections = Vec::new();
    if !hidden.contains(&"claude") {
        let platform = if ctx.platform.is_empty() {
            "mac"
        } else {
            ctx.platform.as_str()
        };
        sections.push(claude_section(cx, data, &of("claude"), profiles, platform)?);
    }
    if !hidden.contains(&"codex") {
        let all: Vec<Acc> = every
            .iter()
            .filter(|a| is_str(a.get("provider"), "codex"))
            .copied()
            .collect();
        sections.push(codex_section(cx, &of("codex"), &all)?);
    }
    let antigravity_rows = of("antigravity");
    if !hidden.contains(&"antigravity") && !antigravity_rows.is_empty() {
        sections.push(antigravity_section(
            cx,
            raw,
            &antigravity_rows,
            antigravity,
        )?);
    }
    let registry = REGISTRY
        .iter()
        .map(|(id, label, long, sign_in, multi)| RegistryRow {
            id: (*id).into(),
            label: (*label).into(),
            long_label: (*long).into(),
            sign_in: (*sign_in).into(),
            multi: *multi,
            visible: !hidden.contains(id),
            count: raw
                .iter()
                .filter(|a| is_str(get(Some(a), "provider"), id))
                .count() as u64,
        })
        .collect();
    Ok(HomeView {
        version: HOME_VIEW_VERSION,
        sections,
        cards: provider_cards(cx, &accounts, &hidden)?,
        registry,
        chrome: chrome(cx, data, raw, ctx)?,
    })
}
