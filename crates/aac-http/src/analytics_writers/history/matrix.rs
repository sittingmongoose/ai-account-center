#![cfg(test)]

use super::*;
use std::collections::HashSet;
use std::fs;

#[derive(Clone, Copy, Debug)]
enum Phase {
    Migration,
    Backfill,
    Live,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Availability {
    Online,
    Offline,
    Returning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Responses {
    Identical,
    LaterHour,
    SameHour,
}

#[derive(Clone, Copy, Debug)]
struct Case {
    phase: Phase,
    local: bool,
    availability: Availability,
    original_first: bool,
    responses: Responses,
    settled: bool,
    repair: bool,
}

struct Observation {
    host: &'static str,
    answer: Value,
}

impl Observation {
    fn archive(&self) -> Value {
        json!({
            "version":8, "lastScanAt":"2026-11-01T13:00:00.000Z",
            "fingerprints":{"claude":self.answer["kinds"]["claude"]["fingerprints"]},
            "rows": self.answer["rows"], "srows": self.answer["srows"]
        })
    }

    fn part(&self, state_index: usize) -> claude_mirrors::Part<'_> {
        let rows = |name: &str| {
            self.answer[name]
                .as_array()
                .expect("scanner rows")
                .iter()
                .map(|row| serde_json::from_value(row.clone()).expect("scanner row"))
                .collect()
        };
        claude_mirrors::Part {
            host: self.host,
            state_index,
            rows: rows("rows"),
            sessions: rows("srows"),
            fingerprints: self.answer["kinds"]["claude"]["fingerprints"].clone(),
        }
    }
}

fn scan(
    home: &Path,
    host: &'static str,
    mirrored: bool,
    at: i64,
    responses: Responses,
) -> Observation {
    scan_named(home, host, mirrored, at, responses, "shared", true)
}

fn scan_named(
    home: &Path,
    host: &'static str,
    mirrored: bool,
    at: i64,
    responses: Responses,
    transcript: &str,
    keyed: bool,
) -> Observation {
    let subdir = if mirrored {
        "ssh-12345678-1234-1234-1234-123456789abc"
    } else {
        "local-project"
    };
    let folder = home.join(".claude/projects").join(subdir);
    fs::create_dir_all(&folder).expect("fixture projects");
    let event = |id: &str, time: i64, input: u64, output: u64| {
        json!({"type":"assistant","timestamp":aac_num::ecma::to_iso_string(time),
            "sessionId":keyed.then_some(transcript),
            "message":{"id":format!("{transcript}-{id}"),"model":"claude-opus-4-5","usage":{"input_tokens":input,"output_tokens":output}}})
    };
    let mut log = format!("{}\n", event("response-one", at, 70, 30));
    if !mirrored && responses != Responses::Identical {
        let next = if responses == Responses::LaterHour {
            at + 3 * 3_600_000
        } else {
            at + 60_000
        };
        log.push_str(&format!("{}\n", event("response-two", next, 130, 40)));
    }
    fs::write(folder.join(format!("{transcript}.jsonl")), log).expect("fixture transcript");
    let request =
        aac_usage::protocol::parse_request(br#"{"kinds":["claude"],"minDateMs":0,"cwd":false}"#)
            .expect("scanner request");
    let answer = aac_usage::protocol::scan(&request, home, &aac_usage::discover::Env::default());
    let names = answer["kinds"]["claude"]["fingerprints"]
        .as_object()
        .expect("Claude prints");
    assert_eq!(names.len(), 1);
    assert!(
        names
            .values()
            .all(|print| print["claudeMirror"] == mirrored)
    );
    Observation { host, answer }
}

fn archive_path(host: &str) -> String {
    format!("legacy/cache/analytics-remote-v1/{host}.json")
}

struct Fixture<'a> {
    home: &'a Path,
    now: i64,
    case: Case,
}

impl Fixture<'_> {
    fn publish(&self, store: &Engine, phase: Phase, observations: &[&Observation]) {
        let pricing = ResolverPricing::new(self.home);
        match phase {
            Phase::Migration => {
                let directory = self.home.join("cache/analytics-remote-v1");
                fs::create_dir_all(&directory).expect("legacy cache directory");
                let mut archive = Vec::new();
                for observation in observations {
                    let bytes =
                        serde_json::to_vec(&observation.archive()).expect("legacy cache bytes");
                    fs::write(directory.join(format!("{}.json", observation.host)), &bytes)
                        .expect("legacy cache");
                    archive.push((archive_path(observation.host), bytes));
                    if self.case.repair {
                        let part = observation.part(0);
                        let worker = part.worker(self.now, &pricing);
                        let value = report::rows_report(&report::RowsReportArgs {
                            computer: part.host,
                            tool: "claude",
                            data: &[worker],
                            kind: "import",
                            read_at: self.now,
                            complete: false,
                            unreadable_records: None,
                            cause: Some("retired_cache"),
                            now: self.now,
                        });
                        commit_report(store, value).expect("seed prior import");
                    }
                }
                if self.case.repair {
                    analytics_cache::archive(store, &archive).expect("seed prior archive");
                    let index =
                        analytics_cache::archived_index(store, "legacy/cache/analytics-remote-v1/")
                            .expect("prior index");
                    let digest =
                        aac_core::sha256_hex(serde_json::to_vec(&index).expect("index bytes"));
                    store
                        .set_meta("analytics_remote_import_v4", &format!("0:{digest}"))
                        .expect("prior receipt");
                    store
                        .set_meta("analytics_files_migrated_v4", "0")
                        .expect("prior marker");
                    if self.case.settled {
                        activity::close_days(store, self.now as u64).expect("settle prior import");
                    }
                }
                let available =
                    self.case.availability == Availability::Online || self.case.original_first;
                let local_originals = if self.case.local && available {
                    crate::analytics_writers::local_inventory::original_names(
                        &self.home.join("original"),
                        &aac_usage::discover::Env::default(),
                        &[],
                    )
                } else {
                    Default::default()
                };
                let migration = super::super::migration::Migration {
                    store,
                    root: self.home,
                    local: Some("nas1"),
                    local_originals,
                    now: self.now,
                    pricing,
                };
                assert!(migration.run().expect("migrate fixture archive"));
            }
            Phase::Backfill => {
                let sources: Vec<_> = observations
                    .iter()
                    .map(|observation| Source {
                        computer: observation.host,
                        tool: "claude",
                        epoch: 0,
                    })
                    .collect();
                for (source, observation) in sources.iter().zip(observations) {
                    assert!(
                        source
                            .accept(
                                store,
                                source.load(store).expect("history cache"),
                                &observation.answer,
                                true,
                                self.now,
                                &pricing
                            )
                            .expect("history scan")
                    );
                }
                Job::default()
                    .publish_claude(store, &sources, self.now, &pricing)
                    .expect("historical publication");
            }
            Phase::Live => {
                let mut parts: Vec<_> = observations
                    .iter()
                    .enumerate()
                    .map(|(index, observation)| observation.part(index))
                    .collect();
                let mut states: Vec<_> = observations
                    .iter()
                    .map(
                        |observation| json!({"tool":"claude","host":observation.host,"state":"ok"}),
                    )
                    .collect();
                super::super::claude_contributions::prepare_live(
                    store,
                    &mut parts,
                    &mut states,
                    self.now,
                    &pricing,
                )
                .expect("live ownership");
                let remote: Vec<_> = parts
                    .iter()
                    .map(|part| {
                        let worker = part.worker(self.now, &pricing);
                        json!({"tool":"claude","host":part.host,"data":worker})
                    })
                    .collect();
                let reports = report::build_activity_reports(&report::ReportArgs {
                    local: &HashMap::new(),
                    remote: &remote,
                    states: &states,
                    window_from_ms: self.now - 31 * super::super::DAY,
                    now: self.now,
                    local_complete: &HashSet::new(),
                    scanner_owned: &HashSet::from(["claude".into()]),
                });
                let reports = report::contract_reports(&reports).expect("live reports");
                let result = activity::ingest(store, &reports).expect("live ingest");
                assert_eq!(result.accepted, reports.len(), "{:?}", result.refused);
            }
        }
    }
}

fn matrix_case(case: Case) {
    let tmp = tempfile::tempdir().expect("fixture");
    let home = tmp.path();
    let db = home.join("store");
    let open = || aac_store::ops::open_engine(&db.to_string_lossy(), None, None).expect("Store");
    let now = aac_store::engine::now_ms() as i64;
    let at = (now - 8 * super::super::DAY).div_euclid(86_400_000) * 86_400_000 + 13 * 3_600_000;
    let fixture = Fixture { home, now, case };
    let original = scan(
        &home.join("original"),
        if case.local { "nas1" } else { "mac" },
        false,
        at,
        case.responses,
    );
    let mirror = scan(&home.join("mirror"), "odroid", true, at, case.responses);
    let store = open();
    let primary = if case.original_first {
        &original
    } else {
        &mirror
    };
    let secondary = if case.original_first {
        &mirror
    } else {
        &original
    };
    let other_phase = match case.phase {
        Phase::Live => Phase::Backfill,
        _ => Phase::Live,
    };
    match case.availability {
        Availability::Online => fixture.publish(&store, case.phase, &[primary, secondary]),
        Availability::Offline => fixture.publish(&store, case.phase, &[&mirror]),
        Availability::Returning if case.original_first => {
            fixture.publish(&store, other_phase, &[primary])
        }
        Availability::Returning => fixture.publish(&store, case.phase, &[primary]),
    }
    if case.settled {
        activity::close_days(&store, now as u64).expect("settle the first publication");
    }
    store.close().expect("close between phases");
    drop(store);
    let store = open();
    if case.availability == Availability::Returning {
        let phase = if case.original_first {
            case.phase
        } else {
            other_phase
        };
        fixture.publish(&store, phase, &[secondary]);
    }
    store.close().expect("close after publication");
    drop(store);
    let store = open();
    let result = aac_store::ops::call(
        &store,
        "activityRange",
        &json!({"fromMs":at,"toMs":at+4*3_600_000}).to_string(),
    )
    .expect("Store report");
    let got: Vec<_> = result["rows"]
        .as_array()
        .expect("usage rows")
        .iter()
        .map(|row| [5, 6, 12].map(|index| row[index].as_u64().expect("usage counts")))
        .collect();
    let expected = match (case.availability, case.responses) {
        (Availability::Offline, _) | (_, Responses::Identical) => vec![[70, 30, 1]],
        (_, Responses::LaterHour) => vec![[70, 30, 1], [130, 40, 1]],
        (_, Responses::SameHour) => vec![[200, 70, 2]],
    };
    assert_eq!(got, expected, "{case:?}: {result}");
    assert_eq!(result["sessions"]["total"], 1, "{case:?}: {result}");
    let session = &result["sessions"]["sample"][0]["parts"][0];
    assert_eq!(
        session["inputTokens"],
        expected.iter().map(|row| row[0]).sum::<u64>(),
        "{case:?}: {result}"
    );
    assert_eq!(
        session["outputTokens"],
        expected.iter().map(|row| row[1]).sum::<u64>(),
        "{case:?}: {result}"
    );
}

#[test]
fn claude_contribution_matrix() {
    let mut cases = 0;
    for phase in [Phase::Migration, Phase::Backfill, Phase::Live] {
        for local in [false, true] {
            for availability in [
                Availability::Online,
                Availability::Offline,
                Availability::Returning,
            ] {
                for original_first in [false, true] {
                    if availability == Availability::Offline && original_first {
                        continue;
                    }
                    for responses in [
                        Responses::Identical,
                        Responses::LaterHour,
                        Responses::SameHour,
                    ] {
                        for settled in [false, true] {
                            for repair in [false, true] {
                                if repair && !matches!(phase, Phase::Migration) {
                                    continue;
                                }
                                matrix_case(Case {
                                    phase,
                                    local,
                                    availability,
                                    original_first,
                                    responses,
                                    settled,
                                    repair,
                                });
                                cases += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(cases, 240);
    eprintln!("Claude contribution matrix: {cases}/{cases}");
}

#[test]
fn separate_unkeyed_mirrors_in_one_cell_are_reconciled_across_restarts() {
    let tmp = tempfile::tempdir().expect("fixture");
    let home = tmp.path();
    let db = home.join("store");
    let open = || aac_store::ops::open_engine(&db.to_string_lossy(), None, None).expect("Store");
    let now = aac_store::engine::now_ms() as i64;
    let at = (now - 8 * super::super::DAY).div_euclid(3_600_000) * 3_600_000;
    let case = Case {
        phase: Phase::Live,
        local: true,
        availability: Availability::Returning,
        original_first: false,
        responses: Responses::SameHour,
        settled: true,
        repair: false,
    };
    let fixture = Fixture { home, now, case };
    let mut mirror = scan_named(
        &home.join("mirror-a"),
        "odroid",
        true,
        at,
        Responses::Identical,
        "a",
        false,
    );
    let second = scan_named(
        &home.join("mirror-b"),
        "odroid",
        true,
        at,
        Responses::Identical,
        "b",
        false,
    );
    for key in ["rows", "srows"] {
        mirror.answer[key]
            .as_array_mut()
            .expect("scanner rows")
            .extend(
                second.answer[key]
                    .as_array()
                    .expect("scanner rows")
                    .iter()
                    .cloned(),
            );
    }
    let prints = |observation: &Observation| {
        observation.answer["kinds"]["claude"]["fingerprints"]
            .as_object()
            .expect("scanner prints")
            .clone()
    };
    mirror.answer["kinds"]["claude"]["fingerprints"]
        .as_object_mut()
        .expect("scanner prints")
        .extend(prints(&second));
    let a = scan_named(
        &home.join("original-a"),
        "nas1",
        false,
        at,
        Responses::SameHour,
        "a",
        false,
    );
    let b = scan_named(
        &home.join("original-b"),
        "mac",
        false,
        at,
        Responses::Identical,
        "b",
        false,
    );
    let check = |store: &Engine, expected: [u64; 3]| {
        let result = aac_store::ops::call(
            store,
            "activityRange",
            &json!({"fromMs":at,"toMs":at+3_600_000}).to_string(),
        )
        .expect("Store report");
        assert_eq!(
            result["rows"].as_array().expect("hour rows").len(),
            1,
            "{result}"
        );
        let totals = [5, 6, 12].map(|col| result["rows"][0][col].as_u64().expect("usage count"));
        assert_eq!(totals, expected, "{result}");
    };
    let store = open();
    fixture.publish(&store, Phase::Live, &[&mirror]);
    check(&store, [140, 60, 2]);
    activity::close_days(&store, now as u64).expect("settle the mirror hour");
    store.close().expect("close Store");
    drop(store);
    let store = open();
    fixture.publish(&store, Phase::Backfill, &[&a]);
    check(&store, [270, 100, 3]);
    store.close().expect("close Store");
    drop(store);
    let store = open();
    fixture.publish(&store, Phase::Live, &[&b]);
    check(&store, [270, 100, 3]);
    let cells = activity::dump_cells(&store).expect("read cell sources");
    assert!(
        cells.iter().all(|(host, _, _)| host != "odroid"),
        "{cells:?}"
    );
}
