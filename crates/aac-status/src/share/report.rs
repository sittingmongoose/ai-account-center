//! The agent report documents (`agent-reports/{thread,tasks,card}@1`), ported field for field
//! from `report-schema.ts`, `report-v2.ts`, `card-schema.ts` and `card-sections.ts`: the same
//! allowlist projection, the same validation order (the first broken field names the refusal),
//! the same fixed reasons, and the same key order in the projected document, including the
//! raw members `reportV2` passes through. Lengths count UTF-16 code units, as JavaScript's.

use crate::validate::{
    self, Invalid, Valid, bool_value, count, fail, head, id_problem, is_card_id,
    is_pm_mail_name, is_writer_file_path, iso, link, list, obj, one_of, opt_iso, opt_obj,
    opt_slug, opt_text, option_key, slug, text,
};
use serde_json::{Map, Value as Json, json};

pub const THREAD_SCHEMA: &str = "agent-reports/thread@1";
pub const TASKS_SCHEMA: &str = "agent-reports/tasks@1";
pub const CARD_SCHEMA: &str = "agent-reports/card@1";

/// Size caps from the format (bytes).
pub const THREAD_CAP: usize = 16 * 1024;
pub const TASKS_CAP: usize = 512 * 1024;
pub const CARD_CAP: usize = 64 * 1024;

pub const THREAD_STATES: [&str; 6] = ["working", "waiting", "blocked", "paused", "idle", "done"];
pub const CARD_STATES: [&str; 3] = ["open", "withdrawn", "superseded"];
pub const CARD_KINDS: [&str; 5] = ["question", "objection", "action", "judgement", "sample"];
pub const URGENCIES: [&str; 4] = ["now", "today", "this-week", "whenever"];
const OPTION_STANCES: [&str; 3] = ["keep", "borrow", "own"];
const STAGE_STATES: [&str; 5] = ["done", "now", "waiting", "next", "skipped"];
const STAGE_BY: [&str; 4] = ["thread", "owner", "reviewer", "other"];
const WAITING_ON: [&str; 5] = ["agent", "review", "lock", "resource", "quota"];
const DELIVERABLE_KINDS: [&str; 8] = [
    "concept",
    "report",
    "document",
    "build",
    "decisions",
    "video",
    "screenshots",
    "other",
];

/// `doc[key] ?? fallback`: absent or null takes the fallback (the `??` operator).
fn or<'a>(doc: &'a Map<String, Json>, key: &str, fallback: &'a Json) -> &'a Json {
    match doc.get(key) {
        None | Some(Json::Null) => fallback,
        Some(value) => value,
    }
}

fn present<'a>(doc: &'a Map<String, Json>, key: &str) -> Option<&'a Json> {
    doc.get(key)
}

/// Insert the raw members of `row` named by `keys`, in that order, when present. The
/// `Object.fromEntries(keys.filter(present))` pass-through of reportV2 and the sections.
fn passthrough(row: &Map<String, Json>, keys: &[&str], out: &mut Map<String, Json>) {
    for key in keys {
        if let Some(value) = row.get(*key) {
            out.insert((*key).to_string(), value.clone());
        }
    }
}

/// Additive report data (`reportV2`): validated field by field, retained in the document's
/// own key order; stages and deliverables pass their other members through as written.
pub fn report_v2(doc: &Map<String, Json>) -> Valid<Map<String, Json>> {
    let mut out = Map::new();
    for (key, cap) in [("title", 60usize), ("now", 140), ("next", 100)] {
        if let Some(value) = present(doc, key) {
            out.insert(key.to_string(), json!(text(Some(value), key, cap)?));
        }
    }
    if let Some(value) = present(doc, "readmeQuestions") {
        if !value.is_boolean() {
            return Err(fail("readmeQuestions"));
        }
        out.insert("readmeQuestions".to_string(), value.clone());
    }
    if let Some(value) = present(doc, "progress") {
        if value.is_null() {
            out.insert("progress".to_string(), Json::Null);
        } else {
            let row = obj(Some(value), "progress")?;
            out.insert(
                "progress".to_string(),
                json!({
                    "done": count(row.get("done"), "progress.done")?,
                    "total": count(row.get("total"), "progress.total")?,
                    "unit": match row.get("unit") {
                        None | Some(Json::Null) => Json::Null,
                        other => json!(text(other, "progress.unit", 40)?),
                    },
                }),
            );
        }
    }
    if let Some(value) = present(doc, "stages") {
        let stages = list(Some(value), "stages", 7, |value, field| {
            let row = obj(Some(value), field)?;
            text(row.get("label"), &format!("{field}.label"), 24)?;
            one_of(row.get("state"), &format!("{field}.state"), &STAGE_STATES)?;
            if let Some(by) = present(row, "by") {
                one_of(Some(by), &format!("{field}.by"), &STAGE_BY)?;
            }
            for key in ["round", "of"] {
                if let Some(value) = present(row, key) {
                    if !value.is_null() {
                        let value = count(Some(value), &format!("{field}.{key}"))?;
                        if !(1..=99).contains(&value) {
                            return Err(fail(&format!("{field}.{key}")));
                        }
                    }
                }
            }
            if let Some(note) = present(row, "note") {
                if !note.is_null() {
                    text(Some(note), &format!("{field}.note"), 40)?;
                }
            }
            if let Some(waiting) = present(row, "waitingOn") {
                if !waiting.is_null() {
                    one_of(Some(waiting), &format!("{field}.waitingOn"), &WAITING_ON)?;
                }
            }
            let mut stage = Map::new();
            passthrough(
                row,
                &["label", "state", "by", "waitingOn", "round", "of", "note"],
                &mut stage,
            );
            Ok(Json::Object(stage))
        })?;
        let nows = stages
            .iter()
            .filter(|stage| stage["state"] == json!("now"))
            .count();
        if stages.len() < 2 || nows > 1 {
            return Err(fail("stages"));
        }
        out.insert("stages".to_string(), Json::Array(stages));
    }
    if let Some(value) = present(doc, "deliverables") {
        let deliverables = list(Some(value), "deliverables", 12, |value, field| {
            let row = obj(Some(value), field)?;
            text(row.get("id"), &format!("{field}.id"), 64)?;
            text(row.get("name"), &format!("{field}.name"), 48)?;
            text(row.get("version"), &format!("{field}.version"), 12)?;
            opt_iso(row.get("updatedAt"), &format!("{field}.updatedAt"))?;
            one_of(row.get("kind"), &format!("{field}.kind"), &DELIVERABLE_KINDS)?;
            for (key, cap) in [
                ("path", 400usize),
                ("url", 400),
                ("supersedes", 12),
                ("note", 160),
                ("unlocks", 160),
            ] {
                if let Some(value) = present(row, key) {
                    if !value.is_null() {
                        text(Some(value), &format!("{field}.{key}"), cap)?;
                    }
                }
            }
            if let Some(main) = present(row, "main") {
                if !main.is_boolean() {
                    return Err(fail(&format!("{field}.main")));
                }
            }
            if let Some(cards) = present(row, "cards") {
                list(Some(cards), &format!("{field}.cards"), 100, |value, at| {
                    text(Some(value), at, 200)
                })?;
            }
            let mut row = row.clone();
            if let Some(media) = present(&row, "media") {
                row.insert("media".to_string(), report_media(Some(media))?);
            }
            if let Some(versions) = present(&row, "versions") {
                let versions = list(
                    Some(versions),
                    &format!("{field}.versions"),
                    8,
                    |value, at| {
                        let version = obj(Some(value), at)?;
                        text(version.get("version"), &format!("{at}.version"), 12)?;
                        opt_iso(version.get("updatedAt"), &format!("{at}.updatedAt"))?;
                        let mut out = Map::new();
                        passthrough(version, &["version", "updatedAt", "path", "url"], &mut out);
                        Ok(Json::Object(out))
                    },
                )?;
                row.insert("versions".to_string(), Json::Array(versions));
            }
            let mut out = Map::new();
            passthrough(
                &row,
                &[
                    "id",
                    "name",
                    "kind",
                    "state",
                    "for",
                    "want",
                    "main",
                    "version",
                    "updatedAt",
                    "supersedes",
                    "path",
                    "url",
                    "note",
                    "unlocks",
                    "media",
                    "versions",
                    "cards",
                    "readyAt",
                    "builtAt",
                ],
                &mut out,
            );
            Ok(Json::Object(out))
        })?;
        out.insert("deliverables".to_string(), Json::Array(deliverables));
    }
    Ok(out)
}

/// `reportMedia`: a card's or deliverable's pictures and clips, projected in place.
pub fn report_media(value: Option<&Json>) -> Valid<Json> {
    let rows = list(value, "media", 6, |value, field| {
        let row = obj(Some(value), field)?;
        one_of(row.get("kind"), &format!("{field}.kind"), &["image", "video"])?;
        let path_absent = matches!(row.get("path"), None | Some(Json::Null));
        let url_absent = matches!(row.get("url"), None | Some(Json::Null));
        if path_absent && url_absent {
            return Err(fail(field));
        }
        for key in ["path", "url", "poster"] {
            if let Some(value) = present(row, key) {
                if !value.is_null() {
                    text(Some(value), &format!("{field}.{key}"), 400)?;
                }
            }
        }
        if let Some(caption) = present(row, "caption") {
            if !caption.is_null() {
                text(Some(caption), &format!("{field}.caption"), 60)?;
            }
        }
        if let Some(label) = present(row, "label") {
            if !label.is_null() {
                one_of(Some(label), &format!("{field}.label"), &["built", "mockup"])?;
            }
        }
        if let Some(seconds) = present(row, "seconds") {
            if !seconds.is_null() {
                count(Some(seconds), &format!("{field}.seconds"))?;
            }
        }
        let mut out = Map::new();
        passthrough(
            row,
            &["kind", "path", "url", "poster", "caption", "label", "seconds"],
            &mut out,
        );
        Ok(Json::Object(out))
    })?;
    Ok(Json::Array(rows))
}

fn blocked_on(value: Option<&Json>, field: &str) -> Valid<Json> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(Json::Null);
    };
    let item = obj(Some(value), field)?;
    Ok(json!({
        "kind": slug(item.get("kind"), &format!("{field}.kind"))?,
        "what": text(item.get("what"), &format!("{field}.what"), 500)?,
        "since": opt_iso(item.get("since"), &format!("{field}.since"))?,
        "short": opt_text(item.get("short"), &format!("{field}.short"), 80)?,
        "until": opt_iso(item.get("until"), &format!("{field}.until"))?,
        "card": opt_text(item.get("card"), &format!("{field}.card"), 200)?,
    }))
}

fn spread(out: &mut Map<String, Json>, head: &validate::DocHead, v2: Map<String, Json>) {
    out.insert("writer".to_string(), json!(head.writer));
    out.insert("seq".to_string(), json!(head.seq));
    out.insert("writtenAt".to_string(), json!(head.written_at));
    for (key, value) in v2 {
        out.insert(key, value);
    }
}

/// `thread@1`: the reporting thread's state, agent, progress and links.
pub fn thread(doc: &Map<String, Json>, writer: &str) -> Valid<Json> {
    let agent = match doc.get("agent") {
        None | Some(Json::Null) => Map::new(),
        other => obj(other, "agent")?.clone(),
    };
    let state = one_of(doc.get("state"), "state", &THREAD_STATES)?;
    let mut ttl: Option<u64> = Some(1800);
    if let Some(value) = present(doc, "ttlSeconds") {
        if value.is_null() {
            ttl = None;
        } else {
            let counted = count(Some(value), "ttlSeconds")?;
            if !(60..=7 * 86_400).contains(&counted) {
                return Err(fail("ttlSeconds"));
            }
            ttl = Some(counted);
        }
    }
    let mut progress = Json::Null;
    if let Some(value) = present(doc, "progress") {
        if !value.is_null() {
            let item = obj(Some(value), "progress")?;
            progress = json!({
                "done": count(item.get("done"), "progress.done")?,
                "total": count(item.get("total"), "progress.total")?,
                "unit": opt_text(item.get("unit"), "progress.unit", 40)?,
            });
        }
    }
    let head = head(doc, THREAD_SCHEMA, writer)?;
    let v2 = report_v2(doc)?;
    let mut out = Map::new();
    spread(&mut out, &head, v2);
    out.insert("ttlSeconds".to_string(), json!(ttl));
    out.insert(
        "agent".to_string(),
        json!({
            "name": opt_text(agent.get("name"), "agent.name", 64)?,
            "harness": opt_text(agent.get("harness"), "agent.harness", 64)?,
            "model": opt_text(agent.get("model"), "agent.model", 120)?,
            "effort": opt_text(agent.get("effort"), "agent.effort", 40)?,
            "account": opt_text(agent.get("account"), "agent.account", 120)?,
            "machine": opt_text(agent.get("machine"), "agent.machine", 64)?,
            "t3Thread": opt_text(agent.get("t3Thread"), "agent.t3Thread", 200)?,
            "parent": opt_text(agent.get("parent"), "agent.parent", 64)?,
        }),
    );
    out.insert("project".to_string(), json!(opt_slug(doc.get("project"), "project")?));
    out.insert("stream".to_string(), json!(opt_slug(doc.get("stream"), "stream")?));
    out.insert("state".to_string(), json!(state));
    out.insert("doing".to_string(), json!(opt_text(doc.get("doing"), "doing", 500)?));
    out.insert("blockedOn".to_string(), blocked_on(doc.get("blockedOn"), "blockedOn")?);
    // The spread keeps v2's progress position when both wrote it; the value is this one's.
    out.insert("progress".to_string(), progress);
    out.insert(
        "links".to_string(),
        Json::Array(list(doc.get("links"), "links", 20, |value, field| {
            link(Some(value), field, 120)
        })?),
    );
    Ok(Json::Object(out))
}

fn task_item(value: &Json, field: &str) -> Valid<Json> {
    let row = obj(Some(value), field)?;
    let mut landed = Json::Null;
    if let Some(value) = present(row, "landed") {
        if !value.is_null() {
            let value = obj(Some(value), &format!("{field}.landed"))?;
            landed = json!({
                "sha": text(value.get("sha"), &format!("{field}.landed.sha"), 64)?,
                "at": opt_iso(value.get("at"), &format!("{field}.landed.at"))?,
            });
        }
    }
    Ok(json!({
        "id": text(row.get("id"), &format!("{field}.id"), 64)?,
        "title": text(row.get("title"), &format!("{field}.title"), 300)?,
        "state": text(row.get("state"), &format!("{field}.state"), 64)?,
        "owner": opt_text(row.get("owner"), &format!("{field}.owner"), 64)?,
        "branch": opt_text(row.get("branch"), &format!("{field}.branch"), 200)?,
        "head": opt_text(row.get("head"), &format!("{field}.head"), 64)?,
        "reviews": list(row.get("reviews"), &format!("{field}.reviews"), 20, |review, at| {
            let value = obj(Some(review), at)?;
            Ok(json!({
                "by": text(value.get("by"), &format!("{at}.by"), 120)?,
                "verdict": text(value.get("verdict"), &format!("{at}.verdict"), 40)?,
                "at": opt_iso(value.get("at"), &format!("{at}.at"))?,
            }))
        })?,
        "landed": landed,
        "blockedOn": blocked_on(row.get("blockedOn"), &format!("{field}.blockedOn"))?,
        "note": opt_text(row.get("note"), &format!("{field}.note"), 40)?,
        "why": opt_text(row.get("why"), &format!("{field}.why"), 500)?,
        "updatedAt": opt_iso(row.get("updatedAt"), &format!("{field}.updatedAt"))?,
    }))
}

/// `tasks@1`: the thread's task list, its project and stream, and its board path.
pub fn tasks(doc: &Map<String, Json>, writer: &str) -> Valid<Json> {
    let items = list(doc.get("items"), "items", 2000, task_item)?;
    let mut ids = std::collections::HashSet::new();
    for item in &items {
        if !ids.insert(item["id"].as_str().unwrap_or_default().to_string()) {
            return Err(Invalid("items repeat an id".to_string()));
        }
    }
    let head = head(doc, TASKS_SCHEMA, writer)?;
    let v2 = report_v2(doc)?;
    let mut out = Map::new();
    spread(&mut out, &head, v2);
    out.insert("project".to_string(), json!(slug(doc.get("project"), "project")?));
    out.insert(
        "projectLabel".to_string(),
        json!(opt_text(doc.get("projectLabel"), "projectLabel", 120)?),
    );
    out.insert("stream".to_string(), json!(slug(doc.get("stream"), "stream")?));
    out.insert(
        "streamLabel".to_string(),
        json!(opt_text(doc.get("streamLabel"), "streamLabel", 120)?),
    );
    out.insert("owner".to_string(), json!(opt_text(doc.get("owner"), "owner", 64)?));
    out.insert("goal".to_string(), json!(opt_text(doc.get("goal"), "goal", 500)?));
    out.insert("board".to_string(), json!(opt_text(doc.get("board"), "board", 600)?));
    out.insert("items".to_string(), Json::Array(items));
    Ok(Json::Object(out))
}

// ------------------------------------------------------------------------------------
// Card sections and media (card-sections.ts)

const MAX_CARD_SECTIONS: usize = 30;
const MAX_OPTION_SECTIONS: usize = 10;
const MAX_OPTION_MEDIA: usize = 6;

/// `^[A-Za-z][A-Za-z0-9-]{0,39}$`
fn block_type(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    value.chars().count() <= 40 && chars.all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// A picture or a clip in the writer's own folder.
fn card_media(value: &Json, field: &str) -> Valid<Json> {
    let item = obj(Some(value), field)?;
    let writer_path = |value: Option<&Json>, field: &str| -> Valid<String> {
        match value {
            Some(Json::String(_)) if is_writer_file_path(value) => {
                Ok(value.and_then(Json::as_str).unwrap_or_default().to_string())
            }
            _ => Err(fail(field)),
        }
    };
    Ok(json!({
        "kind": one_of(item.get("kind"), &format!("{field}.kind"), &["image", "video"])?,
        "path": writer_path(item.get("path"), &format!("{field}.path"))?,
        "caption": opt_text(item.get("caption"), &format!("{field}.caption"), 300)?,
        "label": match item.get("label") {
            None | Some(Json::Null) => Json::Null,
            other => json!(one_of(other, &format!("{field}.label"), &["built", "mockup"])?),
        },
        "poster": match item.get("poster") {
            None | Some(Json::Null) => Json::Null,
            other => json!(writer_path(other, &format!("{field}.poster"))?),
        },
    }))
}

/// JavaScript's `\s`: the whitespace `new URL` tolerates and the link rule refuses.
fn js_whitespace(c: char) -> bool {
    matches!(c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' '
        | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}'
        | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

/// A web link (http or https) or a share path the page offers to copy; nothing else.
fn link_url(value: Option<&Json>, field: &str) -> Valid<String> {
    let url = text(value, field, 600)?;
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        // `new URL` needs a non-empty authority; any whitespace refuses the link outright.
        let rest = &url[url.find("://").map(|at| at + 3).unwrap_or(0)..];
        let authority: String = rest
            .chars()
            .take_while(|c| !matches!(c, '/' | '?' | '#'))
            .collect();
        if !authority.is_empty() && !url.chars().any(js_whitespace) {
            return Ok(url);
        }
        return Err(fail(field));
    }
    if url.starts_with('/') || url.starts_with("\\\\") {
        return Ok(url);
    }
    Err(fail(field))
}

fn block_link(value: &Json, field: &str) -> Valid<Json> {
    let item = obj(Some(value), field)?;
    Ok(json!({
        "label": text(item.get("label"), &format!("{field}.label"), 120)?,
        "url": link_url(item.get("url"), &format!("{field}.url"))?,
    }))
}

/// A table cell may be empty.
fn cell(value: &Json, field: &str) -> Valid<String> {
    if value == &json!("") {
        return Ok(String::new());
    }
    text(Some(value), field, 300)
}

fn section_base(raw: &Map<String, Json>, field: &str) -> Valid<Map<String, Json>> {
    let mut base = Map::new();
    base.insert(
        "title".to_string(),
        json!(opt_text(raw.get("title"), &format!("{field}.title"), 120)?),
    );
    base.insert(
        "fold".to_string(),
        json!(bool_value(raw.get("fold"), &format!("{field}.fold"), false)?),
    );
    Ok(base)
}

/// One content block: known types validated strictly, an unknown type passed through as
/// `{title, fold, type, unknown: true}` so the page can say it cannot show it.
pub fn section(value: &Json, field: &str) -> Valid<Json> {
    let raw = obj(Some(value), field)?;
    let kind = match raw.get("type") {
        Some(Json::String(kind)) if block_type(kind) => kind.clone(),
        _ => return Err(fail(&format!("{field}.type"))),
    };
    let mut out = section_base(raw, field)?;
    out.insert("type".to_string(), json!(kind));
    let items = |max: usize,
                 each: &dyn Fn(&Json, &str) -> Valid<Json>|
     -> Valid<Vec<Json>> {
        list(raw.get("items"), &format!("{field}.items"), max, |item, at| {
            each(item, at)
        })
    };
    let line = |max: usize| -> Box<dyn Fn(&Json, &str) -> Valid<Json>> {
        Box::new(move |item, at| Ok(json!(text(Some(item), at, max)?)))
    };
    match kind.as_str() {
        "text" => {
            out.insert(
                "body".to_string(),
                json!(text(raw.get("body"), &format!("{field}.body"), 4000)?),
            );
        }
        "list" => {
            out.insert("items".to_string(), Json::Array(items(50, &line(500))?));
            out.insert(
                "ordered".to_string(),
                json!(bool_value(raw.get("ordered"), &format!("{field}.ordered"), false)?),
            );
        }
        "facts" => {
            out.insert(
                "rows".to_string(),
                Json::Array(list(raw.get("rows"), &format!("{field}.rows"), 30, |row, at| {
                    let r = obj(Some(row), at)?;
                    Ok(json!({
                        "label": text(r.get("label"), &format!("{at}.label"), 80)?,
                        "value": text(r.get("value"), &format!("{at}.value"), 300)?,
                    }))
                })?),
            );
        }
        "table" => {
            let columns = list(raw.get("columns"), &format!("{field}.columns"), 8, |item, at| {
                Ok(json!(text(Some(item), at, 80)?))
            })?;
            if columns.is_empty() {
                return Err(fail(&format!("{field}.columns")));
            }
            let rows = list(raw.get("rows"), &format!("{field}.rows"), 30, |row, at| {
                let cells = list(Some(row), at, columns.len(), |value, cell_at| {
                    Ok(json!(cell(value, cell_at)?))
                })?;
                if cells.len() != columns.len() {
                    return Err(fail(at));
                }
                Ok(Json::Array(cells))
            })?;
            out.insert("columns".to_string(), Json::Array(columns));
            out.insert("rows".to_string(), Json::Array(rows));
        }
        "compare" => {
            out.insert(
                "items".to_string(),
                Json::Array(items(12, &|item, at| {
                    let r = obj(Some(item), at)?;
                    Ok(json!({
                        "name": text(r.get("name"), &format!("{at}.name"), 120)?,
                        "does": opt_text(r.get("does"), &format!("{at}.does"), 1000)?,
                        "problem": opt_text(r.get("problem"), &format!("{at}.problem"), 1000)?,
                        "link": match r.get("link") {
                            None | Some(Json::Null) => Json::Null,
                            Some(other) => block_link(other, &format!("{at}.link"))?,
                        },
                        "version": opt_text(r.get("version"), &format!("{at}.version"), 60)?,
                    }))
                })?),
            );
        }
        "notCopying" => {
            out.insert(
                "items".to_string(),
                Json::Array(items(20, &|item, at| {
                    let r = obj(Some(item), at)?;
                    Ok(json!({
                        "what": text(r.get("what"), &format!("{at}.what"), 300)?,
                        "why": text(r.get("why"), &format!("{at}.why"), 1000)?,
                    }))
                })?),
            );
        }
        "unknowns" => {
            out.insert("items".to_string(), Json::Array(items(20, &line(500))?));
        }
        "terms" => {
            out.insert(
                "items".to_string(),
                Json::Array(items(20, &|item, at| {
                    let r = obj(Some(item), at)?;
                    Ok(json!({
                        "term": text(r.get("term"), &format!("{at}.term"), 80)?,
                        "meaning": text(r.get("meaning"), &format!("{at}.meaning"), 500)?,
                    }))
                })?),
            );
        }
        "callout" => {
            out.insert(
                "tone".to_string(),
                json!(one_of(raw.get("tone"), &format!("{field}.tone"), &["info", "warn", "crit"])?),
            );
            out.insert(
                "body".to_string(),
                json!(text(raw.get("body"), &format!("{field}.body"), 2000)?),
            );
        }
        "links" => {
            out.insert(
                "items".to_string(),
                Json::Array(items(20, &|item, at| block_link(item, at))?),
            );
        }
        "media" => {
            out.insert(
                "items".to_string(),
                Json::Array(items(12, &|item, at| card_media(item, at))?),
            );
        }
        "quote" => {
            out.insert(
                "items".to_string(),
                Json::Array(items(20, &|item, at| {
                    let r = obj(Some(item), at)?;
                    Ok(json!({
                        "text": text(r.get("text"), &format!("{at}.text"), 4000)?,
                        "where": opt_text(r.get("where"), &format!("{at}.where"), 300)?,
                        "context": list(r.get("context"), &format!("{at}.context"), 10, |value, cell_at| {
                            Ok(json!(text(Some(value), cell_at, 300)?))
                        })?,
                        "source": opt_text(r.get("source"), &format!("{at}.source"), 300)?,
                        "link": match r.get("link") {
                            None | Some(Json::Null) => Json::Null,
                            Some(other) => block_link(other, &format!("{at}.link"))?,
                        },
                        "version": opt_text(r.get("version"), &format!("{at}.version"), 60)?,
                    }))
                })?),
            );
        }
        "planChange" => {
            out.insert(
                "doc".to_string(),
                json!(text(raw.get("doc"), &format!("{field}.doc"), 300)?),
            );
            out.insert(
                "unit".to_string(),
                json!(text(raw.get("unit"), &format!("{field}.unit"), 200)?),
            );
            out.insert(
                "now".to_string(),
                json!(text(raw.get("now"), &format!("{field}.now"), 4000)?),
            );
            out.insert(
                "proposed".to_string(),
                json!(text(raw.get("proposed"), &format!("{field}.proposed"), 4000)?),
            );
        }
        _ => {
            out.insert("unknown".to_string(), json!(true));
        }
    }
    Ok(Json::Object(out))
}

// ------------------------------------------------------------------------------------
// The card document (card-schema.ts)

/// `card@1`: the decision card, its options, sections and answer metadata.
pub fn card(doc: &Map<String, Json>, writer: &str, file_id: &str) -> Valid<Json> {
    let base = head(doc, CARD_SCHEMA, writer)?;
    if doc.get("id").and_then(Json::as_str) != Some(file_id) {
        return Err(Invalid(
            "id does not match its file name and writer".to_string(),
        ));
    }
    let id_refusal = id_problem(doc.get("id"), writer, 64);
    if id_refusal.is_some() || !is_card_id(doc.get("id")) {
        return Err(Invalid(
            id_refusal.unwrap_or_else(|| "id does not match its file name and writer".to_string()),
        ));
    }
    let reply_to = match doc.get("replyTo") {
        None | Some(Json::Null) => Map::new(),
        other => obj(other, "replyTo")?.clone(),
    };
    let pm_mail = match reply_to.get("pmMail") {
        None | Some(Json::Null) => Json::Null,
        Some(value) => value.clone(),
    };
    if !pm_mail.is_null() && !is_pm_mail_name(Some(&pm_mail)) {
        return Err(fail("replyTo.pmMail"));
    }
    let options = list(doc.get("options"), "options", 4, |value, at| {
        let option = obj(Some(value), at)?;
        match option.get("key") {
            Some(Json::String(key)) if option_key(key) => {}
            _ => return Err(fail(&format!("{at}.key"))),
        }
        Ok(json!({
            "key": option.get("key").cloned().unwrap_or(Json::Null),
            "label": text(option.get("label"), &format!("{at}.label"), 300)?,
            "gets": opt_text(option.get("gets"), &format!("{at}.gets"), 1000)?,
            "costs": opt_text(option.get("costs"), &format!("{at}.costs"), 1000)?,
            "media": list(option.get("media"), &format!("{at}.media"), MAX_OPTION_MEDIA, card_media)?,
            "sections": list(option.get("sections"), &format!("{at}.sections"), MAX_OPTION_SECTIONS, section)?,
            "stance": match option.get("stance") {
                None | Some(Json::Null) => Json::Null,
                other => json!(one_of(other, &format!("{at}.stance"), &OPTION_STANCES)?),
            },
        }))
    })?;
    let kind = one_of(Some(or(doc, "kind", &json!("question"))), "kind", &CARD_KINDS)?;
    if (kind == "question" || kind == "judgement") && options.len() < 2 {
        return Err(Invalid("a card needs 2 to 4 options".to_string()));
    }
    if (kind == "objection" || kind == "sample") && options.len() == 1 {
        return Err(Invalid(
            if kind == "sample" {
                "a sample has no options or 2 to 4".to_string()
            } else {
                "an objection has no options or 2 to 4".to_string()
            },
        ));
    }
    if kind == "action" && !options.is_empty() {
        return Err(Invalid("an action card has no options".to_string()));
    }
    let mut keys = std::collections::HashSet::new();
    for option in &options {
        if !keys.insert(option["key"].as_str().unwrap_or_default().to_string()) {
            return Err(Invalid("options repeat a key".to_string()));
        }
    }
    let mut recommendation = Json::Null;
    if let Some(value) = present(doc, "recommendation") {
        if !value.is_null() {
            let value = obj(Some(value), "recommendation")?;
            let key = value.get("key").and_then(Json::as_str).unwrap_or_default();
            if !options.iter().any(|option| option["key"] == json!(key)) {
                return Err(fail("recommendation.key"));
            }
            recommendation = json!({
                "key": value.get("key").cloned().unwrap_or(Json::Null),
                "why": opt_text(value.get("why"), "recommendation.why", 1000)?,
                "confidence": match value.get("confidence") {
                    None | Some(Json::Null) => Json::Null,
                    other => json!(one_of(other, "recommendation.confidence", &["low", "medium", "high"])?),
                },
                "flip": opt_text(value.get("flip"), "recommendation.flip", 500)?,
            });
        }
    }
    let mut old_answer = Json::Null;
    if let Some(value) = present(doc, "oldAnswer") {
        if !value.is_null() {
            let value = obj(Some(value), "oldAnswer")?;
            old_answer = json!({
                "text": text(value.get("text"), "oldAnswer.text", 2000)?,
                "at": opt_iso(value.get("at"), "oldAnswer.at")?,
            });
        }
    }
    let state = one_of(Some(or(doc, "state", &json!("open"))), "state", &CARD_STATES)?;
    let local = &file_id[file_id.find("--").map(|at| at + 2).unwrap_or(0)..];
    let mut out = Map::new();
    spread(&mut out, &base, Map::new());
    if let Some(media) = present(doc, "media") {
        out.insert("media".to_string(), report_media(Some(media))?);
    }
    out.insert("id".to_string(), json!(file_id));
    out.insert(
        "label".to_string(),
        json!(opt_text(doc.get("label"), "label", 32)?.unwrap_or_else(|| local.to_string())),
    );
    out.insert("project".to_string(), json!(opt_slug(doc.get("project"), "project")?));
    out.insert("stream".to_string(), json!(opt_slug(doc.get("stream"), "stream")?));
    out.insert("askedBy".to_string(), json!(opt_text(doc.get("askedBy"), "askedBy", 64)?));
    out.insert(
        "replyTo".to_string(),
        json!({
            "pmMail": pm_mail,
            "t3Thread": opt_text(reply_to.get("t3Thread"), "replyTo.t3Thread", 200)?,
        }),
    );
    out.insert("state".to_string(), json!(state));
    out.insert(
        "supersededBy".to_string(),
        json!(if state == "superseded" {
            opt_text(doc.get("supersededBy"), "supersededBy", 130)?
        } else {
            None
        }),
    );
    out.insert(
        "reason".to_string(),
        json!(if state == "open" {
            None
        } else {
            opt_text(doc.get("reason"), "reason", 500)?
        }),
    );
    out.insert("title".to_string(), json!(text(doc.get("title"), "title", 300)?));
    out.insert(
        "now".to_string(),
        json!(opt_text(Some(or(doc, "now", doc.get("context").unwrap_or(&Json::Null))), "now", 4000)?),
    );
    out.insert("nowAsOf".to_string(), json!(opt_iso(doc.get("nowAsOf"), "nowAsOf")?));
    out.insert("oldAnswer".to_string(), old_answer);
    out.insert(
        "whatChanged".to_string(),
        json!(opt_text(doc.get("whatChanged"), "whatChanged", 2000)?),
    );
    out.insert("options".to_string(), Json::Array(options));
    out.insert("recommendation".to_string(), recommendation);
    out.insert(
        "multiple".to_string(),
        json!(bool_value(doc.get("multiple"), "multiple", false)?),
    );
    out.insert(
        "urgency".to_string(),
        json!(one_of(Some(or(doc, "urgency", &json!("whenever"))), "urgency", &URGENCIES)?),
    );
    out.insert("blocks".to_string(), json!(opt_text(doc.get("blocks"), "blocks", 300)?));
    out.insert(
        "sections".to_string(),
        Json::Array(list(doc.get("sections"), "sections", MAX_CARD_SECTIONS, section)?),
    );
    out.insert("batch".to_string(), json!(opt_slug(doc.get("batch"), "batch")?));
    out.insert(
        "evidence".to_string(),
        Json::Array(list(doc.get("evidence"), "evidence", 20, |value, field| {
            link(Some(value), field, 120)
        })?),
    );
    out.insert("kind".to_string(), json!(kind));
    out.insert(
        "neededBy".to_string(),
        opt_obj(doc.get("neededBy"), "neededBy", |value| {
            Ok(json!({
                "at": opt_iso(value.get("at"), "neededBy.at")?,
                "ifNoAnswer": opt_text(value.get("ifNoAnswer"), "neededBy.ifNoAnswer", 500)?,
            }))
        })?
        .map(|value| value)
        .unwrap_or(Json::Null),
    );
    out.insert(
        "exception".to_string(),
        opt_obj(doc.get("exception"), "exception", |value| {
            Ok(json!({
                "rule": text(value.get("rule"), "exception.rule", 300)?,
                "text": text(value.get("text"), "exception.text", 1000)?,
            }))
        })?
        .unwrap_or(Json::Null),
    );
    out.insert(
        "reversible".to_string(),
        opt_obj(doc.get("reversible"), "reversible", |value| {
            match value.get("yes") {
                Some(Json::Bool(yes)) => Ok(json!({
                    "yes": yes,
                    "how": opt_text(value.get("how"), "reversible.how", 500)?,
                })),
                _ => Err(fail("reversible.yes")),
            }
        })?
        .unwrap_or(Json::Null),
    );
    out.insert("verdict".to_string(), json!(opt_text(doc.get("verdict"), "verdict", 40)?));
    out.insert("group".to_string(), json!(opt_text(doc.get("group"), "group", 80)?));
    out.insert(
        "agreement".to_string(),
        opt_obj(doc.get("agreement"), "agreement", |value| {
            if let Some(same) = present(value, "same") {
                if !same.is_null() && !same.is_boolean() {
                    return Err(fail("agreement.same"));
                }
            }
            Ok(json!({
                "same": match value.get("same") {
                    Some(Json::Bool(same)) => json!(same),
                    _ => Json::Null,
                },
                "note": opt_text(value.get("note"), "agreement.note", 500)?,
            }))
        })?
        .unwrap_or(Json::Null),
    );
    out.insert("doubt".to_string(), json!(opt_text(doc.get("doubt"), "doubt", 1000)?));
    out.insert(
        "appliesTo".to_string(),
        opt_obj(doc.get("appliesTo"), "appliesTo", |value| {
            Ok(json!({
                "count": match value.get("count") {
                    None | Some(Json::Null) => Json::Null,
                    other => json!(count(other, "appliesTo.count")?),
                },
                "description": text(value.get("description"), "appliesTo.description", 500)?,
            }))
        })?
        .unwrap_or(Json::Null),
    );
    out.insert(
        "changesCanon".to_string(),
        Json::Array(list(doc.get("changesCanon"), "changesCanon", 20, |value, at| {
            let item = obj(Some(value), at)?;
            Ok(json!({
                "ref": text(item.get("ref"), &format!("{at}.ref"), 200)?,
                "what": text(item.get("what"), &format!("{at}.what"), 500)?,
            }))
        })?),
    );
    out.insert(
        "related".to_string(),
        Json::Array(list(doc.get("related"), "related", 20, |value, at| {
            Ok(json!(text(Some(value), at, 130)?))
        })?),
    );
    out.insert(
        "replies".to_string(),
        Json::Array(list(doc.get("replies"), "replies", 50, |value, at| {
            let reply = obj(Some(value), at)?;
            Ok(json!({
                "at": iso(reply.get("at"), &format!("{at}.at"))?,
                "text": text(reply.get("text"), &format!("{at}.text"), 2000)?,
            }))
        })?),
    );
    out.insert(
        "askedAt".to_string(),
        json!(iso(
            Some(or(doc, "askedAt", doc.get("writtenAt").unwrap_or(&Json::Null))),
            "askedAt"
        )?),
    );
    Ok(Json::Object(out))
}

/// Bytes of one report file -> its projected document and the bytes' SHA-256, or the fixed
/// refusal reason (`parseReportFile` for thread, tasks and card; runs are aac-store's).
pub fn parse_report_file(
    kind: &str,
    bytes: &[u8],
    writer: &str,
    card_id: &str,
) -> Result<(Json, String), String> {
    let cap = match kind {
        "thread" => THREAD_CAP,
        "tasks" => TASKS_CAP,
        "card" => CARD_CAP,
        _ => return Err("could not be read".to_string()),
    };
    let raw = validate::json_from_bytes(bytes, cap)?;
    let doc = obj(Some(&raw), "document").map_err(|Invalid(reason)| reason)?;
    let projected = match kind {
        "thread" => thread(doc, writer),
        "tasks" => tasks(doc, writer),
        "card" => card(doc, writer, card_id),
        _ => return Err("could not be read".to_string()),
    };
    match projected {
        Ok(doc) => Ok((doc, super::sha256_hex(bytes))),
        Err(Invalid(reason)) => Err(reason),
    }
}
