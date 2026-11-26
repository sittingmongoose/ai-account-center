//! Borrow the validated native report rows or the equivalent rows from a wire feed.
use crate::produce::answered::{CardRow, active_blocker, resolved_card_waits};
use crate::produce::facts::Acc;
use crate::read_context::{Context, rows};
use crate::share::{js_text, reports};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

static NULL: Value = Value::Null;

/// Native report rows and the Status-specific resolution of owner waits.
#[derive(Debug)]
pub struct ReportView {
    state: Arc<reports::AgentReportsState>,
    metadata: Value,
    cleared: HashSet<String>,
    serialized: HashMap<(usize, bool), Arc<SerializedRow>>,
}

#[derive(Debug)]
struct SerializedRow {
    text: String,
    valid: bool,
}

impl ReportView {
    /// Share unchanged row projections with the previous report view.
    pub fn new(state: Arc<reports::AgentReportsState>, closed: &HashSet<String>, previous: Option<&Self>) -> Self {
        let cards: Vec<_> = state
            .cards
            .iter()
            .map(|row| CardRow {
                writer: &row.writer,
                doc: &row.doc,
                sha256: &row.sha256,
            })
            .collect();
        let waits = resolved_card_waits(&cards, closed);
        let cleared = state
            .threads
            .iter()
            .filter(|row| {
                row.doc.get("blockedOn").is_some_and(|v| !v.is_null())
                    && active_blocker(&waits, &row.writer, row.doc.get("blockedOn").cloned()).is_none()
            })
            .map(|row| row.writer.clone())
            .collect();
        let mut metadata = reports::state_metadata(&state);
        reports::append_state_details(&state, &mut metadata);
        let mut view = Self {
            state,
            metadata: Value::Object(metadata),
            cleared,
            serialized: HashMap::new(),
        };
        view.serialized = ReportKind::ALL
            .into_iter()
            .flat_map(|kind| view.rows(kind).iter().map(move |row| (kind, row)))
            .map(|(kind, row)| {
                let key = view.row_key(kind, row);
                let text = previous
                    .and_then(|old| old.serialized.get(&key))
                    .cloned()
                    .unwrap_or_else(|| {
                        let value = view.row_json(kind, row);
                        Arc::new(SerializedRow {
                            valid: wire_order(&value) && !crate::read_context::proto_named(&value),
                            text: js_text(&value),
                        })
                    });
                (key, text)
            })
            .collect();
        view
    }

    pub(crate) fn meta(&self) -> &Value {
        &self.metadata
    }

    fn rows(&self, kind: ReportKind) -> &[Arc<reports::SeenReport>] {
        match kind {
            ReportKind::Threads => &self.state.threads,
            ReportKind::Tasks => &self.state.tasks,
            ReportKind::Cards => &self.state.cards,
        }
    }

    /// Visit one row at a time in the same order as the wire reports object.
    pub(crate) fn facts(&self, acc: &mut Acc) {
        for key in ["root", "state", "scannedAt", "complete"] {
            acc.read(&self.metadata[key], true);
        }
        for kind in ReportKind::ALL {
            for row in self.rows(kind) {
                acc.read_text(&row.file, true);
                acc.read_text(&row.writer, true);
                if let (ReportKind::Threads, Some(doc)) = (kind, row.doc.as_object()) {
                    for (key, item) in doc {
                        acc.read(if key == "blockedOn" { self.blocker(row) } else { item }, true);
                    }
                } else {
                    acc.read(&row.doc, true);
                }
                acc.read_text(&row.sha256, true);
                acc.read_number(row.first_seen_at as f64);
                acc.read_number(row.written_at_ms as f64);
                if let Some(problem) = &row.problem {
                    acc.read_text(&problem.file, true);
                    acc.read_text(&problem.reason, true);
                    acc.read_number(problem.since as f64);
                }
            }
        }
        acc.read(&self.metadata["problems"], true);
        acc.read(&self.metadata["boards"], true);
    }

    /// The raw feed text remains available to the internal feed protocol. The compositor
    /// reads the shared rows directly and never parses this text back into a report tree.
    pub fn source_text(&self) -> (String, bool) {
        use std::fmt::Write;
        let mut out = String::from("{\"reports\":{");
        let mut valid = wire_order(&self.metadata) && !crate::read_context::proto_named(&self.metadata);
        for key in ["root", "state", "scannedAt", "complete"] {
            let _ = write!(out, "\"{key}\":{},", js_text(&self.metadata[key]));
        }
        for kind in ReportKind::ALL {
            let _ = write!(out, "\"{}\":[", kind.key());
            for (index, row) in self.rows(kind).iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                let row = &self.serialized[&self.row_key(kind, row)];
                valid &= row.valid;
                out.push_str(&row.text);
            }
            out.push_str("],");
        }
        let _ = write!(
            out,
            "\"problems\":{},\"boards\":{}}}}}",
            js_text(&self.metadata["problems"]),
            js_text(&self.metadata["boards"])
        );
        (out, valid)
    }

    fn row_key(&self, kind: ReportKind, row: &Arc<reports::SeenReport>) -> (usize, bool) {
        // Both snapshots keep their Arcs alive, so a newly allocated row cannot reuse
        // the address of a row in the previous snapshot.
        (
            Arc::as_ptr(row) as usize,
            matches!(kind, ReportKind::Threads) && self.cleared.contains(&row.writer),
        )
    }

    fn row_json(&self, kind: ReportKind, row: &reports::SeenReport) -> Value {
        let blocker = matches!(kind, ReportKind::Threads).then(|| self.blocker(row));
        reports::row_json(row, blocker)
    }

    fn blocker<'a>(&self, row: &'a reports::SeenReport) -> &'a Value {
        if self.cleared.contains(&row.writer) {
            &NULL
        } else {
            &row.doc["blockedOn"]
        }
    }
}

fn wire_order(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().all(wire_order),
        Value::Object(map) => {
            let mut last = None;
            let mut saw_name = false;
            for (key, member) in map {
                let index = key
                    .as_bytes()
                    .first()
                    .filter(|c| c.is_ascii_digit())
                    .and_then(|_| key.parse::<u32>().ok())
                    .filter(|i| *i != u32::MAX && i.to_string() == *key);
                match index {
                    Some(index) if !saw_name && last.is_none_or(|old| old < index) => last = Some(index),
                    Some(_) => return false,
                    None => saw_name = true,
                }
                if !wire_order(member) {
                    return false;
                }
            }
            true
        }
        _ => true,
    }
}

#[derive(Clone, Copy)]
pub(super) enum ReportKind {
    Threads,
    Tasks,
    Cards,
}
impl ReportKind {
    const ALL: [Self; 3] = [Self::Threads, Self::Tasks, Self::Cards];

    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Threads => "threads",
            Self::Tasks => "tasks",
            Self::Cards => "cards",
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum ReportRow<'a> {
    Native(&'a reports::SeenReport, &'a ReportView),
    Wire(&'a Value),
}

impl<'a> ReportRow<'a> {
    pub(super) fn doc(self) -> &'a Value {
        match self {
            Self::Native(row, _) => &row.doc,
            Self::Wire(row) => &row["doc"],
        }
    }

    pub(super) fn writer(self) -> &'a str {
        match self {
            Self::Native(row, _) => &row.writer,
            Self::Wire(row) => row["writer"].as_str().unwrap_or_default(),
        }
    }

    pub(super) fn writer_value(self) -> Value {
        match self {
            Self::Native(row, _) => json!(row.writer),
            Self::Wire(row) => row["writer"].clone(),
        }
    }

    pub(super) fn writer_matches(self, writer: &Value) -> bool {
        match self {
            Self::Native(row, _) => writer.as_str() == Some(row.writer.as_str()),
            Self::Wire(row) => &row["writer"] == writer,
        }
    }

    pub(super) fn blocker(self) -> &'a Value {
        match self {
            Self::Native(row, view) => view.blocker(row),
            Self::Wire(row) => &row["doc"]["blockedOn"],
        }
    }

    pub(super) fn hash(self) -> Value {
        match self {
            Self::Native(row, _) => json!(row.sha256),
            Self::Wire(row) => row["sha256"].clone(),
        }
    }

    pub(super) fn hash_matches(self, hash: &Value) -> bool {
        match self {
            Self::Native(row, _) => hash.as_str() == Some(row.sha256.as_str()),
            Self::Wire(row) => &row["sha256"] == hash,
        }
    }

    pub(super) fn has_problem(self) -> bool {
        match self {
            Self::Native(row, _) => row.problem.is_some(),
            Self::Wire(row) => !row["problem"].is_null(),
        }
    }

    pub(super) fn problem_reason(self) -> Value {
        match self {
            Self::Native(row, _) => json!(row.problem.as_ref().map(|p| p.reason.as_str())),
            Self::Wire(row) => row["problem"]["reason"].clone(),
        }
    }

    pub(super) fn clock_ahead(self) -> Option<f64> {
        use crate::read_context::number;
        match self {
            Self::Native(row, _) => Some((row.written_at_ms as f64 - row.first_seen_at as f64) / 1000.0),
            Self::Wire(row) => {
                let first = number(&row["firstSeenAt"])?;
                Some((number(&row["writtenAtMs"]).unwrap_or(first) - first) / 1000.0)
            }
        }
    }

    pub(super) fn seen(self, ctx: &Context) -> Option<i64> {
        let (first, written) = match self {
            Self::Native(row, _) => {
                let first = ctx.epoch(&json!(row.first_seen_at))?;
                (first, ctx.epoch(&json!(row.written_at_ms)))
            }
            Self::Wire(row) => {
                let first = ctx.epoch(&row["firstSeenAt"])?;
                (first, ctx.epoch(&row["writtenAtMs"]))
            }
        };
        Some(written.map_or(first, |at| at.min(first)))
    }
}

pub(super) fn report_rows<'a>(
    owned: Option<&'a ReportView>,
    wire: &'a Value,
    kind: ReportKind,
) -> impl Iterator<Item = ReportRow<'a>> {
    let native = owned
        .into_iter()
        .flat_map(move |view| view.rows(kind).iter().map(move |row| ReportRow::Native(row, view)));
    let wire = rows(&wire[kind.key()]).iter().map(ReportRow::Wire);
    native.chain(wire)
}
