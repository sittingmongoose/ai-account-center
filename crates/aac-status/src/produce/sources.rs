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
use super::facts::Facts;
use super::usage::UsageInput;
use aac_num::intl::Zone;
use serde_json::{Map, Value as Json, json};

/// The service's readings for one set of generations.
pub struct Readings<'a> {
    pub reports: &'a Json,
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

/// One source's published text and generation content.
#[derive(Debug, Clone)]
pub struct Sources {
    pub reports: String,
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
    let facts_data = json!({
        "timeZone": readings.time_zone,
        "hiddenThreadNames": [crate::answer_delivery::DOORBELL_SENDER],
        "machineIds": crate::computers::machine_ids(),
        "machineLabels": Map::from_iter(
            crate::computers::machine_labels()
                .into_iter()
                .map(|(id, label)| (id.to_owned(), Json::from(label))),
        ),
    });
    let partial_facts = Facts::of(
        &[readings.reports, readings.quota, readings.pinned, &collected, &answers],
        Some(readings.zone),
    );
    let presentation: crate::read_context::Presentation =
        serde_json::from_value(partial_facts.to_json()).map_err(|_| "facts")?;
    let now_date = crate::read_context::Date::of_now(
        readings.now,
        readings.zone.words(readings.now).ok_or("zone")?.0,
        readings.zone.words(readings.now).ok_or("zone")?.1,
    )
    .ok_or("date")?;
    let mut data = Map::new();
    data.insert("reports".into(), readings.reports.clone());
    data.insert("quota".into(), readings.quota.clone());
    data.insert("pinned".into(), readings.pinned.clone());
    data.insert("collected".into(), collected.clone());
    data.insert("answers".into(), answers.clone());
    data.insert("facts".into(), facts_data.clone());
    let panel = crate::read_status::spend_by_stream(
        &Json::Object(data),
        std::sync::Arc::new(presentation),
        (readings.now, now_date),
        readings.offset_minutes,
        readings.usage.map(UsageInput::account_order).unwrap_or_default(),
    )
    .ok_or("spend")?;
    let quota = json!({"quota": readings.quota, "spendByStream": panel});
    let facts = Facts::of(
        &[
            readings.reports,
            &quota,
            readings.pinned,
            &collected,
            &answers,
            &facts_data,
        ],
        Some(readings.zone),
    );
    Ok(Sources {
        reports: write(&json!({"reports": readings.reports})),
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
            reports: &reports,
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
