//! The composer's words, over small fixture inputs. No share, no database, no clock: every
//! test passes `nowMs` itself.

use aac_status::pinned::{ComposeInput, PinnedUpdate, compose};
use serde_json::{Value, json};

const NOW: i64 = 1_794_116_000_000; // 2026-11-08T05:33:20Z
const OFFSET: i64 = -300; // the dashboard's zone, for clock times

fn input(extra: Value) -> ComposeInput {
    let base = json!({ "nowMs": NOW, "utcOffsetMinutes": OFFSET });
    let mut merged = base.as_object().unwrap().clone();
    for (key, value) in extra.as_object().unwrap() {
        merged.insert(key.clone(), value.clone());
    }
    serde_json::from_value(Value::Object(merged)).unwrap()
}

fn pin(id: &str, order: Value) -> Value {
    json!({
        "machine": "vm", "id": id, "title": format!("Thread {id}"),
        "pinnedAt": "2026-11-08T10:00:00Z", "pinOrderKey": order
    })
}

fn thread(fields: Value) -> Value {
    // The composer takes the thread's fields flattened, as the TypeScript side projects them.
    let mut merged = json!({
        "writer": "vm.pm7-build", "writtenAt": "2026-11-08T05:30:00Z", "writtenAtMs": NOW - 200_000,
        "seq": 12, "state": "working", "ttlSeconds": 1800
    })
    .as_object()
    .unwrap()
    .clone();
    for (key, value) in fields.as_object().unwrap() {
        merged.insert(key.clone(), value.clone());
    }
    Value::Object(merged)
}

fn one_update(input: &ComposeInput) -> PinnedUpdate {
    compose(input).updates.into_iter().next().unwrap()
}

#[test]
fn the_full_update_from_a_reporting_thread() {
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({
            "t3Thread": "t1", "name": "Puppet Master", "harness": "claude",
            "model": "Opus 5.5", "effort": "xhigh", "project": "pm7", "stream": "pm7-build",
            "doing": "Windows 10 sweep for stream F, batch 9."
        }))],
        "tasks": [json!({
            "writer": "vm.pm7-build", "writtenAtMs": NOW - 100_000, "seq": 40,
            "project": "pm7", "stream": "pm7-build",
            "items": [
                { "id": "F-07", "title": "Tray icon sharp at 125% scale", "state": "fixing",
                  "updatedAtMs": null,
                  "reviews": [
                    { "atMs": NOW - 3_600_000, "verdict": "fail" },
                    { "atMs": NOW - 1_800_000, "verdict": "fail" },
                    { "atMs": NOW - 600_000, "verdict": "fail" }
                  ] },
                { "id": "F-08", "title": "Sticker block landed", "state": "landed",
                  "landed": { "atMs": NOW - 500_000 } },
                { "id": "F-01", "title": "Plan the sweep", "state": "done",
                  "updatedAtMs": NOW - 900_000 }
            ]
        })]
    }));
    let view = compose(&built);
    assert_eq!(view.head, "1 pinned in T3 · 1 looping");
    let update = &view.updates[0];
    assert_eq!(
        update.who.as_deref(),
        Some("Puppet Master · pm7-build · Opus 5.5 xhigh")
    );
    assert_eq!(update.report_state, "fresh");
    assert_eq!(update.percent.as_ref().unwrap().text, "66%");
    assert_eq!(update.percent.as_ref().unwrap().done, Some(2));
    assert_eq!(update.percent.as_ref().unwrap().of, Some(3));
    assert_eq!(update.now.as_deref(), Some("Windows 10 sweep for stream F, batch 9."));
    assert_eq!(update.blocked.text, "None.");
    assert_eq!(update.looping.answer, "yes");
    assert_eq!(
        update.looping.text,
        "Yes. F-07 has gone back for fixes 3 times without a passing review."
    );
    // Left has the looping item first; done is newest landed first.
    let left: Vec<&str> = update.left.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(left, ["F-07"]);
    let done: Vec<&str> = update.done.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(done, ["F-08", "F-01"]);
    assert_eq!(update.updated.as_ref().unwrap().text, "Updated 3 min ago");
    assert_eq!(view.counts.looping, 1);
    assert_eq!(view.counts.waiting_on_you, 0);
}

#[test]
fn waiting_on_you_comes_from_every_source_that_names_it() {
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({
            "t3Thread": "t1", "project": "pm7", "stream": "f",
            "blockedOn": { "kind": "owner", "what": "Pick the reviewers for batch B09", "since": null }
        }))],
        "tasks": [json!({
            "writer": "vm.pm7-build", "writtenAtMs": NOW - 100_000, "seq": 40,
            "project": "pm7", "stream": "f",
            "items": [
                { "id": "F-02", "title": "Icon set", "state": "blocked",
                  "blockedOn": { "kind": "owner", "what": "Choose the icon set", "since": null } },
                { "id": "F-03", "title": "Build icons", "state": "todo",
                  "blockedOn": { "kind": "review", "what": "A reviewer for F-02", "since": null } }
            ]
        })],
        "openCards": [json!({
            "writer": "vm.pm7-build", "id": "card-1", "seq": 3, "title": "Which tray icon?"
        })],
        "statuses": [json!({
            "writer": "vm.pm7-build", "writtenAt": "2026-11-08T05:00:00Z",
            "writtenAtMs": NOW - 2_000_000, "seq": 18, "t3Thread": "t1",
            "summary": "Sweeping.", "percent": null,
            "blockers": [ { "text": "Say yes or no to the tray icon", "waitingOn": "owner" } ]
        })]
    }));
    let update = one_update(&built);
    assert!(update.blocked.waiting_on_you);
    assert_eq!(
        update.blocked.text,
        "Waiting on you: Pick the reviewers for batch B09; Choose the icon set; \
         Say yes or no to the tray icon; Which tray icon?. Blocked: A reviewer for F-02 (waiting on review)."
    );
}

#[test]
fn a_blocked_state_without_a_reason_stays_blocked() {
    // `thread@1.state = "blocked"` (or a `tasks@1` item) with no `blockedOn`: the state is
    // known, the reason is not, and the update must say both — never "None." (Sol M1).
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({ "t3Thread": "t1", "state": "blocked" }))]
    }));
    let update = one_update(&built);
    assert_eq!(update.blocked.text, "Blocked; the report does not say on what.");
    assert!(!update.blocked.waiting_on_you);

    // A blocked item without a reason names the item, still never "None."
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({ "t3Thread": "t1", "project": "pm7", "stream": "f" }))],
        "tasks": [json!({
            "writer": "vm.pm7-build", "writtenAtMs": NOW - 100_000, "seq": 40,
            "project": "pm7", "stream": "f",
            "items": [ { "id": "B-1", "title": "Fixture blocked item", "state": "blocked" } ]
        })]
    }));
    let update = one_update(&built);
    assert_eq!(update.blocked.text, "Blocked on B-1; the report does not say on what.");
    assert!(!update.blocked.waiting_on_you);

    // A blocked thread and a named blocker together: the named one reads as before, the
    // unexplained one says so beside it.
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({
            "t3Thread": "t1", "state": "blocked", "project": "pm7", "stream": "f",
            "blockedOn": { "kind": "review", "what": "A reviewer for B-2", "since": null }
        }))],
        "tasks": [json!({
            "writer": "vm.pm7-build", "writtenAtMs": NOW - 100_000, "seq": 40,
            "project": "pm7", "stream": "f",
            "items": [ { "id": "B-2", "title": "Fixture blocked item", "state": "blocked" } ]
        })]
    }));
    let update = one_update(&built);
    assert_eq!(
        update.blocked.text,
        "Blocked: A reviewer for B-2 (waiting on review). Blocked on B-2; the report does not say on what."
    );
}

#[test]
fn a_thread_with_no_report_says_so_and_stops() {
    let built = input(json!({ "pins": [pin("t1", json!("a"))] }));
    let update = one_update(&built);
    assert_eq!(update.report_state, "none");
    assert_eq!(
        update.report_note.as_deref(),
        Some("This thread doesn't report its status yet.")
    );
    assert_eq!(update.percent, None);
    assert_eq!(update.percent_note.as_deref(), Some("No task list."));
    assert_eq!(update.blocked.text, "Not reported.");
    assert_eq!(update.looping.answer, "cantTell");
    assert_eq!(update.updated, None);
    assert_eq!(update.who, None);
    let view = compose(&built);
    assert_eq!(view.head, "1 pinned in T3");
}

#[test]
fn stopped_reporting_is_named_with_its_age_and_an_unpin_hint() {
    let stale_at = NOW - 25 * 24 * 60 * 60 * 1000;
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({
            "t3Thread": "t1", "writtenAt": "2025-10-14T05:30:00Z", "writtenAtMs": stale_at,
            "doing": "Old work."
        }))],
        "statuses": [json!({
            "writer": "vm.pm7-build", "writtenAt": "2025-10-14T05:00:00Z",
            "writtenAtMs": stale_at - 60_000, "seq": 3, "t3Thread": "t1",
            "summary": "The last summary.", "percent": 70
        })]
    }));
    let update = one_update(&built);
    assert_eq!(update.report_state, "stale");
    assert_eq!(
        update.stale_note.as_deref(),
        Some("No status update for 25 days. If it is finished, unpin it in T3.")
    );
    assert_eq!(update.summary.as_deref(), Some("The last summary."));
    assert_eq!(update.updated.as_ref().unwrap().text, "Updated 25 days ago");
    // No task list is reachable without the thread's project and stream match; the estimate
    // stands in.
    assert_eq!(update.percent.as_ref().unwrap().text, "70%");
    assert_eq!(update.percent_note.as_deref(), Some("About 70%, by its own estimate."));
}

#[test]
fn a_past_time_to_live_counts_as_stopped_even_inside_a_day() {
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({
            "t3Thread": "t1", "writtenAtMs": NOW - 3 * 60 * 60 * 1000,
            "ttlSeconds": 1800
        }))]
    }));
    assert_eq!(one_update(&built).report_state, "stale");
}

#[test]
fn an_unreadable_report_greys_the_last_good_copy_with_a_clock_time() {
    let since = NOW - 4 * 60 * 60 * 1000;
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({
            "t3Thread": "t1", "writtenAtMs": NOW - 60_000,
            "problem": "its content is not valid", "problemSinceMs": since
        }))]
    }));
    let update = one_update(&built);
    assert_eq!(update.report_state, "unreadable");
    // The problem began at 01:33:20Z, which the dashboard's zone (UTC-5) shows as 8:33 PM.
    assert_eq!(
        update.unreadable_note.as_deref(),
        Some("Could not be read since 8:33 PM.")
    );
}

#[test]
fn the_task_list_and_the_estimate_disagreeing_is_said_both_ways() {
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({ "t3Thread": "t1", "project": "pm7", "stream": "f" }))],
        "tasks": [json!({
            "writer": "vm.pm7-build", "writtenAtMs": NOW - 100_000, "seq": 40,
            "project": "pm7", "stream": "f",
            "items": [
                { "id": "F-01", "title": "a", "state": "done" },
                { "id": "F-02", "title": "b", "state": "todo" },
                { "id": "F-03", "title": "c", "state": "todo" }
            ]
        })],
        "statuses": [json!({
            "writer": "vm.pm7-build", "writtenAt": "2026-11-08T05:00:00Z",
            "writtenAtMs": NOW - 2_000_000, "seq": 18, "t3Thread": "t1",
            "summary": "Sweeping.", "percent": 70
        })]
    }));
    let update = one_update(&built);
    assert_eq!(update.percent.as_ref().unwrap().text, "33%");
    assert_eq!(
        update.percent_note.as_deref(),
        Some("Its task list says 1 of 3 (33%); the thread estimates 70%.")
    );
}

#[test]
fn no_task_list_and_an_estimate_shows_the_estimate_by_its_own_words() {
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({ "t3Thread": "t1", "project": "pm7", "stream": "f" }))],
        "statuses": [json!({
            "writer": "vm.pm7-build", "writtenAt": "2026-11-08T05:00:00Z",
            "writtenAtMs": NOW - 2_000_000, "seq": 18, "t3Thread": "t1",
            "summary": "Half done.", "percent": 50,
            "left": [ { "id": "F-07", "title": "Tray icon sharp at 125% scale" } ],
            "done": [ { "id": "F-08", "title": "Sticker block landed" } ]
        })]
    }));
    let update = one_update(&built);
    assert_eq!(update.percent.as_ref().unwrap().text, "50%");
    assert_eq!(update.percent_note.as_deref(), Some("About 50%, by its own estimate."));
    // Without a task list, the status document's own left and done stand in.
    assert_eq!(update.left[0].id, "F-07");
    assert_eq!(update.done[0].id, "F-08");
    assert_eq!(update.now.as_deref(), Some("Half done."));
}

#[test]
fn the_loop_rule_fires_at_three_failing_reviews_in_a_row_and_names_the_item() {
    let board = |reviews: Value| {
        input(json!({
            "pins": [pin("t1", json!("a"))],
            "threads": [thread(json!({ "t3Thread": "t1", "project": "pm7", "stream": "f" }))],
            "tasks": [json!({
                "writer": "vm.pm7-build", "writtenAtMs": NOW - 100_000, "seq": 40,
                "project": "pm7", "stream": "f",
                "items": [ { "id": "F-07", "title": "Tray icon", "state": "fixing",
                              "reviews": reviews } ]
            })]
        }))
    };
    let two = board(json!([
        { "atMs": NOW - 3_600_000, "verdict": "fail" },
        { "atMs": NOW - 1_800_000, "verdict": "fail" }
    ]));
    assert_eq!(one_update(&two).looping.answer, "no");

    let three = board(json!([
        { "atMs": NOW - 3_600_000, "verdict": "fail" },
        { "atMs": NOW - 1_800_000, "verdict": "fail" },
        { "atMs": NOW - 600_000, "verdict": "fail" }
    ]));
    assert_eq!(one_update(&three).looping.answer, "yes");

    // A passing round starts the count over, so old failures do not loop.
    let passed = board(json!([
        { "atMs": NOW - 3_600_000, "verdict": "fail" },
        { "atMs": NOW - 1_800_000, "verdict": "fail" },
        { "atMs": NOW - 1_200_000, "verdict": "pass" },
        { "atMs": NOW - 600_000, "verdict": "fail" }
    ]));
    assert_eq!(one_update(&passed).looping.answer, "no");
}

#[test]
fn a_passing_or_unclear_verdict_never_counts_as_a_failing_round() {
    let board = |reviews: Value| {
        input(json!({
            "pins": [pin("t1", json!("a"))],
            "threads": [thread(json!({ "t3Thread": "t1", "project": "pm7", "stream": "f" }))],
            "tasks": [json!({
                "writer": "vm.pm7-build", "writtenAtMs": NOW - 100_000, "seq": 40,
                "project": "pm7", "stream": "f",
                "items": [ { "id": "F-07", "title": "Tray icon", "state": "review",
                              "reviews": reviews } ]
            })]
        }))
    };
    // Passing with follow-ups is a pass, however it is spelled.
    for verdict in [
        "PASS WITH FOLLOW-UPS",
        " pass with notes ",
        "Pass with nonblocking follow-ups",
        "passed",
        "LGTM",
    ] {
        let follow_ups = board(json!([
            { "atMs": NOW - 3_600_000, "verdict": "PASS WITH FOLLOW-UPS" },
            { "atMs": NOW - 1_800_000, "verdict": "PASS WITH FOLLOW-UPS" },
            { "atMs": NOW - 600_000, "verdict": verdict }
        ]));
        assert_eq!(
            one_update(&follow_ups).looping.answer,
            "no",
            "verdict {verdict} must not count as a failing round"
        );
    }
    // A verdict that says neither (the reviewer's own words) is unclear: it never counts
    // toward a loop, and it breaks the row of consecutive failures.
    let unclear = board(json!([
        { "atMs": NOW - 3_600_000, "verdict": "fail" },
        { "atMs": NOW - 1_800_000, "verdict": "fail" },
        { "atMs": NOW - 1_200_000, "verdict": "inconclusive: rerun the probes" },
        { "atMs": NOW - 600_000, "verdict": "fail" }
    ]));
    assert_eq!(one_update(&unclear).looping.answer, "no");

    // The documented failure spellings still fail, case and trim included.
    for verdict in ["FAIL", " Failed ", "BLOCK", "blocked", "CHANGES REQUESTED", "rejected"] {
        let failing = board(json!([
            { "atMs": NOW - 3_600_000, "verdict": "fail" },
            { "atMs": NOW - 1_800_000, "verdict": "fail" },
            { "atMs": NOW - 600_000, "verdict": verdict }
        ]));
        assert_eq!(
            one_update(&failing).looping.answer,
            "yes",
            "verdict {verdict} must count as a failing round"
        );
    }
}

#[test]
fn reviews_outside_the_last_day_do_not_loop_and_undated_ones_do() {
    let outside = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({ "t3Thread": "t1", "project": "pm7", "stream": "f" }))],
        "tasks": [json!({
            "writer": "vm.pm7-build", "writtenAtMs": NOW - 100_000, "seq": 40,
            "project": "pm7", "stream": "f",
            "items": [ { "id": "F-07", "title": "Tray icon", "state": "fixing",
                "reviews": [
                    { "atMs": NOW - 3 * 24 * 60 * 60 * 1000, "verdict": "fail" },
                    { "atMs": NOW - 2 * 24 * 60 * 60 * 1000, "verdict": "fail" },
                    { "atMs": NOW - 24 * 60 * 60 * 1000 - 1, "verdict": "fail" }
                ] } ]
        })]
    }));
    assert_eq!(one_update(&outside).looping.answer, "no");

    let undated = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({ "t3Thread": "t1", "project": "pm7", "stream": "f" }))],
        "tasks": [json!({
            "writer": "vm.pm7-build", "writtenAtMs": NOW - 100_000, "seq": 40,
            "project": "pm7", "stream": "f",
            "items": [ { "id": "F-07", "title": "Tray icon", "state": "fixing",
                "reviews": [ { "atMs": null, "verdict": "fail" },
                             { "atMs": null, "verdict": "fail" },
                             { "atMs": null, "verdict": "fail" } ] } ]
        })]
    }));
    assert_eq!(one_update(&undated).looping.answer, "yes");
}

#[test]
fn the_threads_own_looping_answer_is_used_when_the_board_stays_quiet() {
    let base = || {
        input(json!({
            "pins": [pin("t1", json!("a"))],
            "threads": [thread(json!({ "t3Thread": "t1", "project": "pm7", "stream": "f" }))],
            "statuses": [json!({
                "writer": "vm.pm7-build", "writtenAt": "2026-11-08T05:00:00Z",
                "writtenAtMs": NOW - 2_000_000, "seq": 18, "t3Thread": "t1",
                "summary": "Sweeping."
            })]
        }))
    };
    let mut said_yes = base();
    said_yes.statuses[0].looping = Some(
        serde_json::from_value(json!({
            "state": "yes", "text": "F-07: 5 fix rounds, same finding"
        }))
        .unwrap(),
    );
    let update = one_update(&said_yes);
    assert_eq!(update.looping.answer, "yes");
    assert_eq!(update.looping.text, "Yes. F-07: 5 fix rounds, same finding");

    let mut said_no = base();
    said_no.statuses[0].looping = Some(serde_json::from_value(json!({ "state": "no" })).unwrap());
    assert_eq!(one_update(&said_no).looping.text, "No.");

    // No board and no answer: can't tell.
    assert_eq!(one_update(&base()).looping.text, "Can't tell.");
}

#[test]
fn a_machine_that_could_not_be_read_is_named_with_its_reason() {
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "machineProblems": [
            { "machine": "mac", "code": "did_not_answer" },
            { "machine": "windows", "code": "shape" },
            { "machine": "nas1", "code": "io" }
        ]
    }));
    let view = compose(&built);
    assert_eq!(view.machine_problems.len(), 3);
    assert_eq!(
        view.machine_problems[0].text,
        "Pinned threads unavailable on the Mac: it did not answer."
    );
    assert_eq!(
        view.machine_problems[1].text,
        "Pinned threads unavailable on Windows: T3's state file changed shape."
    );
    assert_eq!(
        view.machine_problems[2].text,
        "Pinned threads unavailable on Nas1: T3's state file could not be read."
    );
    assert_eq!(view.machine_problems[0].label, "the Mac");
}

#[test]
fn the_head_counts_looping_and_waiting_pins() {
    let built = input(json!({
        "pins": [pin("t1", json!("a")), pin("t2", json!("b"))],
        "threads": [
            thread(json!({ "t3Thread": "t1", "project": "pm7", "stream": "f",
                "blockedOn": { "kind": "owner", "what": "Pick reviewers", "since": null } })),
            thread(json!({ "t3Thread": "t2", "project": "pm7", "stream": "g" }))
        ],
        "tasks": [
            json!({
                "writer": "vm.pm7-build", "writtenAtMs": NOW - 100_000, "seq": 40,
                "project": "pm7", "stream": "g",
                "items": [ { "id": "F-07", "title": "Tray icon", "state": "fixing",
                    "reviews": [ { "atMs": NOW - 600_000, "verdict": "fail" },
                                 { "atMs": NOW - 300_000, "verdict": "fail" },
                                 { "atMs": NOW - 100_000, "verdict": "fail" } ] } ]
            })
        ]
    }));
    let view = compose(&built);
    assert_eq!(view.head, "2 pinned in T3 · 1 looping · 1 waiting on you");
    assert_eq!(view.counts.pinned, 2);
}

#[test]
fn age_words_use_plain_units() {
    let at = |ms| {
        let built = input(json!({
            "pins": [pin("t1", json!("a"))],
            "threads": [thread(json!({ "t3Thread": "t1", "writtenAtMs": NOW - ms }))]
        }));
        one_update(&built).updated.as_ref().unwrap().text.clone()
    };
    assert_eq!(at(30_000), "Updated just now");
    assert_eq!(at(6 * 60_000), "Updated 6 min ago");
    assert_eq!(at(3 * 60 * 60 * 1000), "Updated 3 hours ago");
    assert_eq!(at(60 * 60 * 1000), "Updated 1 hour ago");
    assert_eq!(at(2 * 24 * 60 * 60 * 1000), "Updated 2 days ago");
}

#[test]
fn dropped_items_never_count_against_the_percent_or_the_left_list() {
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({ "t3Thread": "t1", "project": "pm7", "stream": "f" }))],
        "tasks": [json!({
            "writer": "vm.pm7-build", "writtenAtMs": NOW - 100_000, "seq": 40,
            "project": "pm7", "stream": "f",
            "items": [
                { "id": "F-01", "title": "a", "state": "done" },
                { "id": "F-02", "title": "b", "state": "dropped" },
                { "id": "F-03", "title": "c", "state": "dropped" },
                { "id": "F-04", "title": "d", "state": "todo" }
            ]
        })]
    }));
    let update = one_update(&built);
    assert_eq!(update.percent.as_ref().unwrap().text, "50%");
    assert_eq!(update.percent.as_ref().unwrap().of, Some(2));
    let left: Vec<&str> = update.left.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(left, ["F-04"]);
}

#[test]
fn two_threads_of_one_pin_uses_the_newest_report() {
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [
            thread(json!({ "t3Thread": "t1", "writtenAtMs": NOW - 500_000,
                           "doing": "Old doing." })),
            thread(json!({ "t3Thread": "t1", "writtenAtMs": NOW - 60_000,
                           "doing": "New doing." }))
        ]
    }));
    assert_eq!(one_update(&built).now.as_deref(), Some("New doing."));
}

#[test]
fn a_task_board_that_cannot_be_read_says_so_beside_its_counts() {
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({ "t3Thread": "t1", "project": "pm7", "stream": "f" }))],
        "tasks": [json!({
            "writer": "vm.pm7-build", "writtenAtMs": NOW - 100_000, "seq": 40,
            "project": "pm7", "stream": "f", "problem": "its content is not valid",
            "items": [ { "id": "F-01", "title": "a", "state": "done" },
                       { "id": "F-02", "title": "b", "state": "todo" } ]
        })]
    }));
    let update = one_update(&built);
    assert_eq!(update.percent.as_ref().unwrap().text, "50%");
    assert_eq!(
        update.percent_note.as_deref(),
        Some("Its task list could not be read; these counts are its last good copy.")
    );
}
