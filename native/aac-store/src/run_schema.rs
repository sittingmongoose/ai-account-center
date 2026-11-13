//! The `agent-reports/run@1` projection, ported from `src/web-server/status/run-schema.ts` and its
//! helpers (`schema-helpers.ts`): the same fields, bounds and fixed refusal words, so a run the
//! TypeScript reader accepts is accepted here with the same projection, and a refusal reads the
//! same. Lengths count UTF-16 code units, as JavaScript's `length` does. The secret check is not
//! here: the TypeScript `jsonFromBytes` is the one secret filter (owner decision 2026-11-09), and
//! only text it accepted reaches this module.

use serde_json::{Map, Value as Json, json};

pub const RUN_SCHEMA: &str = "agent-reports/run@1";
pub const RUN_CAP_BYTES: usize = 32 * 1024;
pub const RUN_TASK_ID_MAX: usize = 512;
pub const RUN_EVIDENCE_LABEL_MAX: usize = 300;
pub const RUN_LOCAL_ID_MAX: usize = 128;
pub const RUN_ID_MAX: usize = 64 + 2 + RUN_LOCAL_ID_MAX;
pub const RUN_FLAG_KINDS: [&str; 7] = [
    "shortcut",
    "cheating",
    "incomplete-read",
    "fabricated",
    "test-weakened",
    "scope-cut",
    "false-claim",
];

/// A refusal in the reader's fixed words.
#[derive(Debug, Clone, PartialEq)]
pub struct Invalid(pub String);

type R<T> = std::result::Result<T, Invalid>;

fn fail<T>(field: &str) -> R<T> {
    Err(Invalid(format!("{field} is missing or not valid")))
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Control characters other than tab and newline, and bidirectional overrides.
fn has_control(s: &str) -> bool {
    s.chars().any(|c| {
        let u = c as u32;
        (u <= 0x08)
            || (0x0b..=0x1f).contains(&u)
            || (0x7f..=0x9f).contains(&u)
            || u == 0x061c
            || u == 0x200e
            || u == 0x200f
            || (0x202a..=0x202e).contains(&u)
            || (0x2066..=0x2069).contains(&u)
    })
}

fn absent(v: Option<&Json>) -> bool {
    matches!(v, None | Some(Json::Null))
}

fn obj<'a>(v: Option<&'a Json>, field: &str) -> R<&'a Map<String, Json>> {
    match v {
        Some(Json::Object(m)) => Ok(m),
        _ => fail(field),
    }
}

fn text(v: Option<&Json>, field: &str, max: usize) -> R<String> {
    match v {
        Some(Json::String(s)) if !s.is_empty() && utf16_len(s) <= max && !has_control(s) => Ok(s.clone()),
        _ => fail(field),
    }
}

fn opt_text(v: Option<&Json>, field: &str, max: usize) -> R<Option<String>> {
    match v {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(s)) if s.is_empty() => Ok(None),
        _ => text(v, field, max).map(Some),
    }
}

fn bounded_text(v: Option<&Json>, field: &str, max: usize) -> R<String> {
    if let Some(Json::String(s)) = v {
        let len = utf16_len(s);
        if len > max {
            return Err(Invalid(format!(
                "{field} is too long: {len} characters, over the {max} allowed"
            )));
        }
    }
    text(v, field, max)
}

fn is_slug(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_alphanumeric()
        && b[1..].iter().all(|c| c.is_ascii_alphanumeric() || b"._-".contains(c))
}

fn opt_slug(v: Option<&Json>, field: &str) -> R<Option<String>> {
    match v {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(s)) if is_slug(s) => Ok(Some(s.clone())),
        _ => fail(field),
    }
}

const SAFE: f64 = 9_007_199_254_740_991.0;

/// A whole number from 0 to 2^53 - 1 (`Number.isSafeInteger` and not negative).
fn count(v: Option<&Json>, field: &str) -> R<u64> {
    match v {
        Some(Json::Number(n)) => {
            if let Some(u) = n.as_u64() {
                if (u as f64) <= SAFE {
                    return Ok(u);
                }
            } else if let Some(f) = n.as_f64()
                && f.fract() == 0.0
                && (0.0..=SAFE).contains(&f)
            {
                return Ok(f as u64);
            }
            fail(field)
        }
        _ => fail(field),
    }
}

fn opt_count(v: Option<&Json>, field: &str) -> R<Option<u64>> {
    if absent(v) { Ok(None) } else { count(v, field).map(Some) }
}

/// A finite number, or null.
fn num(v: Option<&Json>, field: &str) -> R<Option<Json>> {
    match v {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Number(n)) => Ok(Some(Json::Number(n.clone()))),
        _ => fail(field),
    }
}

fn num_f64(n: &Json) -> f64 {
    n.as_f64().unwrap_or(0.0)
}

/// A time or an amount spent: known and at least 0, or null.
fn amount(v: Option<&Json>, field: &str) -> R<Option<Json>> {
    let n = num(v, field)?;
    if let Some(value) = &n
        && num_f64(value) < 0.0
    {
        return fail(field);
    }
    Ok(n)
}

fn bool_or(v: Option<&Json>, field: &str, fallback: bool) -> R<bool> {
    match v {
        None | Some(Json::Null) => Ok(fallback),
        Some(Json::Bool(b)) => Ok(*b),
        _ => fail(field),
    }
}

fn one_of(v: Option<&Json>, field: &str, allowed: &[&str]) -> R<String> {
    match v {
        Some(Json::String(s)) if allowed.contains(&s.as_str()) => Ok(s.clone()),
        _ => fail(field),
    }
}

fn digits(s: &str, at: usize, n: usize) -> Option<u32> {
    let part = s.get(at..at + n)?;
    if part.bytes().all(|b| b.is_ascii_digit()) {
        part.parse().ok()
    } else {
        None
    }
}

/// The reader's ISO rule plus `realTimestamp` (a real calendar moment, offset within +-14:00).
pub fn real_iso(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 17 {
        return false;
    }
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute)) = (
        digits(s, 0, 4),
        digits(s, 5, 2),
        digits(s, 8, 2),
        digits(s, 11, 2),
        digits(s, 14, 2),
    ) else {
        return false;
    };
    if b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' {
        return false;
    }
    let mut at = 16;
    let mut second = 0;
    if b.get(at) == Some(&b':') {
        let Some(sec) = digits(s, at + 1, 2) else { return false };
        second = sec;
        at += 3;
        if b.get(at) == Some(&b'.') {
            let frac = b[at + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
            if !(1..=9).contains(&frac) {
                return false;
            }
            at += 1 + frac;
        }
    }
    match b.get(at) {
        Some(b'Z') if at + 1 == b.len() => {}
        Some(b'+') | Some(b'-') if at + 6 == b.len() && b[at + 3] == b':' => {
            let (Some(oh), Some(om)) = (digits(s, at + 1, 2), digits(s, at + 4, 2)) else {
                return false;
            };
            if om > 59 || oh * 60 + om > 14 * 60 {
                return false;
            }
        }
        _ => return false,
    }
    if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 {
        return false;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let last = match month {
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (1..=last).contains(&day)
}

/// Epoch milliseconds of a time `real_iso` accepted.
pub fn iso_ms(s: &str) -> Option<i64> {
    if !real_iso(s) {
        return None;
    }
    let n = |a: usize, l: usize| digits(s, a, l).map(|v| v as i64);
    let (year, month, day, hour, minute) = (n(0, 4)?, n(5, 2)?, n(8, 2)?, n(11, 2)?, n(14, 2)?);
    let b = s.as_bytes();
    let mut at = 16;
    let mut second = 0;
    let mut millis = 0i64;
    if b.get(at) == Some(&b':') {
        second = n(at + 1, 2)?;
        at += 3;
        if b.get(at) == Some(&b'.') {
            let frac: String = b[at + 1..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .map(|c| *c as char)
                .collect();
            let three: String = frac.chars().chain(std::iter::repeat('0')).take(3).collect();
            millis = three.parse().ok()?;
            at += 1 + frac.len();
        }
    }
    let offset_min = match b.get(at) {
        Some(b'+') => n(at + 1, 2)? * 60 + n(at + 4, 2)?,
        Some(b'-') => -(n(at + 1, 2)? * 60 + n(at + 4, 2)?),
        _ => 0,
    };
    let days = days_from_civil(year, month as u32, day as u32);
    Some(((days * 24 + hour) * 60 + minute - offset_min) * 60_000 + second * 1000 + millis)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn iso(v: Option<&Json>, field: &str) -> R<String> {
    match v {
        Some(Json::String(s)) if real_iso(s) => Ok(s.clone()),
        _ => fail(field),
    }
}

fn opt_iso(v: Option<&Json>, field: &str) -> R<Option<String>> {
    if absent(v) { Ok(None) } else { iso(v, field).map(Some) }
}

fn opt_obj(v: Option<&Json>, field: &str, read: impl FnOnce(&Map<String, Json>) -> R<Json>) -> R<Json> {
    if absent(v) {
        Ok(Json::Null)
    } else {
        read(obj(v, field)?)
    }
}

fn list(v: Option<&Json>, field: &str, max: usize, mut each: impl FnMut(&Json, &str) -> R<Json>) -> R<Json> {
    match v {
        None | Some(Json::Null) => Ok(Json::Array(Vec::new())),
        Some(Json::Array(items)) if items.len() <= max => Ok(Json::Array(
            items
                .iter()
                .enumerate()
                .map(|(i, item)| each(item, &format!("{field}[{i}]")))
                .collect::<R<Vec<_>>>()?,
        )),
        _ => fail(field),
    }
}

fn opt_json<T: Into<Json>>(v: Option<T>) -> Json {
    v.map(Into::into).unwrap_or(Json::Null)
}

fn tokens(v: Option<&Json>, field: &str) -> R<Json> {
    opt_obj(v, field, |t| {
        Ok(json!({
            "in": opt_json(opt_count(t.get("in"), &format!("{field}.in"))?),
            "out": opt_json(opt_count(t.get("out"), &format!("{field}.out"))?),
            "cached": opt_json(opt_count(t.get("cached"), &format!("{field}.cached"))?),
        }))
    })
}

fn quota(v: Option<&Json>, field: &str) -> R<Json> {
    opt_obj(v, field, |q| {
        let delta = num(q.get("deltaPct"), &format!("{field}.deltaPct"))?;
        let Some(delta) = delta else {
            return fail(&format!("{field}.deltaPct"));
        };
        Ok(json!({
            "account": text(q.get("account"), &format!("{field}.account"), 120)?,
            "window": text(q.get("window"), &format!("{field}.window"), 40)?,
            "deltaPct": delta,
        }))
    })
}

/// Whether `name` is a run file name the reader reads: printable ASCII, at most 200 characters.
pub fn run_file_name_readable(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name.bytes().all(|b| (0x21..=0x7e).contains(&b))
        && !name.contains('/')
        && !name.contains('\\')
}

/// `<machine>.<name>`: lowercase, at most 64 characters, a known machine, no `--`.
pub fn is_writer_name(value: &str, machines: &[&str]) -> bool {
    let b = value.as_bytes();
    let Some(dot) = value.find('.') else { return false };
    let head_ok = dot > 0
        && b[0].is_ascii_lowercase() | b[0].is_ascii_digit()
        && b[..dot]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-');
    let tail = &b[dot + 1..];
    let tail_ok = !tail.is_empty()
        && (tail[0].is_ascii_lowercase() || tail[0].is_ascii_digit())
        && tail
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'.' || *c == b'-');
    value.len() <= 64 && head_ok && tail_ok && machines.contains(&&value[..dot]) && !value.contains("--")
}

fn id_problem(value: Option<&Json>, writer: &str) -> Option<String> {
    let Some(Json::String(id)) = value else {
        return Some("id is missing or not valid".into());
    };
    if id.is_empty() {
        return Some("id is missing or not valid".into());
    }
    let prefix = format!("{writer}--");
    if !id.starts_with(&prefix) {
        return Some(format!(
            "id does not name its writer: it must start with \"{writer}--\""
        ));
    }
    let local = &id[prefix.len()..];
    if local.is_empty() {
        return Some("id has no local part after its writer and \"--\"".into());
    }
    let len = utf16_len(local);
    if len > RUN_LOCAL_ID_MAX {
        return Some(format!(
            "id is too long: the part after \"{writer}--\" is {len} characters, over the {RUN_LOCAL_ID_MAX} allowed"
        ));
    }
    let b = local.as_bytes();
    if !(b[0].is_ascii_alphanumeric() && b.iter().all(|c| c.is_ascii_alphanumeric() || b"._-".contains(c))) {
        return Some(
            "id is not valid: the part after its writer and \"--\" must start with a letter or digit and hold only letters, digits, \".\", \"_\" and \"-\""
                .into(),
        );
    }
    None
}

fn metric_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_alphabetic()
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

/// The run projection of a parsed document, or the reader's refusal. `writer` is the folder's
/// writer name.
pub fn project_run(doc: &Json, writer: &str) -> R<Json> {
    let Json::Object(doc) = doc else {
        return fail("document");
    };
    if doc.get("schema").and_then(Json::as_str) != Some(RUN_SCHEMA) {
        return fail("schema");
    }
    if doc.get("writer").and_then(Json::as_str) != Some(writer) {
        return Err(Invalid("writer does not match its folder".into()));
    }
    let seq = count(doc.get("seq"), "seq")?;
    let written_at = iso(doc.get("writtenAt"), "writtenAt")?;
    if let Some(problem) = id_problem(doc.get("id"), writer) {
        return Err(Invalid(problem));
    }
    let task = obj(doc.get("task"), "task")?;
    let model = obj(doc.get("model"), "model")?;
    let mut metrics = Map::new();
    if !absent(doc.get("metrics")) {
        let raw = obj(doc.get("metrics"), "metrics")?;
        if raw.len() > 50 {
            return fail("metrics");
        }
        for (name, value) in raw {
            if !metric_name(name) {
                return fail("metrics");
            }
            metrics.insert(name.clone(), opt_json(num(Some(value), &format!("metrics.{name}"))?));
        }
    }
    let task_out = json!({
        "id": bounded_text(task.get("id"), "task.id", RUN_TASK_ID_MAX)?,
        "type": opt_json(opt_text(task.get("type"), "task.type", 64)?),
        "title": text(task.get("title"), "task.title", 300)?,
        "size": opt_obj(task.get("size"), "task.size", |s| Ok(json!({
            "records": opt_json(opt_count(s.get("records"), "task.size.records")?),
            "files": opt_json(opt_count(s.get("files"), "task.size.files")?),
            "items": opt_json(opt_count(s.get("items"), "task.size.items")?),
            "words": opt_json(opt_count(s.get("words"), "task.size.words")?),
            "tasks": opt_json(opt_count(s.get("tasks"), "task.size.tasks")?),
        })))?,
    });
    let model_out = json!({
        "id": opt_json(opt_text(model.get("id"), "model.id", 120)?),
        "provider": opt_json(opt_text(model.get("provider"), "model.provider", 64)?),
        "harness": opt_json(opt_text(model.get("harness"), "model.harness", 64)?),
        "effort": opt_json(opt_text(model.get("effort"), "model.effort", 40)?),
        "account": opt_json(opt_text(model.get("account"), "model.account", 120)?),
    });
    let source = one_of(doc.get("source"), "source", &["observed", "benchmark"])?;
    let benchmark = opt_text(doc.get("benchmark"), "benchmark", 120)?;
    let project = opt_slug(doc.get("project"), "project")?;
    let stream = opt_slug(doc.get("stream"), "stream")?;
    let started_at = opt_iso(doc.get("startedAt"), "startedAt")?;
    let ended_at = iso(doc.get("endedAt"), "endedAt")?;
    let wall = amount(doc.get("wallSeconds"), "wallSeconds")?;
    let outcome = text(doc.get("outcome"), "outcome", 40)?;
    let cost = opt_obj(doc.get("cost"), "cost", |c| {
        Ok(json!({
            "usd": opt_json(amount(c.get("usd"), "cost.usd")?),
            "credits": opt_json(amount(c.get("credits"), "cost.credits")?),
            "quota": quota(c.get("quota"), "cost.quota")?,
            "tokens": tokens(c.get("tokens"), "cost.tokens")?,
            "estimate": opt_obj(c.get("estimate"), "cost.estimate", |e| Ok(json!({
                "source": text(e.get("source"), "cost.estimate.source", 80)?,
                "method": text(e.get("method"), "cost.estimate.method", 200)?,
                "credits": opt_json(amount(e.get("credits"), "cost.estimate.credits")?),
                "quota": quota(e.get("quota"), "cost.estimate.quota")?,
                "tokens": tokens(e.get("tokens"), "cost.estimate.tokens")?,
            })))?,
        }))
    })?;
    let scope = opt_obj(doc.get("scope"), "scope", |s| {
        Ok(json!({
            "filesAssigned": opt_json(opt_count(s.get("filesAssigned"), "scope.filesAssigned")?),
            "filesRead": opt_json(opt_count(s.get("filesRead"), "scope.filesRead")?),
            "recordsAssigned": opt_json(opt_count(s.get("recordsAssigned"), "scope.recordsAssigned")?),
            "recordsJudged": opt_json(opt_count(s.get("recordsJudged"), "scope.recordsJudged")?),
        }))
    })?;
    let review = opt_obj(doc.get("review"), "review", |r| {
        let findings = obj(r.get("findings"), "review.findings")?;
        // It decides quality, so a missing one must not read as "not overturned".
        let Some(Json::Bool(overturned)) = r.get("overturned") else {
            return fail("review.overturned");
        };
        // Only an explicit null is unknown: a missing count is refused, as the reader does.
        let finding = |key: &str| -> R<Json> {
            match findings.get(key) {
                Some(Json::Null) => Ok(Json::Null),
                other => Ok(json!(count(other, &format!("review.findings.{key}"))?)),
            }
        };
        Ok(json!({
            "of": text(r.get("of"), "review.of", 200)?,
            "verdict": one_of(r.get("verdict"), "review.verdict", &["pass", "fail", "cannot-verify"])?,
            "findings": { "high": finding("high")?, "medium": finding("medium")?, "low": finding("low")? },
            "overturned": overturned,
            "by": opt_json(opt_text(r.get("by"), "review.by", 120)?),
            "disputed": bool_or(r.get("disputed"), "review.disputed", false)?,
        }))
    })?;
    let flags = list(doc.get("flags"), "flags", 20, |value, at| {
        let flag = obj(Some(value), at)?;
        Ok(json!({
            "kind": one_of(flag.get("kind"), &format!("{at}.kind"), &RUN_FLAG_KINDS)?,
            "detail": text(flag.get("detail"), &format!("{at}.detail"), 1000)?,
            "evidence": text(flag.get("evidence"), &format!("{at}.evidence"), 600)?,
            "reportedBy": opt_json(opt_text(flag.get("reportedBy"), &format!("{at}.reportedBy"), 64)?),
        }))
    })?;
    let evidence = list(doc.get("evidence"), "evidence", 20, |value, at| {
        let item = obj(Some(value), at)?;
        Ok(json!({
            "label": bounded_text(item.get("label"), &format!("{at}.label"), RUN_EVIDENCE_LABEL_MAX)?,
            "path": text(item.get("path"), &format!("{at}.path"), 600)?,
        }))
    })?;
    let process = opt_obj(doc.get("process"), "process", |p| {
        Ok(json!({
            "id": text(p.get("id"), "process.id", 120)?,
            "version": opt_json(opt_text(p.get("version"), "process.version", 60)?),
            "sha256": opt_json(opt_text(p.get("sha256"), "process.sha256", 64)?),
        }))
    })?;
    let imported = opt_obj(doc.get("imported"), "imported", |i| {
        Ok(json!({
            "from": text(i.get("from"), "imported.from", 300)?,
            "at": iso(i.get("at"), "imported.at")?,
        }))
    })?;
    let unknown_model = bool_or(doc.get("unknownModel"), "unknownModel", false)?;
    Ok(json!({
        "writer": writer,
        "seq": seq,
        "writtenAt": written_at,
        "id": doc.get("id").cloned().unwrap_or(Json::Null),
        "source": source,
        "benchmark": opt_json(benchmark),
        "project": opt_json(project),
        "stream": opt_json(stream),
        "task": task_out,
        "model": model_out,
        "startedAt": opt_json(started_at),
        "endedAt": ended_at,
        "wallSeconds": opt_json(wall),
        "outcome": outcome,
        "cost": cost,
        "scope": scope,
        "review": review,
        "flags": flags,
        "evidence": evidence,
        "process": process,
        "metrics": Json::Object(metrics),
        "imported": imported,
        "unknownModel": unknown_model,
    }))
}

/// Bytes of one run file to its projection (the size, NUL, UTF-8 and JSON rules of
/// `jsonFromBytes`, without its secret check, which TypeScript already made).
pub fn parse_run_text(text: &str, writer: &str) -> R<Json> {
    if text.len() > RUN_CAP_BYTES {
        return Err(Invalid(format!("larger than the {} KB cap", RUN_CAP_BYTES / 1024)));
    }
    if text.is_empty() {
        return Err(Invalid("empty".into()));
    }
    if text.as_bytes().contains(&0) {
        return Err(Invalid("contains NUL bytes from a damaged write".into()));
    }
    let value: Json = serde_json::from_str(text).map_err(|_| Invalid("not valid JSON".into()))?;
    project_run(&value, writer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> Json {
        json!({
            "schema": RUN_SCHEMA, "id": "vm.lead--run-1", "writer": "vm.lead", "seq": 1,
            "writtenAt": "2026-11-13T03:40:15Z", "source": "observed",
            "task": { "id": "t-1", "type": "review", "title": "A review" },
            "model": { "id": "gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "max" },
            "endedAt": "2026-11-13T03:40:15Z", "outcome": "done"
        })
    }

    #[test]
    fn a_minimal_run_projects_with_nulls_not_zeros() {
        let run = project_run(&minimal(), "vm.lead").unwrap();
        assert_eq!(run["wallSeconds"], Json::Null);
        assert_eq!(run["cost"], Json::Null);
        assert_eq!(run["flags"], json!([]));
        assert_eq!(run["metrics"], json!({}));
        assert_eq!(run["unknownModel"], json!(false));
        assert_eq!(run["task"]["size"], Json::Null);
        assert_eq!(run["model"]["account"], Json::Null);
    }

    #[test]
    fn refusals_read_as_the_typescript_reader_words() {
        let mut d = minimal();
        d["writer"] = json!("vm.other");
        assert_eq!(
            project_run(&d, "vm.lead").unwrap_err().0,
            "writer does not match its folder"
        );
        let mut d = minimal();
        d["id"] = json!("vm.other--x");
        assert_eq!(
            project_run(&d, "vm.lead").unwrap_err().0,
            "id does not name its writer: it must start with \"vm.lead--\""
        );
        let mut d = minimal();
        d["task"]["id"] = json!("x".repeat(513));
        assert_eq!(
            project_run(&d, "vm.lead").unwrap_err().0,
            "task.id is too long: 513 characters, over the 512 allowed"
        );
        let mut d = minimal();
        d["review"] =
            json!({ "of": "x", "verdict": "pass", "findings": { "high": 0, "medium": 0 }, "overturned": false });
        assert_eq!(
            project_run(&d, "vm.lead").unwrap_err().0,
            "review.findings.low is missing or not valid"
        );
        let mut d = minimal();
        d["review"] = json!({ "of": "x", "verdict": "pass", "findings": { "high": 0, "medium": 0, "low": null } });
        assert_eq!(
            project_run(&d, "vm.lead").unwrap_err().0,
            "review.overturned is missing or not valid"
        );
        let mut d = minimal();
        d["wallSeconds"] = json!(-1);
        assert_eq!(
            project_run(&d, "vm.lead").unwrap_err().0,
            "wallSeconds is missing or not valid"
        );
        let mut d = minimal();
        d["task"]["title"] = json!("bad\u{202e}title");
        assert_eq!(
            project_run(&d, "vm.lead").unwrap_err().0,
            "task.title is missing or not valid"
        );
        let mut d = minimal();
        d["flags"] = json!([{ "kind": "lazy", "detail": "d", "evidence": "e" }]);
        assert_eq!(
            project_run(&d, "vm.lead").unwrap_err().0,
            "flags[0].kind is missing or not valid"
        );
    }

    #[test]
    fn empty_optional_text_reads_as_null_and_tab_newline_are_text() {
        let mut d = minimal();
        d["model"]["effort"] = json!("");
        d["task"]["title"] = json!("two\tparts\nhere");
        let run = project_run(&d, "vm.lead").unwrap();
        assert_eq!(run["model"]["effort"], Json::Null);
    }

    #[test]
    fn calendar_rules() {
        assert!(real_iso("2026-11-13T03:40:15Z"));
        assert!(real_iso("2026-11-13T03:40Z"));
        assert!(real_iso("2026-11-13T03:40:15.123456789+14:00"));
        assert!(real_iso("2024-02-29T00:00:00Z"));
        assert!(!real_iso("2026-02-29T00:00:00Z"));
        assert!(!real_iso("2026-11-13T24:00:00Z"));
        assert!(!real_iso("2026-11-13T03:40:60Z"));
        assert!(!real_iso("2026-11-13T03:40:15+14:01"));
        assert!(!real_iso("2026-11-13 03:40:15Z"));
        assert!(!real_iso("2026-11-13T03:40:15.Z"));
        assert_eq!(iso_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(iso_ms("1970-01-01T01:00:00+01:00"), Some(0));
        assert_eq!(iso_ms("2026-11-13T03:40:15.5Z"), Some(1_794_541_215_500));
    }

    #[test]
    fn counts_accept_whole_doubles_like_javascript() {
        let mut d = minimal();
        d["seq"] = json!(5.0);
        assert_eq!(project_run(&d, "vm.lead").unwrap()["seq"], json!(5));
        d["seq"] = json!(5.5);
        assert!(project_run(&d, "vm.lead").is_err());
    }

    #[test]
    fn writer_names() {
        let machines = ["vm", "mac", "windows", "nas1"];
        assert!(is_writer_name("vm.t3-bdbeeaaa", &machines));
        assert!(is_writer_name("windows.opus-typed-decisions", &machines));
        assert!(!is_writer_name("ubuntu.lead", &machines));
        assert!(!is_writer_name("vm.a--b", &machines));
        assert!(!is_writer_name("vm.Lead", &machines));
        assert!(!is_writer_name("vm.", &machines));
    }
}
