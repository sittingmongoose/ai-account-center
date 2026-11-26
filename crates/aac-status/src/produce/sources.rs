//! G16c: the six fed generations' data and facts, as the service publishes them.
//!
//! One function from the producers' readings to the exact texts the service pushes:
//! `status-reports` (`{reports}`), `status-quota` (`{quota, spendByStream}`), `status-pinned`
//! (`{pinned}`), `status-collected` (`{registry, machines, browsers, simlock, usage}`),
//! `status-answers` (`{answers, sends, answerHistoryProblem}`) and `status-facts`
//! (`{timeZone, hiddenThreadNames, machineIds, machineLabels}` plus the presentation facts over
//! the other five).
//!
//! The member order is the TypeScript envelope's (the service's feed refuses another order), and
//! every text is written with [`crate::share::js_text`]'s Node rules, so the pushed data
//! round-trips through the service's own `js_json` check byte for byte. The spend-by-stream panel
//! is composed here from the same data object the view is built from, with the facts over it.

use super::answers_feed::AnswersFeed;
use super::facts::{Acc, Facts};
use super::usage::UsageInput;
use crate::read_status::ReportView;
use aac_num::intl::Zone;
use serde_json::{Map, Value as Json, json};

/// The service's readings for one set of generations.
pub struct Readings<'a> {
    pub reports: Reports<'a>,
    pub quota: &'a Json,
    pub pinned: &'a Json,
    pub registry: &'a Json,
    /// The `machines` member: one collected wrapper per registered machine.
    pub machines: &'a Json,
    pub browsers: &'a Json,
    pub simlock: &'a Json,
    pub usage: Option<&'a UsageInput>,
    pub answers: &'a AnswersFeed,
    pub time_zone: &'a str,
    pub now: i64,
    /// The display zone's UTC offset at `now`, for the panel's one clock word.
    pub offset_minutes: i64,
    pub zone: &'a Zone,
}

pub enum Reports<'a> {
    Wire(&'a Json),
    Native(&'a ReportView),
}

impl Reports<'_> {
    fn facts(&self, acc: &mut Acc) {
        match self {
            Self::Wire(value) => acc.read(value, true),
            Self::Native(reports) => reports.facts(acc),
        }
    }
}

/// One source's published text and generation content.
#[derive(Debug, Clone)]
pub struct Sources {
    pub reports: String,
    pub reports_valid: bool,
    pub quota: String,
    pub pinned: String,
    pub collected: String,
    pub answers: String,
    pub facts_data: String,
    pub facts: String,
}

/// The six sources' texts, or the fixed reason a fact the composition needs is missing.
///
/// # Errors
/// The zone's facts cannot write a word the panel needs, or the quota report's spend view does
/// not read as this port expects: the caller declines and the TypeScript handler answers.
pub fn build(readings: &Readings<'_>) -> Result<Sources, &'static str> {
    let collected = json!({
        "registry": readings.registry,
        "machines": readings.machines,
        "browsers": readings.browsers,
        "simlock": readings.simlock,
        "usage": readings.usage,
    });
    let answers = readings.answers.to_json();
    let facts_data = facts_data(readings.time_zone);
    // Only quota and the task stream labels contribute to the spend panel. The other
    // source trees go through the accumulator once, below.
    let mut spend_facts = Acc::default();
    spend_facts.read(readings.quota, true);
    let (wire_reports, native_reports) = match readings.reports {
        Reports::Wire(value) => (value, None),
        Reports::Native(reports) => (&Json::Null, Some(reports)),
    };
    crate::read_status::spend_label_facts(wire_reports, native_reports, &mut spend_facts);
    let presentation = Facts::build(&spend_facts, Some(readings.zone)).into_presentation();
    let (clock, day) = readings.zone.words(readings.now).ok_or("zone")?;
    let now_date = crate::read_context::Date::of_now(readings.now, clock, day).ok_or("date")?;
    let panel = crate::read_status::spend_by_stream_parts(
        vec![("reports", wire_reports), ("quota", readings.quota)],
        native_reports,
        std::sync::Arc::new(presentation),
        (readings.now, now_date),
        readings.offset_minutes,
        readings.usage.map(UsageInput::account_order).unwrap_or_default(),
    )
    .ok_or("spend")?;
    let quota = json!({"quota": readings.quota, "spendByStream": panel});
    let mut acc = Acc::default();
    readings.reports.facts(&mut acc);
    for value in [&quota, readings.pinned, &collected, &answers, &facts_data] {
        acc.read(value, true);
    }
    let facts = Facts::build(&acc, Some(readings.zone));
    let (report_text, reports_valid) = match readings.reports {
        Reports::Wire(value) => (write(&json!({"reports": value})), true),
        Reports::Native(reports) => reports.source_text(),
    };
    Ok(Sources {
        reports: report_text,
        reports_valid,
        quota: write(&quota),
        pinned: write(&json!({"pinned": readings.pinned})),
        collected: write(&collected),
        answers: write(&answers),
        facts_data: write(&facts_data),
        facts: facts.to_text(),
    })
}

/// The facts data object alone (a request before the readings exist still needs its identity).
#[must_use]
pub fn facts_data(time_zone: &str) -> Json {
    json!({
        "timeZone": time_zone,
        "hiddenThreadNames": [crate::answer_delivery::DOORBELL_SENDER],
        "machineIds": crate::computers::machine_ids(),
        "machineLabels": Map::from_iter(
            crate::computers::machine_labels()
                .into_iter()
                .map(|(id, label)| (id.to_owned(), Json::from(label))),
        ),
    })
}

fn write(value: &Json) -> String {
    crate::share::js_text(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zone() -> Zone {
        crate::produce::facts::ZoneFacts {
            from: 1_700_000_000_000,
            to: 1_900_000_000_000,
            offsets: vec![[1_700_000_000_000, -300]],
        }
        .zone()
        .unwrap()
    }

    #[test]
    fn every_source_is_written_in_the_envelopes_member_order() {
        let reports = json!({"threads": [], "tasks": [], "cards": [], "problems": []});
        let quota = Json::Null;
        let pinned = json!({"unavailable": "the pinned threads have not been read yet"});
        let collected = json!({
            "registry": {"data": null, "asOf": null, "error": null},
            "machines": {},
            "browsers": {"data": null, "asOf": null, "error": null},
            "simlock": {"data": null, "asOf": null, "error": null},
            "usage": null,
        });
        let answers = AnswersFeed {
            answers: Vec::new(),
            sends: Vec::new(),
            problem: None,
        };
        let sources = build(&Readings {
            reports: Reports::Wire(&reports),
            quota: &quota,
            pinned: &pinned,
            registry: &collected["registry"],
            machines: &collected["machines"],
            browsers: &collected["browsers"],
            simlock: &collected["simlock"],
            usage: None,
            answers: &answers,
            time_zone: "America/New_York",
            now: 1_790_000_000_000,
            offset_minutes: -300,
            zone: &zone(),
        })
        .unwrap();
        assert!(
            sources
                .collected
                .starts_with("{\"registry\":{\"data\":null,\"asOf\":null,\"error\":null},\"machines\":{},")
        );
        assert!(sources.answers.starts_with("{\"answers\":[],\"sends\":[]"));
        assert!(
            sources
                .facts_data
                .starts_with("{\"timeZone\":\"America/New_York\",\"hiddenThreadNames\":[\"aac-answers\"]")
        );
        assert!(sources.facts.starts_with("{\"dates\":{"));
    }
}
