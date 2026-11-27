//! Compact agent-reports reader (`v1/writers/<writer>/{thread.json,tasks.json,cards/*.json}`).
//!
//! Replaces retained `serde_json::Value` DOM trees (`Arc<Json>` in `SeenReport`) with typed
//! compact projections (`CompactThreadDoc`, `CompactTasksDoc`, `CompactCardDoc`), inline
//! `[u8; 32]` SHA-256 digests, numeric `StatStamp`, and pre-computed wire validity + content
//! hashes. Byte-for-byte parity with `aac_status::share::reports::AgentReportsReader`.

use super::primitives::{
    CompactProblem, CompactSha, CompactTransientMisses, StatStamp, proto_named_str, wire_order,
};
use aac_status::read_context::proto_named;
use aac_status::share::report::{CARD_CAP, TASKS_CAP, THREAD_CAP, parse_report_file};
use aac_status::share::reports::{AgentReportsState, Limits, SeenReport};
use aac_status::share::{
    MISSING_GRACE_MS, PERSISTENT_LISTING_ERROR, PERSISTENT_SHARE_ERROR, ReadError, SEQ_WENT_BACK,
    ShareFs, compare, folder_row_problem, is_missing, is_transient, js_text, share_local_path,
    showable,
};
use aac_status::validate::is_writer_name;
use indexmap::IndexMap;
use serde_json::{Map, Value as Json, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const REDATE_PASSES: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReportKind {
    Thread,
    Tasks,
    Card,
}

impl ReportKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Thread => "thread",
            Self::Tasks => "tasks",
            Self::Card => "card",
        }
    }

    fn cap(self) -> usize {
        match self {
            Self::Thread => THREAD_CAP,
            Self::Tasks => TASKS_CAP,
            Self::Card => CARD_CAP,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ThreadState {
    Working,
    Waiting,
    Blocked,
    Paused,
    Idle,
    Done,
    Other(Box<str>),
}

impl ThreadState {
    pub(crate) fn from_str(s: &str) -> Self {
        match s {
            "working" => Self::Working,
            "waiting" => Self::Waiting,
            "blocked" => Self::Blocked,
            "paused" => Self::Paused,
            "idle" => Self::Idle,
            "done" => Self::Done,
            other => Self::Other(other.into()),
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Blocked => "blocked",
            Self::Paused => "paused",
            Self::Idle => "idle",
            Self::Done => "done",
            Self::Other(s) => s,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TaskItemState {
    Todo,
    Working,
    Review,
    Fixing,
    Blocked,
    Ready,
    Landed,
    Done,
    Dropped,
    Other(Box<str>),
}

impl TaskItemState {
    pub(crate) fn from_str(s: &str) -> Self {
        match s {
            "todo" => Self::Todo,
            "working" => Self::Working,
            "review" => Self::Review,
            "fixing" => Self::Fixing,
            "blocked" => Self::Blocked,
            "ready" => Self::Ready,
            "landed" => Self::Landed,
            "done" => Self::Done,
            "dropped" => Self::Dropped,
            other => Self::Other(other.into()),
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Todo => "todo",
            Self::Working => "working",
            Self::Review => "review",
            Self::Fixing => "fixing",
            Self::Blocked => "blocked",
            Self::Ready => "ready",
            Self::Landed => "landed",
            Self::Done => "done",
            Self::Dropped => "dropped",
            Self::Other(s) => s,
        }
    }

    pub(crate) fn known_index(&self) -> Option<usize> {
        match self {
            Self::Todo => Some(0),
            Self::Working => Some(1),
            Self::Review => Some(2),
            Self::Fixing => Some(3),
            Self::Blocked => Some(4),
            Self::Ready => Some(5),
            Self::Landed => Some(6),
            Self::Done => Some(7),
            Self::Dropped => Some(8),
            Self::Other(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CardState {
    Open,
    Withdrawn,
    Superseded,
    Other(Box<str>),
}

impl CardState {
    pub(crate) fn from_str(s: &str) -> Self {
        match s {
            "open" => Self::Open,
            "withdrawn" => Self::Withdrawn,
            "superseded" => Self::Superseded,
            other => Self::Other(other.into()),
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Open => "open",
            Self::Withdrawn => "withdrawn",
            Self::Superseded => "superseded",
            Self::Other(s) => s,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompactAgent {
    pub(crate) name: Option<Box<str>>,
    pub(crate) harness: Option<Box<str>>,
    pub(crate) model: Option<Box<str>>,
    pub(crate) effort: Option<Box<str>>,
    pub(crate) account: Option<Box<str>>,
    pub(crate) machine: Option<Box<str>>,
    pub(crate) t3_thread: Option<Box<str>>,
    pub(crate) parent: Option<Box<str>>,
}

impl CompactAgent {
    fn from_json(value: Option<&Json>) -> (Self, bool) {
        let Some(obj) = value.and_then(Json::as_object) else {
            return (
                Self {
                    name: None,
                    harness: None,
                    model: None,
                    effort: None,
                    account: None,
                    machine: None,
                    t3_thread: None,
                    parent: None,
                },
                true,
            );
        };
        let get = |k: &str| obj.get(k).and_then(Json::as_str).map(Into::into);
        (
            Self {
                name: get("name"),
                harness: get("harness"),
                model: get("model"),
                effort: get("effort"),
                account: get("account"),
                machine: get("machine"),
                t3_thread: get("t3Thread"),
                parent: get("parent"),
            },
            false,
        )
    }

    pub(crate) fn to_json(&self) -> Json {
        json!({
            "name": self.name.as_deref(),
            "harness": self.harness.as_deref(),
            "model": self.model.as_deref(),
            "effort": self.effort.as_deref(),
            "account": self.account.as_deref(),
            "machine": self.machine.as_deref(),
            "t3Thread": self.t3_thread.as_deref(),
            "parent": self.parent.as_deref(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompactBlockedOn {
    pub(crate) kind: Box<str>,
    pub(crate) what: Box<str>,
    pub(crate) since: Option<Box<str>>,
    pub(crate) short: Option<Box<str>>,
    pub(crate) until: Option<Box<str>>,
    pub(crate) card: Option<Box<str>>,
}

impl CompactBlockedOn {
    fn from_json(value: Option<&Json>) -> Option<Box<Self>> {
        let obj = value?.as_object()?;
        Some(Box::new(Self {
            kind: obj
                .get("kind")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .into(),
            what: obj
                .get("what")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .into(),
            since: obj.get("since").and_then(Json::as_str).map(Into::into),
            short: obj.get("short").and_then(Json::as_str).map(Into::into),
            until: obj.get("until").and_then(Json::as_str).map(Into::into),
            card: obj.get("card").and_then(Json::as_str).map(Into::into),
        }))
    }

    pub(crate) fn to_json(&self) -> Json {
        json!({
            "kind": self.kind.as_ref(),
            "what": self.what.as_ref(),
            "since": self.since.as_deref(),
            "short": self.short.as_deref(),
            "until": self.until.as_deref(),
            "card": self.card.as_deref(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompactProgress {
    pub(crate) done: u64,
    pub(crate) total: u64,
    pub(crate) unit: Option<Box<str>>,
}

impl CompactProgress {
    fn from_json(value: Option<&Json>) -> Option<Box<Self>> {
        let obj = value?.as_object()?;
        Some(Box::new(Self {
            done: obj.get("done").and_then(Json::as_u64).unwrap_or(0),
            total: obj.get("total").and_then(Json::as_u64).unwrap_or(0),
            unit: obj.get("unit").and_then(Json::as_str).map(Into::into),
        }))
    }

    pub(crate) fn to_json(&self) -> Json {
        json!({
            "done": self.done,
            "total": self.total,
            "unit": self.unit.as_deref(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompactLink {
    pub(crate) label: Box<str>,
    pub(crate) path: Box<str>,
}

/// Compact typed projection of `agent-reports/thread@1`.
#[derive(Clone, Debug)]
pub(crate) struct CompactThreadDoc {
    pub(crate) writer: Box<str>,
    pub(crate) seq: u64,
    pub(crate) written_at: Box<str>,
    /// Compact JSON object of additive v2/v3 keys in original order (`None` when none present).
    pub(crate) v2_v3_json: Option<Box<[u8]>>,
    /// True when `v2` inserted `"progress"` so `to_json()` preserves its v2 key position.
    pub(crate) has_v2_progress: bool,
    pub(crate) ttl_seconds: Option<u64>,
    pub(crate) agent: CompactAgent,
    /// True only for synthetic test fixtures that omitted `"agent"` entirely.
    pub(crate) agent_absent: bool,
    pub(crate) project: Option<Box<str>>,
    pub(crate) stream: Option<Box<str>>,
    pub(crate) state: ThreadState,
    pub(crate) doing: Option<Box<str>>,
    pub(crate) blocked_on: Option<Box<CompactBlockedOn>>,
    pub(crate) progress: Option<Box<CompactProgress>>,
    pub(crate) links: Box<[CompactLink]>,
    /// Compact JSON of any non-standard keys present only in synthetic unit-test fixtures.
    pub(crate) extra_raw_json: Option<Box<[u8]>>,
}

const THREAD_V2_V3_KEYS: [&str; 12] = [
    "title",
    "now",
    "next",
    "readmeQuestions",
    "stages",
    "deliverables",
    "goal",
    "nextBy",
    "nextByDay",
    "nextByNote",
    "more",
    "saidAt",
];

const THREAD_STANDARD_KEYS: [&str; 25] = [
    "writer",
    "seq",
    "writtenAt",
    "title",
    "now",
    "next",
    "readmeQuestions",
    "progress",
    "stages",
    "deliverables",
    "goal",
    "nextBy",
    "nextByDay",
    "nextByNote",
    "more",
    "saidAt",
    "ttlSeconds",
    "agent",
    "project",
    "stream",
    "state",
    "doing",
    "blockedOn",
    "links",
    // progress counted above; 24 distinct keys, duplicate harmless
    "writer",
];

impl CompactThreadDoc {
    pub(crate) fn from_projected_json(doc: &Json) -> Self {
        let obj = doc.as_object();
        let get_str = |k: &str| obj.and_then(|m| m.get(k)).and_then(Json::as_str);
        let mut v2_v3 = Map::new();
        let mut has_v2_progress = false;
        let mut extra_raw = Map::new();
        if let Some(map) = obj {
            // Preserve exact insertion order of v2/v3 keys and check if `progress` appeared
            // before `ttlSeconds` (meaning v2 placed it there).
            let mut seen_ttl = false;
            for (k, v) in map {
                let ks = k.as_str();
                if ks == "ttlSeconds" {
                    seen_ttl = true;
                } else if ks == "progress" && !seen_ttl {
                    has_v2_progress = true;
                    v2_v3.insert(k.clone(), Json::Null);
                } else if THREAD_V2_V3_KEYS.contains(&ks) {
                    v2_v3.insert(k.clone(), v.clone());
                } else if !THREAD_STANDARD_KEYS.contains(&ks) {
                    extra_raw.insert(k.clone(), v.clone());
                }
            }
        }
        let v2_v3_json = if v2_v3.is_empty() {
            None
        } else {
            Some(
                js_text(&Json::Object(v2_v3))
                    .into_bytes()
                    .into_boxed_slice(),
            )
        };
        let extra_raw_json = if extra_raw.is_empty() {
            None
        } else {
            Some(js_text(doc).into_bytes().into_boxed_slice())
        };
        let (agent, agent_absent) = CompactAgent::from_json(obj.and_then(|m| m.get("agent")));
        let links = obj
            .and_then(|m| m.get("links"))
            .and_then(Json::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| CompactLink {
                        label: item["label"].as_str().unwrap_or_default().into(),
                        path: item["path"].as_str().unwrap_or_default().into(),
                    })
                    .collect::<Vec<_>>()
                    .into_boxed_slice()
            })
            .unwrap_or_else(|| Box::new([]));
        Self {
            writer: get_str("writer").unwrap_or_default().into(),
            seq: obj
                .and_then(|m| m.get("seq"))
                .and_then(Json::as_u64)
                .unwrap_or(0),
            written_at: get_str("writtenAt").unwrap_or_default().into(),
            v2_v3_json,
            has_v2_progress,
            ttl_seconds: obj.and_then(|m| m.get("ttlSeconds")).and_then(Json::as_u64),
            agent,
            agent_absent,
            project: get_str("project").map(Into::into),
            stream: get_str("stream").map(Into::into),
            state: ThreadState::from_str(get_str("state").unwrap_or_default()),
            doing: get_str("doing").map(Into::into),
            blocked_on: CompactBlockedOn::from_json(obj.and_then(|m| m.get("blockedOn"))),
            progress: CompactProgress::from_json(obj.and_then(|m| m.get("progress"))),
            links,
            extra_raw_json,
        }
    }

    pub(crate) fn to_json(&self) -> Json {
        if let Some(raw) = &self.extra_raw_json
            && let Ok(val) = serde_json::from_slice::<Json>(raw)
        {
            return val;
        }
        let mut out = Map::new();
        out.insert("writer".to_owned(), Json::from(self.writer.as_ref()));
        out.insert("seq".to_owned(), Json::from(self.seq));
        out.insert("writtenAt".to_owned(), Json::from(self.written_at.as_ref()));
        let progress_val = self
            .progress
            .as_deref()
            .map(CompactProgress::to_json)
            .unwrap_or(Json::Null);
        if let Some(bytes) = &self.v2_v3_json
            && let Ok(Json::Object(v2_v3)) = serde_json::from_slice::<Json>(bytes)
        {
            for (k, v) in v2_v3 {
                if k == "progress" {
                    out.insert(k, progress_val.clone());
                } else {
                    out.insert(k, v);
                }
            }
        }
        out.insert(
            "ttlSeconds".to_owned(),
            self.ttl_seconds.map(Json::from).unwrap_or(Json::Null),
        );
        if !self.agent_absent {
            out.insert("agent".to_owned(), self.agent.to_json());
        }
        out.insert(
            "project".to_owned(),
            self.project
                .as_deref()
                .map(Json::from)
                .unwrap_or(Json::Null),
        );
        out.insert(
            "stream".to_owned(),
            self.stream.as_deref().map(Json::from).unwrap_or(Json::Null),
        );
        out.insert("state".to_owned(), Json::from(self.state.as_str()));
        out.insert(
            "doing".to_owned(),
            self.doing.as_deref().map(Json::from).unwrap_or(Json::Null),
        );
        out.insert(
            "blockedOn".to_owned(),
            self.blocked_on
                .as_deref()
                .map(CompactBlockedOn::to_json)
                .unwrap_or(Json::Null),
        );
        if !self.has_v2_progress {
            out.insert("progress".to_owned(), progress_val);
        }
        out.insert(
            "links".to_owned(),
            Json::Array(
                self.links
                    .iter()
                    .map(|l| json!({"label": l.label.as_ref(), "path": l.path.as_ref()}))
                    .collect(),
            ),
        );
        Json::Object(out)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompactReview {
    pub(crate) by: Box<str>,
    pub(crate) verdict: Box<str>,
    pub(crate) at: Option<Box<str>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompactLanded {
    pub(crate) sha: Box<str>,
    pub(crate) at: Option<Box<str>>,
}

/// Optional rarely-present task item fields boxed behind one pointer so the common
/// task item pays only 8 bytes instead of 160+.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompactTaskItemExtras {
    /// Each `Option<Option<Box<str>>>`: `None` = key omitted, `Some(None)` = `null`, `Some(Some(s))` = string.
    pub(crate) due: Option<Option<Box<str>>>,
    pub(crate) due_day: Option<Option<Box<str>>>,
    pub(crate) due_note: Option<Option<Box<str>>>,
    pub(crate) due_was: Option<Option<Box<str>>>,
    pub(crate) done_at: Option<Option<Box<str>>>,
    pub(crate) outcome: Option<Option<Box<str>>>,
    pub(crate) blocked_on: Option<Box<CompactBlockedOn>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompactTaskItem {
    pub(crate) id: Box<str>,
    pub(crate) title: Box<str>,
    pub(crate) state: TaskItemState,
    pub(crate) owner: Option<Box<str>>,
    pub(crate) branch: Option<Box<str>>,
    pub(crate) head: Option<Box<str>>,
    pub(crate) reviews: Box<[CompactReview]>,
    pub(crate) landed: Option<Box<CompactLanded>>,
    pub(crate) needs_you: Option<Option<Box<str>>>,
    pub(crate) extras: Option<Box<CompactTaskItemExtras>>,
    pub(crate) note: Option<Box<str>>,
    pub(crate) why: Option<Box<str>>,
    pub(crate) updated_at: Option<Box<str>>,
    pub(crate) updated_at_ms: Option<i64>,
}

impl CompactTaskItem {
    fn from_json(value: &Json) -> Self {
        let obj = value.as_object();
        let get_str = |k: &str| obj.and_then(|m| m.get(k)).and_then(Json::as_str);
        let opt_field = |k: &str| -> Option<Option<Box<str>>> {
            obj.and_then(|m| m.get(k))
                .map(|v| v.as_str().map(Into::into))
        };
        let reviews = obj
            .and_then(|m| m.get("reviews"))
            .and_then(Json::as_array)
            .map(|list| {
                list.iter()
                    .map(|r| CompactReview {
                        by: r["by"].as_str().unwrap_or_default().into(),
                        verdict: r["verdict"].as_str().unwrap_or_default().into(),
                        at: r["at"].as_str().map(Into::into),
                    })
                    .collect::<Vec<_>>()
                    .into_boxed_slice()
            })
            .unwrap_or_else(|| Box::new([]));
        let landed = obj
            .and_then(|m| m.get("landed"))
            .and_then(Json::as_object)
            .map(|l| {
                Box::new(CompactLanded {
                    sha: l
                        .get("sha")
                        .and_then(Json::as_str)
                        .unwrap_or_default()
                        .into(),
                    at: l.get("at").and_then(Json::as_str).map(Into::into),
                })
            });
        let due = opt_field("due");
        let due_day = opt_field("dueDay");
        let due_note = opt_field("dueNote");
        let due_was = opt_field("dueWas");
        let done_at = opt_field("doneAt");
        let outcome = opt_field("outcome");
        let blocked_on = CompactBlockedOn::from_json(obj.and_then(|m| m.get("blockedOn")));
        let extras = if due.is_some()
            || due_day.is_some()
            || due_note.is_some()
            || due_was.is_some()
            || done_at.is_some()
            || outcome.is_some()
            || blocked_on.is_some()
        {
            Some(Box::new(CompactTaskItemExtras {
                due,
                due_day,
                due_note,
                due_was,
                done_at,
                outcome,
                blocked_on,
            }))
        } else {
            None
        };
        let updated_at = get_str("updatedAt");
        let updated_at_ms = updated_at.and_then(aac_num::ecma::parse_instant);
        Self {
            id: get_str("id").unwrap_or_default().into(),
            title: get_str("title").unwrap_or_default().into(),
            state: TaskItemState::from_str(get_str("state").unwrap_or_default()),
            owner: get_str("owner").map(Into::into),
            branch: get_str("branch").map(Into::into),
            head: get_str("head").map(Into::into),
            reviews,
            landed,
            needs_you: opt_field("needsYou"),
            extras,
            note: get_str("note").map(Into::into),
            why: get_str("why").map(Into::into),
            updated_at: updated_at.map(Into::into),
            updated_at_ms,
        }
    }

    pub(crate) fn to_json(&self) -> Json {
        let mut out = Map::new();
        out.insert("id".to_owned(), Json::from(self.id.as_ref()));
        out.insert("title".to_owned(), Json::from(self.title.as_ref()));
        out.insert("state".to_owned(), Json::from(self.state.as_str()));
        out.insert(
            "owner".to_owned(),
            self.owner.as_deref().map(Json::from).unwrap_or(Json::Null),
        );
        out.insert(
            "branch".to_owned(),
            self.branch.as_deref().map(Json::from).unwrap_or(Json::Null),
        );
        out.insert(
            "head".to_owned(),
            self.head.as_deref().map(Json::from).unwrap_or(Json::Null),
        );
        out.insert(
            "reviews".to_owned(),
            Json::Array(
                self.reviews
                    .iter()
                    .map(|r| {
                        json!({
                            "by": r.by.as_ref(),
                            "verdict": r.verdict.as_ref(),
                            "at": r.at.as_deref(),
                        })
                    })
                    .collect(),
            ),
        );
        out.insert(
            "landed".to_owned(),
            match self.landed.as_deref() {
                None => Json::Null,
                Some(l) => json!({"sha": l.sha.as_ref(), "at": l.at.as_deref()}),
            },
        );
        if let Some(needs_you) = &self.needs_you {
            out.insert(
                "needsYou".to_owned(),
                needs_you.as_deref().map(Json::from).unwrap_or(Json::Null),
            );
        }
        if let Some(ex) = self.extras.as_deref() {
            for (k, val) in [
                ("due", &ex.due),
                ("dueDay", &ex.due_day),
                ("dueNote", &ex.due_note),
                ("dueWas", &ex.due_was),
                ("doneAt", &ex.done_at),
                ("outcome", &ex.outcome),
            ] {
                if let Some(v) = val {
                    out.insert(
                        k.to_owned(),
                        v.as_deref().map(Json::from).unwrap_or(Json::Null),
                    );
                }
            }
        }
        out.insert(
            "blockedOn".to_owned(),
            self.extras
                .as_deref()
                .and_then(|ex| ex.blocked_on.as_deref())
                .map(CompactBlockedOn::to_json)
                .unwrap_or(Json::Null),
        );
        out.insert(
            "note".to_owned(),
            self.note.as_deref().map(Json::from).unwrap_or(Json::Null),
        );
        out.insert(
            "why".to_owned(),
            self.why.as_deref().map(Json::from).unwrap_or(Json::Null),
        );
        out.insert(
            "updatedAt".to_owned(),
            self.updated_at
                .as_deref()
                .map(Json::from)
                .unwrap_or(Json::Null),
        );
        Json::Object(out)
    }
}

/// Compact typed projection of `agent-reports/tasks@1`.
#[derive(Clone, Debug)]
pub(crate) struct CompactTasksDoc {
    pub(crate) writer: Box<str>,
    pub(crate) seq: u64,
    pub(crate) written_at: Box<str>,
    /// Compact JSON object of additive v2 keys (`None` when absent).
    pub(crate) v2_json: Option<Box<[u8]>>,
    pub(crate) project: Box<str>,
    pub(crate) project_label: Option<Box<str>>,
    pub(crate) stream: Box<str>,
    pub(crate) stream_label: Option<Box<str>>,
    pub(crate) owner: Option<Box<str>>,
    pub(crate) goal: Option<Box<str>>,
    pub(crate) board: Option<Box<str>>,
    pub(crate) items: Box<[CompactTaskItem]>,
}

const TASKS_V2_KEYS: [&str; 7] = [
    "title",
    "now",
    "next",
    "readmeQuestions",
    "progress",
    "stages",
    "deliverables",
];

impl CompactTasksDoc {
    pub(crate) fn from_projected_json(doc: &Json) -> Self {
        let obj = doc.as_object();
        let get_str = |k: &str| obj.and_then(|m| m.get(k)).and_then(Json::as_str);
        let mut v2 = Map::new();
        if let Some(map) = obj {
            for (k, v) in map {
                if TASKS_V2_KEYS.contains(&k.as_str()) {
                    v2.insert(k.clone(), v.clone());
                }
            }
        }
        let v2_json = if v2.is_empty() {
            None
        } else {
            Some(js_text(&Json::Object(v2)).into_bytes().into_boxed_slice())
        };
        let items = obj
            .and_then(|m| m.get("items"))
            .and_then(Json::as_array)
            .map(|list| {
                list.iter()
                    .map(CompactTaskItem::from_json)
                    .collect::<Vec<_>>()
                    .into_boxed_slice()
            })
            .unwrap_or_else(|| Box::new([]));
        Self {
            writer: get_str("writer").unwrap_or_default().into(),
            seq: obj
                .and_then(|m| m.get("seq"))
                .and_then(Json::as_u64)
                .unwrap_or(0),
            written_at: get_str("writtenAt").unwrap_or_default().into(),
            v2_json,
            project: get_str("project").unwrap_or_default().into(),
            project_label: get_str("projectLabel").map(Into::into),
            stream: get_str("stream").unwrap_or_default().into(),
            stream_label: get_str("streamLabel").map(Into::into),
            owner: get_str("owner").map(Into::into),
            goal: get_str("goal").map(Into::into),
            board: get_str("board").map(Into::into),
            items,
        }
    }

    pub(crate) fn to_json(&self) -> Json {
        let mut out = Map::new();
        out.insert("writer".to_owned(), Json::from(self.writer.as_ref()));
        out.insert("seq".to_owned(), Json::from(self.seq));
        out.insert("writtenAt".to_owned(), Json::from(self.written_at.as_ref()));
        if let Some(bytes) = &self.v2_json
            && let Ok(Json::Object(v2)) = serde_json::from_slice::<Json>(bytes)
        {
            for (k, v) in v2 {
                out.insert(k, v);
            }
        }
        out.insert("project".to_owned(), Json::from(self.project.as_ref()));
        out.insert(
            "projectLabel".to_owned(),
            self.project_label
                .as_deref()
                .map(Json::from)
                .unwrap_or(Json::Null),
        );
        out.insert("stream".to_owned(), Json::from(self.stream.as_ref()));
        out.insert(
            "streamLabel".to_owned(),
            self.stream_label
                .as_deref()
                .map(Json::from)
                .unwrap_or(Json::Null),
        );
        out.insert(
            "owner".to_owned(),
            self.owner.as_deref().map(Json::from).unwrap_or(Json::Null),
        );
        out.insert(
            "goal".to_owned(),
            self.goal.as_deref().map(Json::from).unwrap_or(Json::Null),
        );
        out.insert(
            "board".to_owned(),
            self.board.as_deref().map(Json::from).unwrap_or(Json::Null),
        );
        out.insert(
            "items".to_owned(),
            Json::Array(self.items.iter().map(CompactTaskItem::to_json).collect()),
        );
        Json::Object(out)
    }
}

/// Compact projection of `agent-reports/card@1`: hot typed fields for filtering/check/answers
/// plus compact `js_text` UTF-8 bytes (never a retained `serde_json::Value` tree).
#[derive(Clone, Debug)]
pub(crate) struct CompactCardDoc {
    pub(crate) id: Box<str>,
    pub(crate) label: Option<Box<str>>,
    pub(crate) seq: u64,
    pub(crate) state: CardState,
    pub(crate) kind: Box<str>,
    /// Compact `js_text` UTF-8 bytes of the projected card document.
    pub(crate) doc_json: Box<[u8]>,
}

impl CompactCardDoc {
    pub(crate) fn from_projected_json(doc: &Json) -> Self {
        let obj = doc.as_object();
        let get_str = |k: &str| obj.and_then(|m| m.get(k)).and_then(Json::as_str);
        Self {
            id: get_str("id").unwrap_or_default().into(),
            label: get_str("label").map(Into::into),
            seq: obj
                .and_then(|m| m.get("seq"))
                .and_then(Json::as_u64)
                .unwrap_or(0),
            state: CardState::from_str(get_str("state").unwrap_or_default()),
            kind: get_str("kind").unwrap_or_default().into(),
            doc_json: js_text(doc).into_bytes().into_boxed_slice(),
        }
    }

    pub(crate) fn to_json(&self) -> Json {
        serde_json::from_slice(&self.doc_json).unwrap_or(Json::Null)
    }
}

/// Compact equivalent of `aac_status::produce::answered::ResolvedWaits` over `CompactSeenReport`
/// cards without deserializing closed card documents.
pub(crate) struct CompactResolvedWaits {
    writers: HashMap<Box<str>, HashMap<Box<str>, bool>>,
}

impl CompactResolvedWaits {
    pub(crate) fn answered(&self, writer: &str, card: Option<&str>, words: Option<&str>) -> bool {
        let named = card.map(str::trim).unwrap_or("");
        let prefix = words.and_then(|w| w.strip_prefix("Card ")?.split_once(':').map(|(n, _)| n));
        let reference = if named.is_empty() {
            prefix.unwrap_or("")
        } else {
            named
        };
        !reference.is_empty()
            && self
                .writers
                .get(writer)
                .and_then(|refs| refs.get(reference))
                .copied()
                == Some(true)
    }
}

/// Mirrors `aac_status::produce::answered::resolved_card_waits` directly on compact cards.
pub(crate) fn compact_resolved_card_waits(
    cards: &[Arc<CompactSeenReport>],
    closed: &HashSet<String>,
) -> CompactResolvedWaits {
    let mut writers: HashMap<Box<str>, HashMap<Box<str>, bool>> = HashMap::new();
    for row in cards {
        let Some(card) = row.card() else { continue };
        let id = card.id.as_ref();
        let resolved = card.state != CardState::Open
            || closed.contains(&aac_status::answers_engine::card_version(
                id,
                &Json::from(card.seq),
                &row.sha256.to_hex_string(),
            ));
        let label = card.label.as_deref();
        let refs = writers.entry(row.writer.clone()).or_default();
        for reference in [
            Some(id),
            id.find("--")
                .map(|at| &id[at + 2..])
                .or_else(|| Some(&id[1.min(id.len())..])),
            label,
        ] {
            let Some(reference) = reference.filter(|r| !r.is_empty()) else {
                continue;
            };
            let held = refs.get(reference).copied();
            refs.insert(reference.into(), resolved && held != Some(false));
        }
    }
    CompactResolvedWaits { writers }
}

/// True when a thread's `blockedOn` is an owner wait explained by a resolved card
/// (matches `aac_status::read_status_reports::ReportView::new` cleared-writer computation).
pub(crate) fn compact_cleared_writers(
    threads: &[Arc<CompactSeenReport>],
    cards: &[Arc<CompactSeenReport>],
    closed: &HashSet<String>,
) -> HashSet<Box<str>> {
    let waits = compact_resolved_card_waits(cards, closed);
    threads
        .iter()
        .filter(|row| {
            let Some(thread) = row.thread() else {
                return false;
            };
            let Some(blocker) = thread.blocked_on.as_deref() else {
                return false;
            };
            blocker.kind.as_ref() == "owner"
                && waits.answered(&row.writer, blocker.card.as_deref(), Some(&blocker.what))
        })
        .map(|row| row.writer.clone())
        .collect()
}

#[derive(Clone, Debug)]
pub(crate) enum CompactReportDoc {
    Thread(Box<CompactThreadDoc>),
    Tasks(Box<CompactTasksDoc>),
    Card(Box<CompactCardDoc>),
}

impl CompactReportDoc {
    pub(crate) fn seq(&self) -> u64 {
        match self {
            Self::Thread(t) => t.seq,
            Self::Tasks(t) => t.seq,
            Self::Card(c) => c.seq,
        }
    }

    pub(crate) fn to_json(&self) -> Json {
        match self {
            Self::Thread(t) => t.to_json(),
            Self::Tasks(t) => t.to_json(),
            Self::Card(c) => c.to_json(),
        }
    }
}

/// One accepted report revision in compact typed form, shared (`Arc`) between reader and snapshot.
#[derive(Clone, Debug)]
pub(crate) struct CompactSeenReport {
    pub(crate) file: Box<str>,
    pub(crate) writer: Box<str>,
    pub(crate) doc: CompactReportDoc,
    pub(crate) sha256: CompactSha,
    pub(crate) first_seen_at: u64,
    pub(crate) written_at_ms: i64,
    pub(crate) problem: Option<Box<CompactProblem>>,
    /// Pre-checked once at parse time: `wire_order(&doc) && !proto_named(&doc)` and safe metadata.
    pub(crate) doc_valid_wire: bool,
    /// SHA-256 of the projected doc's canonical `js_text` bytes (avoids serializing all docs on publish).
    pub(crate) doc_hash: [u8; 32],
}

impl CompactSeenReport {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_projected(
        kind: ReportKind,
        file: &str,
        writer: &str,
        doc: &Json,
        sha256: CompactSha,
        first_seen_at: u64,
        written_at_ms: i64,
        problem: Option<CompactProblem>,
    ) -> Self {
        let doc_valid_wire = wire_order(doc)
            && !proto_named(doc)
            && !proto_named_str(file)
            && !proto_named_str(writer)
            && !sha256.proto_unsafe();
        let doc_hash: [u8; 32] = Sha256::digest(js_text(doc).as_bytes()).into();
        let compact_doc = match kind {
            ReportKind::Thread => {
                CompactReportDoc::Thread(Box::new(CompactThreadDoc::from_projected_json(doc)))
            }
            ReportKind::Tasks => {
                CompactReportDoc::Tasks(Box::new(CompactTasksDoc::from_projected_json(doc)))
            }
            ReportKind::Card => {
                CompactReportDoc::Card(Box::new(CompactCardDoc::from_projected_json(doc)))
            }
        };
        Self {
            file: file.into(),
            writer: writer.into(),
            doc: compact_doc,
            sha256,
            first_seen_at,
            written_at_ms,
            problem: problem.map(Box::new),
            doc_valid_wire,
            doc_hash,
        }
    }

    pub(crate) fn seq(&self) -> u64 {
        self.doc.seq()
    }

    pub(crate) fn thread(&self) -> Option<&CompactThreadDoc> {
        match &self.doc {
            CompactReportDoc::Thread(t) => Some(t),
            _ => None,
        }
    }

    pub(crate) fn tasks(&self) -> Option<&CompactTasksDoc> {
        match &self.doc {
            CompactReportDoc::Tasks(t) => Some(t),
            _ => None,
        }
    }

    pub(crate) fn card(&self) -> Option<&CompactCardDoc> {
        match &self.doc {
            CompactReportDoc::Card(c) => Some(c),
            _ => None,
        }
    }

    pub(crate) fn thread_t3_thread(&self) -> Option<&str> {
        self.thread()?.agent.t3_thread.as_deref()
    }

    pub(crate) fn card_id(&self) -> Option<&str> {
        Some(self.card()?.id.as_ref())
    }

    pub(crate) fn is_valid_wire(&self) -> bool {
        self.doc_valid_wire && self.problem.as_deref().is_none_or(|p| !p.proto_unsafe())
    }

    pub(crate) fn to_seen_report(&self) -> SeenReport {
        SeenReport {
            file: self.file.to_string(),
            writer: self.writer.to_string(),
            doc: Arc::new(self.doc.to_json()),
            sha256: self.sha256.to_hex_string(),
            first_seen_at: self.first_seen_at,
            written_at_ms: self.written_at_ms,
            problem: self
                .problem
                .as_deref()
                .map(CompactProblem::to_share_problem),
        }
    }

    pub(crate) fn to_row_json(&self) -> Json {
        let mut out = Map::new();
        out.insert("file".to_owned(), Json::from(self.file.as_ref()));
        out.insert("writer".to_owned(), Json::from(self.writer.as_ref()));
        out.insert("doc".to_owned(), self.doc.to_json());
        out.insert("sha256".to_owned(), Json::from(self.sha256.to_hex_string()));
        out.insert("firstSeenAt".to_owned(), Json::from(self.first_seen_at));
        out.insert("writtenAtMs".to_owned(), Json::from(self.written_at_ms));
        out.insert(
            "problem".to_owned(),
            self.problem
                .as_deref()
                .map(CompactProblem::to_json)
                .unwrap_or(Json::Null),
        );
        Json::Object(out)
    }
}

/// Compact snapshot of `AgentReportsReader`.
#[derive(Clone, Debug)]
pub(crate) struct CompactAgentReportsState {
    pub(crate) root: Box<str>,
    pub(crate) state: &'static str,
    pub(crate) scanned_at: Option<u64>,
    pub(crate) complete: bool,
    pub(crate) threads: Box<[Arc<CompactSeenReport>]>,
    pub(crate) tasks: Box<[Arc<CompactSeenReport>]>,
    pub(crate) cards: Box<[Arc<CompactSeenReport>]>,
    pub(crate) problems: Box<[CompactProblem]>,
    /// Board paths in exact scan order (`(given_path, mtime_ms)`).
    pub(crate) boards: Box<[(Box<str>, Option<f64>)]>,
}

impl CompactAgentReportsState {
    pub(crate) fn empty() -> Self {
        Self {
            root: "".into(),
            state: "missing",
            scanned_at: None,
            complete: false,
            threads: Box::new([]),
            tasks: Box::new([]),
            cards: Box::new([]),
            problems: Box::new([]),
            boards: Box::new([]),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_agent_reports_state(state: &AgentReportsState) -> Self {
        let convert =
            |rows: &[Arc<SeenReport>], kind: ReportKind| -> Box<[Arc<CompactSeenReport>]> {
                rows.iter()
                    .map(|row| {
                        Arc::new(CompactSeenReport::from_projected(
                            kind,
                            &row.file,
                            &row.writer,
                            &row.doc,
                            CompactSha::from_hex_or_str(&row.sha256),
                            row.first_seen_at,
                            row.written_at_ms,
                            row.problem
                                .as_ref()
                                .map(|p| CompactProblem::new(&p.file, &p.reason, p.since)),
                        ))
                    })
                    .collect::<Vec<_>>()
                    .into_boxed_slice()
            };
        let problems = state
            .problems
            .iter()
            .map(|p| CompactProblem::new(&p.file, &p.reason, p.since))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let boards = state
            .boards
            .iter()
            .map(|(k, v)| (k.as_str().into(), v.as_f64()))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            root: state.root.as_str().into(),
            state: state.state,
            scanned_at: state.scanned_at,
            complete: state.complete,
            threads: convert(&state.threads, ReportKind::Thread),
            tasks: convert(&state.tasks, ReportKind::Tasks),
            cards: convert(&state.cards, ReportKind::Card),
            problems,
            boards,
        }
    }

    pub(crate) fn board_mtime(&self, given: &str) -> Option<f64> {
        self.boards
            .iter()
            .find(|(path, _)| path.as_ref() == given)
            .and_then(|(_, mtime)| *mtime)
    }

    pub(crate) fn is_valid_wire(&self) -> bool {
        if proto_named_str(&self.root) {
            return false;
        }
        if self.problems.iter().any(CompactProblem::proto_unsafe) {
            return false;
        }
        // Check board keys against wire_order and proto_named.
        let mut last_idx: Option<u32> = None;
        let mut saw_name = false;
        for (key, _) in &self.boards {
            if proto_named_str(key) {
                return false;
            }
            let index = key
                .as_bytes()
                .first()
                .filter(|c| c.is_ascii_digit())
                .and_then(|_| key.parse::<u32>().ok())
                .filter(|i| *i != u32::MAX && i.to_string() == key.as_ref());
            match index {
                Some(index) if !saw_name && last_idx.is_none_or(|old| old < index) => {
                    last_idx = Some(index);
                }
                Some(_) => return false,
                None => saw_name = true,
            }
        }
        self.threads.iter().all(|r| r.is_valid_wire())
            && self.tasks.iter().all(|r| r.is_valid_wire())
            && self.cards.iter().all(|r| r.is_valid_wire())
    }

    /// Fast deterministic content hash (matches every observable field, including cleared blockers)
    /// so `Produce::publish` can detect unchanged report generations without serializing multi-MB JSON.
    pub(crate) fn content_hash(&self, cleared: &HashSet<Box<str>>) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"compact-reports-v1:");
        h.update((self.root.len() as u64).to_le_bytes());
        h.update(self.root.as_bytes());
        h.update((self.state.len() as u64).to_le_bytes());
        h.update(self.state.as_bytes());
        match self.scanned_at {
            Some(at) => {
                h.update([1u8]);
                h.update(at.to_le_bytes());
            }
            None => h.update([0u8]),
        }
        h.update([u8::from(self.complete)]);
        let mut hash_rows = |rows: &[Arc<CompactSeenReport>], check_cleared: bool| {
            h.update((rows.len() as u64).to_le_bytes());
            for row in rows {
                h.update((row.file.len() as u64).to_le_bytes());
                h.update(row.file.as_bytes());
                h.update((row.writer.len() as u64).to_le_bytes());
                h.update(row.writer.as_bytes());
                h.update(row.doc_hash);
                row.sha256.update_hasher(&mut h);
                h.update(row.first_seen_at.to_le_bytes());
                h.update(row.written_at_ms.to_le_bytes());
                h.update([u8::from(
                    check_cleared && cleared.contains(row.writer.as_ref()),
                )]);
                match row.problem.as_deref() {
                    Some(p) => {
                        h.update([1u8]);
                        h.update((p.file.len() as u64).to_le_bytes());
                        h.update(p.file.as_bytes());
                        h.update((p.reason.len() as u64).to_le_bytes());
                        h.update(p.reason.as_bytes());
                        h.update(p.since.to_le_bytes());
                    }
                    None => h.update([0u8]),
                }
            }
        };
        hash_rows(&self.threads, true);
        hash_rows(&self.tasks, false);
        hash_rows(&self.cards, false);
        h.update((self.problems.len() as u64).to_le_bytes());
        for p in &self.problems {
            h.update((p.file.len() as u64).to_le_bytes());
            h.update(p.file.as_bytes());
            h.update((p.reason.len() as u64).to_le_bytes());
            h.update(p.reason.as_bytes());
            h.update(p.since.to_le_bytes());
        }
        h.update((self.boards.len() as u64).to_le_bytes());
        for (path, mtime) in &self.boards {
            h.update((path.len() as u64).to_le_bytes());
            h.update(path.as_bytes());
            match mtime {
                Some(v) => {
                    h.update([1u8]);
                    h.update(v.to_bits().to_le_bytes());
                }
                None => h.update([0u8]),
            }
        }
        h.finalize().into()
    }

    /// Materialize the exact JSON snapshot for legacy `/internal/status/inputs` or parity tests.
    pub(crate) fn state_json(&self, scanned_at_override: Option<u64>) -> Json {
        let mut out = Map::new();
        out.insert("root".to_owned(), Json::from(self.root.as_ref()));
        out.insert("state".to_owned(), Json::from(self.state));
        let scanned_at = scanned_at_override.or(self.scanned_at);
        out.insert(
            "scannedAt".to_owned(),
            scanned_at.map(Json::from).unwrap_or(Json::Null),
        );
        out.insert("complete".to_owned(), Json::from(self.complete));
        out.insert(
            "threads".to_owned(),
            Json::Array(self.threads.iter().map(|r| r.to_row_json()).collect()),
        );
        out.insert(
            "tasks".to_owned(),
            Json::Array(self.tasks.iter().map(|r| r.to_row_json()).collect()),
        );
        out.insert(
            "cards".to_owned(),
            Json::Array(self.cards.iter().map(|r| r.to_row_json()).collect()),
        );
        out.insert(
            "problems".to_owned(),
            Json::Array(self.problems.iter().map(CompactProblem::to_json).collect()),
        );
        let mut boards = Map::new();
        for (k, v) in &self.boards {
            boards.insert(k.to_string(), v.map(Json::from).unwrap_or(Json::Null));
        }
        out.insert("boards".to_owned(), Json::Object(boards));
        Json::Object(out)
    }

    /// Materialize transient `AgentReportsState` ONLY when `pinned_panel` runs (never retained).
    pub(crate) fn to_agent_reports_state(&self) -> AgentReportsState {
        let mut boards = Map::new();
        for (k, v) in &self.boards {
            boards.insert(k.to_string(), v.map(Json::from).unwrap_or(Json::Null));
        }
        AgentReportsState {
            root: self.root.to_string(),
            state: self.state,
            scanned_at: self.scanned_at,
            complete: self.complete,
            threads: self
                .threads
                .iter()
                .map(|r| Arc::new(r.to_seen_report()))
                .collect(),
            tasks: self
                .tasks
                .iter()
                .map(|r| Arc::new(r.to_seen_report()))
                .collect(),
            cards: self
                .cards
                .iter()
                .map(|r| Arc::new(r.to_seen_report()))
                .collect(),
            problems: self
                .problems
                .iter()
                .map(CompactProblem::to_share_problem)
                .collect(),
            boards,
        }
    }
}

#[derive(Clone)]
struct CompactEntry {
    kind: ReportKind,
    writer: Box<str>,
    stamp: Option<StatStamp>,
    good: Option<Arc<CompactSeenReport>>,
    problem: Option<Box<CompactProblem>>,
    missing_since: Option<u64>,
}

pub(crate) struct CompactAgentReportsReader {
    root: PathBuf,
    limits: Limits,
    entries: IndexMap<Box<str>, CompactEntry>,
    folder_problems: IndexMap<Box<str>, CompactProblem>,
    boards: IndexMap<Box<str>, Option<f64>>,
    board_paths: Vec<(Box<str>, Option<PathBuf>)>,
    state: &'static str,
    scanned_at: Option<u64>,
    complete: bool,
    scan_partial: bool,
    scan_bytes: u64,
    over_bytes: u64,
    misses: CompactTransientMisses,
    listing_misses: CompactTransientMisses,
    folder_dates: HashMap<Box<str>, f64>,
    kept_writers: HashSet<Box<str>>,
    date_cursor: usize,
}

impl CompactAgentReportsReader {
    pub(crate) fn new(root: PathBuf, limits: Limits) -> Self {
        Self {
            root,
            limits,
            entries: IndexMap::new(),
            folder_problems: IndexMap::new(),
            boards: IndexMap::new(),
            board_paths: Vec::new(),
            state: "missing",
            scanned_at: None,
            complete: false,
            scan_partial: false,
            scan_bytes: 0,
            over_bytes: 0,
            misses: CompactTransientMisses::default(),
            listing_misses: CompactTransientMisses::default(),
            folder_dates: HashMap::new(),
            kept_writers: HashSet::new(),
            date_cursor: 0,
        }
    }

    pub(crate) fn scanned_at(&self) -> Option<u64> {
        self.scanned_at
    }

    pub(crate) fn complete(&self) -> bool {
        self.complete
    }

    fn select_writers(&mut self, names: Vec<String>, fs: &dyn ShareFs, dir: &Path) -> Vec<String> {
        let listed: HashSet<&str> = names.iter().map(String::as_str).collect();
        self.folder_dates
            .retain(|name, _| listed.contains(name.as_ref()));
        self.kept_writers
            .retain(|name| listed.contains(name.as_ref()));
        if names.len() <= self.limits.writers {
            self.kept_writers = names.iter().map(|n| n.as_str().into()).collect();
            return names;
        }
        let mut fresh: HashSet<&str> = HashSet::new();
        for name in &names {
            if !self.folder_dates.contains_key(name.as_str()) {
                let mtime = fs
                    .stat(&dir.join(name))
                    .map(|stat| stat.mtime_ms)
                    .unwrap_or(0.0);
                self.folder_dates.insert(name.as_str().into(), mtime);
                fresh.insert(name.as_str());
            }
        }
        let mut unkept: Vec<&String> = names
            .iter()
            .filter(|name| {
                !self.kept_writers.contains(name.as_str()) && !fresh.contains(name.as_str())
            })
            .collect();
        unkept.sort_by(|a, b| compare(a, b));
        if !unkept.is_empty() {
            let slice = unkept.len().div_ceil(REDATE_PASSES).max(1);
            let start = self.date_cursor % unkept.len();
            for step in 0..slice {
                let name = unkept[(start + step) % unkept.len()];
                let mtime = fs
                    .stat(&dir.join(name))
                    .map(|stat| stat.mtime_ms)
                    .unwrap_or(0.0);
                self.folder_dates.insert(name.as_str().into(), mtime);
            }
            self.date_cursor = (start + slice) % unkept.len();
        }
        let mut dated: Vec<(String, f64)> = names
            .into_iter()
            .map(|name| {
                let mtime = self.folder_dates.get(name.as_str()).copied().unwrap_or(0.0);
                (name, mtime)
            })
            .collect();
        dated.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| compare(&a.0, &b.0))
        });
        dated.truncate(self.limits.writers);
        let kept: Vec<String> = dated.into_iter().map(|(name, _)| name).collect();
        self.kept_writers = kept.iter().map(|n| n.as_str().into()).collect();
        kept
    }

    fn redate(&mut self, fs: &dyn ShareFs, dir: &Path, name: &str) {
        if self.folder_dates.contains_key(name)
            && let Ok(stat) = fs.stat(&dir.join(name))
        {
            self.folder_dates.insert(name.into(), stat.mtime_ms);
        }
    }

    fn set_problem(
        &mut self,
        rel: &str,
        kind: ReportKind,
        writer: &str,
        reason: &str,
        stamp: Option<StatStamp>,
        since: u64,
    ) -> bool {
        if let Some(entry) = self.entries.get_mut(rel) {
            let changed = entry.problem.as_deref().map(|p| p.reason.as_ref()) != Some(reason);
            if changed {
                let prob = Box::new(CompactProblem::new(rel, reason, since));
                if let Some(good) = &mut entry.good {
                    Arc::make_mut(good).problem = Some(prob.clone());
                }
                entry.problem = Some(prob);
            }
            entry.stamp = stamp;
            return changed;
        }
        let prob = Box::new(CompactProblem::new(rel, reason, since));
        self.entries.insert(
            rel.into(),
            CompactEntry {
                kind,
                writer: writer.into(),
                stamp,
                good: None,
                problem: Some(prob),
                missing_since: None,
            },
        );
        true
    }

    fn transient(&mut self, rel: &str, kind: ReportKind, writer: &str, now: u64) -> bool {
        match self.misses.miss(rel, now) {
            None => false,
            Some(since) => self.set_problem(rel, kind, writer, PERSISTENT_SHARE_ERROR, None, since),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn accept(
        &mut self,
        rel: &str,
        kind: ReportKind,
        writer: &str,
        doc: &Json,
        sha256: CompactSha,
        stamp: StatStamp,
        now: u64,
    ) -> bool {
        let seq = doc["seq"].as_u64().unwrap_or(0);
        let written_at = doc["writtenAt"].as_str().unwrap_or_default();
        if let Some(entry) = self.entries.get(rel)
            && let Some(good) = &entry.good
            && seq < good.seq()
        {
            return self.set_problem(rel, kind, writer, SEQ_WENT_BACK, Some(stamp), now);
        }
        if let Some(entry) = self.entries.get_mut(rel) {
            let had_problem = entry.problem.is_some();
            entry.stamp = Some(stamp);
            entry.problem = None;
            if let Some(good) = &mut entry.good
                && good.sha256 == sha256
            {
                if had_problem {
                    Arc::make_mut(good).problem = None;
                }
                return had_problem;
            }
            let written_at_ms = aac_num::ecma::parse_instant(written_at).unwrap_or(0);
            entry.good = Some(Arc::new(CompactSeenReport::from_projected(
                kind,
                rel,
                writer,
                doc,
                sha256,
                now,
                written_at_ms,
                None,
            )));
            return true;
        }
        let written_at_ms = aac_num::ecma::parse_instant(written_at).unwrap_or(0);
        let good = Arc::new(CompactSeenReport::from_projected(
            kind,
            rel,
            writer,
            doc,
            sha256,
            now,
            written_at_ms,
            None,
        ));
        self.entries.insert(
            rel.into(),
            CompactEntry {
                kind,
                writer: writer.into(),
                stamp: Some(stamp),
                good: Some(good),
                problem: None,
                missing_since: None,
            },
        );
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn visit(
        &mut self,
        fs: &dyn ShareFs,
        file: &Path,
        rel: &str,
        kind: ReportKind,
        writer: &str,
        card_id: &str,
        seen: &mut HashSet<String>,
        now: u64,
    ) -> bool {
        let cap = kind.cap();
        let stat = match fs.stat(file) {
            Ok(stat) => stat,
            Err(error) => {
                seen.insert(rel.to_owned());
                self.scan_partial = true;
                if is_transient(&error) {
                    return self.transient(rel, kind, writer, now);
                }
                self.misses.clear(rel);
                return self.set_problem(rel, kind, writer, "could not be read", None, now);
            }
        };
        if !stat.file {
            seen.insert(rel.to_owned());
            return self.set_problem(rel, kind, writer, "is not a regular file", None, now);
        }
        if stat.size <= cap as u64 {
            if self.scan_bytes + stat.size > self.limits.bytes {
                self.over_bytes += 1;
                return false;
            }
            self.scan_bytes += stat.size;
        }
        seen.insert(rel.to_owned());
        let stamp = StatStamp::from_stat(&stat);
        if let Some(entry) = self.entries.get(rel)
            && entry.stamp == Some(stamp)
        {
            self.misses.clear(rel);
            return false;
        }
        if stat.size > cap as u64 {
            return self.set_problem(
                rel,
                kind,
                writer,
                &format!("larger than the {} KB cap", cap / 1024),
                Some(stamp),
                now,
            );
        }
        let bytes = match fs.read(file, cap) {
            Ok(bytes) => bytes,
            Err(ReadError::OverCap) => {
                return self.set_problem(
                    rel,
                    kind,
                    writer,
                    &format!("larger than the {} KB cap", cap / 1024),
                    Some(stamp),
                    now,
                );
            }
            Err(ReadError::Io(error)) => {
                self.scan_partial = true;
                if is_transient(&error) {
                    return self.transient(rel, kind, writer, now);
                }
                self.misses.clear(rel);
                return self.set_problem(rel, kind, writer, "could not be read", None, now);
            }
        };
        self.misses.clear(rel);
        match parse_report_file(kind.as_str(), &bytes, writer, card_id) {
            Err(reason) => self.set_problem(rel, kind, writer, &reason, Some(stamp), now),
            Ok((doc, sha_hex)) => {
                let sha = CompactSha::from_hex_or_str(&sha_hex);
                self.accept(rel, kind, writer, &doc, sha, stamp, now)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn listing_failed(
        &mut self,
        name: &str,
        folder: &str,
        error: &std::io::Error,
        seen: &mut HashSet<String>,
        failed_listings: &mut HashSet<String>,
        now: u64,
        folder_problems: &mut IndexMap<Box<str>, CompactProblem>,
        before: &IndexMap<Box<str>, CompactProblem>,
    ) -> bool {
        self.scan_partial = true;
        let rel = if folder.is_empty() {
            format!("writers/{name}")
        } else {
            format!("writers/{name}/cards")
        };
        let mut reason = Some("folder could not be listed".to_owned());
        let mut since = now;
        if is_transient(error) {
            failed_listings.insert(rel.clone());
            match self.listing_misses.miss(&rel, since) {
                None => reason = None,
                Some(first) => {
                    reason = Some(PERSISTENT_LISTING_ERROR.to_owned());
                    since = first;
                }
            }
        }
        if let Some(reason) = &reason {
            put_folder_problem(folder_problems, before, &rel, reason, since);
        }
        let prefix = format!("writers/{name}/{folder}");
        let keys: Vec<(Box<str>, ReportKind, Box<str>)> = self
            .entries
            .iter()
            .filter(|(key, _)| key.starts_with(&prefix))
            .map(|(key, entry)| (key.clone(), entry.kind, entry.writer.clone()))
            .collect();
        let mut changed = false;
        let row_reason = reason.as_deref().map(folder_row_problem);
        for (key, kind, writer) in keys {
            seen.insert(key.to_string());
            if let Some(row_reason) = &row_reason
                && self.set_problem(&key, kind, &writer, row_reason, None, since)
            {
                changed = true;
            }
        }
        changed
    }

    pub(crate) fn scan(&mut self, fs: &dyn ShareFs, now: u64) -> bool {
        let writers_dir = self.root.join("v1").join("writers");
        let mut changed = false;
        self.scan_partial = false;
        self.scan_bytes = 0;
        self.over_bytes = 0;
        let writers = match fs.list(&writers_dir) {
            Ok(writers) => writers,
            Err(error) => {
                self.complete = false;
                if error.kind() != std::io::ErrorKind::NotFound && is_transient(&error) {
                    return false;
                }
                let next = if is_missing(&error) {
                    "missing"
                } else {
                    "error"
                };
                changed = self.state != next;
                self.state = next;
                self.scanned_at = Some(now);
                return changed;
            }
        };
        if self.state != "ok" {
            changed = true;
        }
        self.state = "ok";
        let mut seen: HashSet<String> = HashSet::new();
        let mut folder_problems: IndexMap<Box<str>, CompactProblem> = IndexMap::new();
        let before = self.folder_problems.clone();
        let mut failed_listings: HashSet<String> = HashSet::new();
        let mut names = Vec::new();
        for dirent in &writers {
            let name = &dirent.name;
            if name.starts_with('.') || !dirent.is_dir {
                continue;
            }
            if !is_writer_name(name) {
                if showable(name) {
                    put_folder_problem(
                        &mut folder_problems,
                        &before,
                        &format!("writers/{name}"),
                        "folder name is not a writer name",
                        now,
                    );
                }
                continue;
            }
            names.push(name.clone());
        }
        let kept = self.select_writers(names.clone(), fs, &writers_dir);
        if kept.len() < names.len() {
            self.scan_partial = true;
            put_folder_problem(
                &mut folder_problems,
                &before,
                "limits/writers",
                &format!(
                    "{} active writers past the limit of {} were not read; the most recently changed are kept",
                    names.len() - kept.len(),
                    self.limits.writers
                ),
                now,
            );
        }
        let mut cards_visited = 0usize;
        let mut cards_over = 0usize;
        let mut previous: Option<(&String, bool)> = None;
        for name in &kept {
            if let Some((done, was)) = previous.take()
                && changed != was
            {
                self.redate(fs, &writers_dir, done);
            }
            previous = Some((name, changed));
            let base = writers_dir.join(name);
            let listed: HashSet<String> = match fs.list(&base) {
                Ok(entries) => entries.into_iter().map(|entry| entry.name).collect(),
                Err(error) => {
                    if self.listing_failed(
                        name,
                        "",
                        &error,
                        &mut seen,
                        &mut failed_listings,
                        now,
                        &mut folder_problems,
                        &before,
                    ) {
                        changed = true;
                    }
                    continue;
                }
            };
            for (file, kind) in [
                ("thread.json", ReportKind::Thread),
                ("tasks.json", ReportKind::Tasks),
            ] {
                if !listed.contains(file) {
                    continue;
                }
                let rel = format!("writers/{name}/{file}");
                if self.visit(fs, &base.join(file), &rel, kind, name, "", &mut seen, now) {
                    changed = true;
                }
            }
            if !listed.contains("cards") {
                continue;
            }
            let mut cards = match fs.list(&base.join("cards")) {
                Ok(cards) => cards,
                Err(error) => {
                    if self.listing_failed(
                        name,
                        "cards/",
                        &error,
                        &mut seen,
                        &mut failed_listings,
                        now,
                        &mut folder_problems,
                        &before,
                    ) {
                        changed = true;
                    }
                    Vec::new()
                }
            };
            cards.sort_by(|a, b| compare(&a.name, &b.name));
            for card in cards {
                if card.name.starts_with('.') || !card.name.ends_with(".json") {
                    continue;
                }
                let id = &card.name[..card.name.len() - ".json".len()];
                let rel = format!("writers/{name}/cards/{}", card.name);
                if !showable(&card.name) {
                    continue;
                }
                if !card.is_file {
                    self.scan_partial = true;
                    if self.entries.contains_key(rel.as_str()) {
                        seen.insert(rel.clone());
                        self.misses.clear(&rel);
                        if self.set_problem(
                            &rel,
                            ReportKind::Card,
                            name,
                            "is not a regular file",
                            None,
                            now,
                        ) {
                            changed = true;
                        }
                    } else {
                        put_folder_problem(
                            &mut folder_problems,
                            &before,
                            &rel,
                            "is not a regular file",
                            now,
                        );
                    }
                    continue;
                }
                if cards_visited >= self.limits.cards {
                    cards_over += 1;
                    continue;
                }
                cards_visited += 1;
                if self.visit(
                    fs,
                    &base.join("cards").join(&card.name),
                    &rel,
                    ReportKind::Card,
                    name,
                    id,
                    &mut seen,
                    now,
                ) {
                    changed = true;
                }
            }
        }
        if let Some((done, was)) = previous.take()
            && changed != was
        {
            self.redate(fs, &writers_dir, done);
        }
        if cards_over > 0 {
            self.scan_partial = true;
            put_folder_problem(
                &mut folder_problems,
                &before,
                "limits/cards",
                &format!(
                    "{cards_over} card files past the limit of {} were not read",
                    self.limits.cards
                ),
                now,
            );
        }
        if self.over_bytes > 0 {
            self.scan_partial = true;
            put_folder_problem(
                &mut folder_problems,
                &before,
                "limits/bytes",
                &format!(
                    "{} files past the limit of {} MB were not read",
                    self.over_bytes,
                    self.limits.bytes / (1024 * 1024)
                ),
                now,
            );
        }
        self.entries.retain(|key, entry| {
            if seen.contains(key.as_ref()) {
                entry.missing_since = None;
                return true;
            }
            let missing_since = *entry.missing_since.get_or_insert(now);
            let retained = now.saturating_sub(missing_since) < MISSING_GRACE_MS;
            changed |= !retained;
            retained
        });
        self.misses.keep_only_strings(&seen);
        self.listing_misses.keep_only_strings(&failed_listings);
        if !same_problems(&self.folder_problems, &folder_problems) {
            changed = true;
        }
        self.folder_problems = folder_problems;
        if changed {
            self.refresh_board_paths();
        }
        if self.stat_boards(fs) {
            changed = true;
        }
        self.scanned_at = Some(now);
        let unseen_card = self.entries.values().any(|entry| {
            entry.kind == ReportKind::Card && entry.problem.is_some() && entry.good.is_none()
        });
        self.complete = !self.scan_partial && !unseen_card;
        changed
    }

    fn refresh_board_paths(&mut self) {
        let mut paths: Vec<Box<str>> = Vec::new();
        let mut push = |path: &str| {
            if !paths.iter().any(|held| held.as_ref() == path) {
                paths.push(path.into());
            }
        };
        for entry in self.entries.values() {
            if entry.kind == ReportKind::Tasks
                && let Some(row) = &entry.good
                && let Some(tasks) = row.tasks()
                && let Some(board) = tasks.board.as_deref()
            {
                push(board);
            }
        }
        for entry in self.entries.values() {
            if entry.kind != ReportKind::Thread {
                continue;
            }
            let Some(row) = &entry.good else { continue };
            let Some(thread) = row.thread() else { continue };
            for link in &thread.links {
                if link.label.to_ascii_lowercase().contains("board") {
                    push(&link.path);
                }
            }
        }
        self.board_paths = paths
            .into_iter()
            .map(|given| {
                let local = share_local_path(&given, &self.root);
                (given, local)
            })
            .collect();
    }

    fn stat_boards(&mut self, fs: &dyn ShareFs) -> bool {
        let mut next: IndexMap<Box<str>, Option<f64>> =
            IndexMap::with_capacity(self.board_paths.len());
        for (given, local) in &self.board_paths {
            let mtime = local.as_ref().and_then(|local| {
                fs.stat(local)
                    .ok()
                    .filter(|stat| stat.file)
                    .map(|stat| stat.mtime_ms)
                    .filter(|v| v.is_finite())
                    .map(|v| if v == 0.0 { 0.0 } else { v })
            });
            next.insert(given.clone(), mtime);
        }
        let changed = next.len() != self.boards.len()
            || next
                .iter()
                .any(|(key, value)| self.boards.get(key) != Some(value));
        self.boards = next;
        changed
    }

    pub(crate) fn snapshot(&self) -> CompactAgentReportsState {
        let mut problems: Vec<CompactProblem> = self
            .entries
            .values()
            .filter_map(|entry| entry.problem.as_deref().cloned())
            .chain(self.folder_problems.values().cloned())
            .collect();
        problems.sort_by(|a, b| compare(&a.file, &b.file));
        let mut threads = Vec::new();
        let mut tasks = Vec::new();
        let mut cards = Vec::new();
        for entry in self.entries.values() {
            let Some(good) = &entry.good else { continue };
            match entry.kind {
                ReportKind::Thread => threads.push(Arc::clone(good)),
                ReportKind::Tasks => tasks.push(Arc::clone(good)),
                ReportKind::Card => cards.push(Arc::clone(good)),
            }
        }
        let boards: Box<[(Box<str>, Option<f64>)]> = self
            .boards
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        CompactAgentReportsState {
            root: self.root.to_string_lossy().as_ref().into(),
            state: self.state,
            scanned_at: self.scanned_at,
            complete: self.complete,
            threads: threads.into_boxed_slice(),
            tasks: tasks.into_boxed_slice(),
            cards: cards.into_boxed_slice(),
            problems: problems.into_boxed_slice(),
            boards,
        }
    }
}

fn same_problems(
    before: &IndexMap<Box<str>, CompactProblem>,
    after: &IndexMap<Box<str>, CompactProblem>,
) -> bool {
    before.len() == after.len()
        && after.iter().all(|(key, value)| {
            before.get(key).map(|p| p.reason.as_ref()) == Some(value.reason.as_ref())
        })
}

fn put_folder_problem(
    folder_problems: &mut IndexMap<Box<str>, CompactProblem>,
    before: &IndexMap<Box<str>, CompactProblem>,
    file: &str,
    reason: &str,
    since: u64,
) {
    let since = match before.get(file) {
        Some(prior) if prior.reason.as_ref() == reason => prior.since,
        _ => since,
    };
    folder_problems.insert(file.into(), CompactProblem::new(file, reason, since));
}
