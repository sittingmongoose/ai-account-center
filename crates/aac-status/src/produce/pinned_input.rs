//! G16c: the `composePinned` input the TypeScript collector built.
//!
//! `PinnedThreadsCollector.compose` assembled one object from the reports reader's rows,
//! the answer history and the status documents, then the native composer decided every word.
//! The reports' additive v2 members (`title`, `now`, `next`, `stages`, `deliverables`,
//! `readmeQuestions`, `progress`) are validated and projected here (`report-v2.ts`), the
//! owner waits resolved against closed cards (`activeBlocker`), every `Date.parse` done once,
//! and the pin rows come from the merged pins.
//!
//! A document the projection refuses declines the whole composition (`None`), as the
//! TypeScript `reportV2` throwing kept the previous panel.

use super::answered::{ResolvedWaits, active_blocker};
use crate::answers_engine::card_version;
use crate::pinned::{
    BlockedIn, CardIn, ComposeInput, ItemIn, ItemLiteIn, LandedIn, LoopingIn, MachineProblemIn, PinIn, ProgressIn,
    ReviewIn, StatusIn, StatusProblemIn, TasksIn, ThreadIn,
};
use crate::pins::MergedPin;
use crate::share::reports::AgentReportsState;
use crate::share::status_docs::StatusDocsState;
use aac_num::ecma;
use serde_json::{Map, Value as Json};
use std::collections::{BTreeMap, HashSet};

/// The packaged catalog's `models.json` rows (`packagedModelRows`): the shipped file, never
/// the share's copy, so a label is a function of the release.
fn packaged_models() -> &'static Vec<(String, String)> {
    use std::sync::LazyLock;
    static MODELS: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
        let Ok(text) = std::str::from_utf8(crate::share::seed::embedded_seed("models.json").unwrap_or_default()) else {
            return Vec::new();
        };
        let Ok(raw) = serde_json::from_str::<Json>(text) else {
            return Vec::new();
        };
        raw.get("models")
            .and_then(Json::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        Some((
                            row.get("model")?.as_str()?.to_owned(),
                            row.get("label")?.as_str()?.to_owned(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default()
    });
    &MODELS
}

/// `modelLabelFor`: an exact catalog model, else the reporter's short form, else null.
#[must_use]
pub fn model_label_for(model: Option<&str>) -> Option<String> {
    let model = model?;
    let rows = packaged_models();
    if let Some((_, label)) = rows.iter().find(|(id, _)| id == model) {
        return Some(label.clone());
    }
    let short = model.rsplit('/').next().unwrap_or(model);
    rows.iter()
        .find(|(id, _)| id.rsplit('/').next().unwrap_or(id) == short)
        .map(|(_, label)| label.clone())
}

/// The pinned input, as the collector built it. A row the projection refuses is `None`.
pub struct Input {
    pub input: ComposeInput,
}

fn text_ok(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && !value.chars().any(|c| {
            matches!(
                c,
                '\u{0000}'..='\u{0008}'
                    | '\u{000b}'..='\u{001f}'
                    | '\u{007f}'..='\u{009f}'
                    | '\u{061c}'
                    | '\u{200e}'
                    | '\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
            )
        })
}

fn text(value: Option<&Json>, max: usize) -> Option<String> {
    let value = value?.as_str()?;
    text_ok(value, max).then(|| value.to_owned())
}

fn count(value: Option<&Json>) -> Option<u64> {
    let n = value?.as_f64()?;
    (n.fract() == 0.0 && n >= 0.0 && n <= 9_007_199_254_740_991.0)
        .then(|| u64::try_from(n as i64).ok())
        .flatten()
}

fn one_of(value: Option<&Json>, allowed: &[&str]) -> Option<String> {
    let value = value?.as_str()?;
    allowed.contains(&value).then(|| value.to_owned())
}

fn epoch(value: Option<&Json>) -> Option<i64> {
    let value = value?.as_str()?;
    ecma::parse_instant(value)
}

fn array(value: Option<&Json>) -> &[Json] {
    value.and_then(Json::as_array).map(Vec::as_slice).unwrap_or_default()
}

/// `reportV2(doc)`: the additive report data the pinned updates may draw, validated field by
/// field. `None` is the reference's `fail`.
fn report_v2(doc: &Json) -> Option<Json> {
    let mut out = Map::new();
    for (key, cap) in [("title", 60), ("now", 140), ("next", 100)] {
        if let Some(value) = doc.get(key) {
            out.insert(key.to_owned(), Json::String(text(Some(value), cap)?));
        }
    }
    if let Some(value) = doc.get("readmeQuestions") {
        out.insert("readmeQuestions".to_owned(), Json::Bool(value.as_bool()?));
    }
    if let Some(value) = doc.get("progress") {
        if value.is_null() {
            out.insert("progress".to_owned(), Json::Null);
        } else {
            let row = value.as_object()?;
            let unit = match row.get("unit") {
                None | Some(Json::Null) => Json::Null,
                Some(unit) => Json::String(text(Some(unit), 40)?),
            };
            let mut progress = Map::new();
            progress.insert("done".to_owned(), Json::from(count(row.get("done"))?));
            progress.insert("total".to_owned(), Json::from(count(row.get("total"))?));
            progress.insert("unit".to_owned(), unit);
            out.insert("progress".to_owned(), Json::Object(progress));
        }
    }
    if let Some(value) = doc.get("stages") {
        let rows = value.as_array()?;
        if rows.len() > 7 {
            return None;
        }
        let mut stages = Vec::new();
        for row in rows {
            let row = row.as_object()?;
            text(row.get("label"), 24)?;
            one_of(row.get("state"), &["done", "now", "waiting", "next", "skipped"])?;
            if row.contains_key("by") {
                one_of(row.get("by"), &["thread", "owner", "reviewer", "other"])?;
            }
            for key in ["round", "of"] {
                if row.get(key).is_some_and(|v| !v.is_null()) {
                    let value = count(row.get(key))?;
                    if !(1..=99).contains(&value) {
                        return None;
                    }
                }
            }
            if row.get("note").is_some_and(|v| !v.is_null()) {
                text(row.get("note"), 40)?;
            }
            if row.get("waitingOn").is_some_and(|v| !v.is_null()) {
                one_of(row.get("waitingOn"), &["agent", "review", "lock", "resource", "quota"])?;
            }
            let mut stage = Map::new();
            for key in ["label", "state", "by", "waitingOn", "round", "of", "note"] {
                if let Some(value) = row.get(key) {
                    stage.insert(key.to_owned(), value.clone());
                }
            }
            stages.push(Json::Object(stage));
        }
        if stages.len() < 2 || stages.iter().filter(|row| row["state"] == Json::from("now")).count() > 1 {
            return None;
        }
        out.insert("stages".to_owned(), Json::Array(stages));
    }
    if let Some(value) = doc.get("deliverables") {
        let rows = value.as_array()?;
        if rows.len() > 12 {
            return None;
        }
        let mut deliverables = Vec::new();
        for row in rows {
            let row = row.as_object()?;
            text(row.get("id"), 64)?;
            text(row.get("name"), 48)?;
            text(row.get("version"), 12)?;
            if row.get("updatedAt").is_some_and(|v| !v.is_null()) {
                epoch(row.get("updatedAt"))?;
            }
            one_of(
                row.get("kind"),
                &[
                    "concept",
                    "report",
                    "document",
                    "build",
                    "decisions",
                    "video",
                    "screenshots",
                    "other",
                ],
            )?;
            for (key, cap) in [
                ("path", 400),
                ("url", 400),
                ("supersedes", 12),
                ("note", 160),
                ("unlocks", 160),
            ] {
                if row.get(key).is_some_and(|v| !v.is_null()) {
                    text(row.get(key), cap)?;
                }
            }
            if let Some(main) = row.get("main") {
                main.as_bool()?;
            }
            if row.contains_key("cards") {
                let cards = row.get("cards").and_then(Json::as_array)?;
                if cards.len() > 100 {
                    return None;
                }
                for card in cards {
                    text(Some(card), 200)?;
                }
            }
            if row.contains_key("media") {
                let media = row.get("media").and_then(Json::as_array)?;
                if media.len() > 6 {
                    return None;
                }
                for item in media {
                    let item = item.as_object()?;
                    one_of(item.get("kind"), &["image", "video"])?;
                    if item.get("path").is_none_or(Json::is_null) && item.get("url").is_none_or(Json::is_null) {
                        return None;
                    }
                    for (key, cap) in [("path", 400), ("url", 400), ("poster", 400)] {
                        if item.get(key).is_some_and(|v| !v.is_null()) {
                            text(item.get(key), cap)?;
                        }
                    }
                    if item.get("caption").is_some_and(|v| !v.is_null()) {
                        text(item.get("caption"), 60)?;
                    }
                    if item.get("label").is_some_and(|v| !v.is_null()) {
                        one_of(item.get("label"), &["built", "mockup"])?;
                    }
                    if item.get("seconds").is_some_and(|v| !v.is_null()) {
                        count(item.get("seconds"))?;
                    }
                }
            }
            if row.contains_key("versions") {
                let versions = row.get("versions").and_then(Json::as_array)?;
                if versions.len() > 8 {
                    return None;
                }
                for version in versions {
                    let version = version.as_object()?;
                    text(version.get("version"), 12)?;
                    if version.get("updatedAt").is_some_and(|v| !v.is_null()) {
                        epoch(version.get("updatedAt"))?;
                    }
                }
            }
            let mut out_row = Map::new();
            for key in [
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
            ] {
                if let Some(value) = row.get(key) {
                    out_row.insert(key.to_owned(), value.clone());
                }
            }
            deliverables.push(Json::Object(out_row));
        }
        out.insert("deliverables".to_owned(), Json::Array(deliverables));
    }
    Some(Json::Object(out))
}

fn blocked_on(waits: &ResolvedWaits, writer: &str, blocker: Option<&Json>) -> Option<BlockedIn> {
    let blocker = active_blocker(waits, writer, blocker.cloned())?;
    let object = blocker.as_object()?;
    let mut additions = BTreeMap::new();
    for (key, value) in object {
        if !["kind", "what", "since", "sinceMs"].contains(&key.as_str()) {
            additions.insert(key.clone(), value.clone());
        }
    }
    Some(BlockedIn {
        additions,
        kind: blocker.get("kind")?.as_str()?.to_owned(),
        what: blocker.get("what")?.as_str()?.to_owned(),
        since: blocker.get("since").and_then(Json::as_str).map(str::to_owned),
        since_ms: epoch(blocker.get("since")),
    })
}

fn pin(merged: &MergedPin) -> PinIn {
    PinIn {
        machine: merged.machine.clone(),
        id: merged.id.clone(),
        title: merged.title.clone(),
        pinned_at: merged.pinned_at.clone(),
        pin_order_key: merged.pin_order_key.clone(),
    }
}

/// `PinnedThreadsCollector.compose`'s input for these readings.
///
/// # Errors
/// `None` when a report document fails the v2 projection, a required member is missing, or a
/// thread's `writtenAt` is not text: the reference's composer throw.
pub fn compose_input(
    reports: &AgentReportsState,
    docs: &StatusDocsState,
    pins: &[MergedPin],
    machine_problems: &[MachineProblemIn],
    now_ms: i64,
    closed: &HashSet<String>,
    reads: &ResolvedWaits,
) -> Option<ComposeInput> {
    let mut threads = Vec::with_capacity(reports.threads.len());
    for row in &reports.threads {
        let doc = &row.doc;
        let agent = doc.get("agent").cloned().unwrap_or(Json::Null);
        let model = agent.get("model").and_then(Json::as_str);
        let mut additions = map(report_v2(doc)?)?;
        additions.extend(crate::share::report::report_v3(doc.as_object()?).ok()?);
        // `progress` is a typed thread member; the reference's spread sets it once and the
        // deserializer reads it there, so the composer's extra members never hold it.
        additions.remove("progress");
        threads.push(ThreadIn {
            additions,
            writer: row.writer.clone(),
            written_at: doc.get("writtenAt")?.as_str()?.to_owned(),
            written_at_ms: row.written_at_ms,
            seq: doc.get("seq")?.as_u64()?,
            t3_thread: agent.get("t3Thread").and_then(Json::as_str).map(str::to_owned),
            name: agent.get("name").and_then(Json::as_str).map(str::to_owned),
            harness: agent.get("harness").and_then(Json::as_str).map(str::to_owned),
            model: model.map(str::to_owned),
            model_label: model_label_for(model),
            effort: agent.get("effort").and_then(Json::as_str).map(str::to_owned),
            account: agent.get("account").and_then(Json::as_str).map(str::to_owned),
            project: doc.get("project").and_then(Json::as_str).map(str::to_owned),
            stream: doc.get("stream").and_then(Json::as_str).map(str::to_owned),
            state: doc.get("state").and_then(Json::as_str).unwrap_or("").to_owned(),
            doing: doc.get("doing").and_then(Json::as_str).map(str::to_owned),
            blocked_on: blocked_on(reads, &row.writer, doc.get("blockedOn")),
            progress: progress(doc.get("progress"))?,
            ttl_seconds: doc.get("ttlSeconds").and_then(Json::as_i64),
            problem: row.problem.as_ref().map(|p| p.reason.clone()),
            problem_since_ms: row.problem.as_ref().map(|p| i64::try_from(p.since).unwrap_or(i64::MAX)),
        });
    }
    let mut tasks = Vec::with_capacity(reports.tasks.len());
    for row in &reports.tasks {
        let doc = &row.doc;
        let mut items = Vec::new();
        for item in array(doc.get("items")) {
            let reviews = array(item.get("reviews"))
                .iter()
                .map(|review| ReviewIn {
                    at_ms: epoch(review.get("at")),
                    verdict: review.get("verdict").and_then(Json::as_str).unwrap_or("").to_owned(),
                })
                .collect();
            items.push(ItemIn {
                note: item.get("note").and_then(Json::as_str).map(str::to_owned),
                why: item.get("why").and_then(Json::as_str).map(str::to_owned),
                due: item.get("due").and_then(Json::as_str).map(str::to_owned),
                due_day: item.get("dueDay").and_then(Json::as_str).map(str::to_owned),
                due_note: item.get("dueNote").and_then(Json::as_str).map(str::to_owned),
                due_was: item.get("dueWas").and_then(Json::as_str).map(str::to_owned),
                done_at: item.get("doneAt").and_then(Json::as_str).map(str::to_owned),
                outcome: item.get("outcome").and_then(Json::as_str).map(str::to_owned),
                needs_you: item.get("needsYou").and_then(Json::as_str).map(str::to_owned),
                id: item.get("id")?.as_str()?.to_owned(),
                title: item.get("title")?.as_str()?.to_owned(),
                state: item.get("state")?.as_str()?.to_owned(),
                reviews,
                landed: item.get("landed").filter(|v| !v.is_null()).map(|landed| LandedIn {
                    at_ms: epoch(landed.get("at")),
                }),
                blocked_on: blocked_on(reads, &row.writer, item.get("blockedOn")),
                updated_at_ms: epoch(item.get("updatedAt")),
            });
        }
        tasks.push(TasksIn {
            additions: map(report_v2(doc)?)?,
            writer: row.writer.clone(),
            written_at_ms: row.written_at_ms,
            seq: doc.get("seq")?.as_u64()?,
            project: doc.get("project")?.as_str()?.to_owned(),
            stream: doc.get("stream")?.as_str()?.to_owned(),
            project_label: doc.get("projectLabel").and_then(Json::as_str).map(str::to_owned),
            stream_label: doc.get("streamLabel").and_then(Json::as_str).map(str::to_owned),
            problem: row.problem.as_ref().map(|p| p.reason.clone()),
            items,
        });
    }
    let mut open_cards = Vec::new();
    for row in &reports.cards {
        let doc = &row.doc;
        let id = doc.get("id").and_then(Json::as_str).unwrap_or("");
        let seq = doc.get("seq").cloned().unwrap_or(Json::Null);
        if doc.get("state").and_then(Json::as_str) != Some("open")
            || closed.contains(&card_version(id, &seq, &row.sha256))
        {
            continue;
        }
        open_cards.push(CardIn {
            writer: row.writer.clone(),
            id: id.to_owned(),
            seq: seq.as_u64()?,
            title: doc.get("title")?.as_str()?.to_owned(),
            kind: doc.get("kind").and_then(Json::as_str).unwrap_or("question").to_owned(),
            asked_at_ms: epoch(doc.get("askedAt")),
        });
    }
    let mut statuses = Vec::new();
    for row in &docs.docs {
        let doc = row.get("doc")?;
        let writer = row.get("writer")?.as_str()?.to_owned();
        let mut blockers = Vec::new();
        for blocker in array(doc.get("blockers")) {
            if blocker.get("waitingOn").and_then(Json::as_str) == Some("owner")
                && reads.answered(&writer, blocker.get("card"), blocker.get("text"))
            {
                continue;
            }
            let mut additions = BTreeMap::new();
            for (key, value) in blocker.as_object()? {
                if !["text", "waitingOn"].contains(&key.as_str()) {
                    additions.insert(key.clone(), value.clone());
                }
            }
            blockers.push(crate::pinned::BlockerIn {
                additions,
                text: blocker.get("text")?.as_str()?.to_owned(),
                waiting_on: blocker.get("waitingOn")?.as_str()?.to_owned(),
            });
        }
        let mut additions = map(report_v2(doc)?)?;
        additions.extend(crate::share::report::report_v3(doc.as_object()?).ok()?);
        statuses.push(StatusIn {
            additions,
            writer,
            written_at: doc.get("writtenAt")?.as_str()?.to_owned(),
            written_at_ms: row.get("writtenAtMs").and_then(Json::as_i64).unwrap_or(0),
            seq: row
                .get("seq")
                .and_then(Json::as_u64)
                .or_else(|| doc.get("seq").and_then(Json::as_u64))?,
            t3_thread: doc.get("t3Thread")?.as_str()?.to_owned(),
            summary: doc.get("summary")?.as_str()?.to_owned(),
            percent: doc.get("percent").and_then(Json::as_u64),
            done: items_lite(doc.get("done")),
            left: items_lite(doc.get("left")),
            blockers,
            looping: doc.get("looping").and_then(|looping| {
                Some(LoopingIn {
                    state: looping.get("state")?.as_str()?.to_owned(),
                    text: looping.get("text").and_then(Json::as_str).map(str::to_owned),
                })
            }),
            problem: row
                .get("problem")
                .and_then(|p| p.get("reason"))
                .and_then(Json::as_str)
                .map(str::to_owned),
            problem_since_ms: row.get("problem").and_then(|p| p.get("since")).and_then(Json::as_i64),
        });
    }
    let status_problems = docs
        .problems
        .iter()
        .map(|problem| StatusProblemIn {
            file: problem.file.clone(),
            reason: problem.reason.clone(),
            since_ms: i64::try_from(problem.since).unwrap_or(i64::MAX),
        })
        .collect();
    Some(ComposeInput {
        now_ms,
        pins: pins.iter().map(pin).collect(),
        machine_problems: machine_problems.to_vec(),
        threads,
        tasks,
        open_cards,
        statuses,
        status_problems,
    })
}

fn map(value: Json) -> Option<BTreeMap<String, Json>> {
    let object = value.as_object()?;
    Some(object.iter().map(|(key, value)| (key.clone(), value.clone())).collect())
}

fn progress(value: Option<&Json>) -> Option<Option<ProgressIn>> {
    let value = value?;
    if value.is_null() {
        return Some(None);
    }
    Some(Some(ProgressIn {
        done: count(value.get("done"))?,
        total: count(value.get("total"))?,
        unit: value.get("unit").and_then(Json::as_str).map(str::to_owned),
    }))
}

fn items_lite(value: Option<&Json>) -> Vec<ItemLiteIn> {
    array(value)
        .iter()
        .filter_map(|item| {
            Some(ItemLiteIn {
                id: item.get("id")?.as_str()?.to_owned(),
                title: item.get("title")?.as_str()?.to_owned(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn model_labels_come_from_the_packaged_catalog() {
        // The packaged file ships at least one model; its short form finds the label too.
        let (model, label) = packaged_models().first().expect("packaged models");
        assert_eq!(model_label_for(Some(model)).as_deref(), Some(label.as_str()));
        let short = model.rsplit('/').next().unwrap();
        assert_eq!(model_label_for(Some(short)).as_deref(), Some(label.as_str()));
        assert_eq!(model_label_for(Some("not/a-model")), None);
        assert_eq!(model_label_for(None), None);
    }

    #[test]
    fn the_v2_projection_keeps_only_valid_members() {
        let doc = json!({
            "title": "Build", "now": "Doing", "next": "Then", "readmeQuestions": true,
            "progress": {"done": 2, "total": 5, "unit": "items"},
            "unknown": "dropped",
        });
        let value = report_v2(&doc).unwrap();
        assert_eq!(value["title"], json!("Build"));
        assert_eq!(value["progress"]["done"], json!(2));
        assert!(value.get("unknown").is_none());
        // An empty title, a control character and a bad progress are refused.
        assert!(report_v2(&json!({"title": ""})).is_none());
        assert!(report_v2(&json!({"title": "a\u{0}b"})).is_none());
        assert!(report_v2(&json!({"progress": {"done": 1}})).is_none());
        assert!(report_v2(&json!({"stages": [{"label": "a", "state": "done"}]})).is_none());
    }
}
