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
            "t3Thread": "t1", "name": "pm7-build", "harness": "claude",
            "model": "claude-opus-5-5", "modelLabel": "Opus 5.5", "effort": "xhigh",
            "project": "puppet-master", "stream": "F",
            "doing": "Windows 10 sweep for stream F, batch 9."
        }))],
        "tasks": [json!({
            "writer": "vm.pm7-build", "writtenAtMs": NOW - 100_000, "seq": 40,
            "project": "puppet-master", "projectLabel": "Puppet Master",
            "stream": "F", "streamLabel": "PM7 follow-ups",
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
    assert_eq!(update.blocked.as_ref().unwrap().text, "None.");
    assert_eq!(update.looping.answer, "yes");
    assert_eq!(
        update.looping.text,
        "Yes. F-07 has gone back for fixes 3 times without a passing review."
    );
    // Left has the looping item first; done is newest landed first.
    let left: Vec<&str> = update
        .left
        .as_ref()
        .unwrap()
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(left, ["F-07"]);
    let done: Vec<&str> = update
        .done
        .as_ref()
        .unwrap()
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(done, ["F-08", "F-01"]);
    assert_eq!(update.updated.as_ref().unwrap().text, "Updated 3 min ago");
    assert_eq!(view.counts.looping, 1);
    assert_eq!(view.counts.waiting_on_you, 0);
}

#[test]
fn the_who_line_labels_the_project_and_the_model_or_says_what_it_knows() {
    // No board and a model the catalog does not know: the project id and the raw model
    // id show as written, never invented labels — and the bare stream letter stays out.
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({
            "t3Thread": "t1", "name": "some-thread",
            "model": "some/unknown-1", "effort": "high",
            "project": "some-project", "stream": "X"
        }))]
    }));
    assert_eq!(
        one_update(&built).who.as_deref(),
        Some("some-project · some-thread · some/unknown-1 high")
    );
    // No name at all: the board's stream label stands in for the middle part.
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({
            "t3Thread": "t1", "project": "puppet-master", "stream": "F"
        }))],
        "tasks": [json!({
            "writer": "vm.other", "writtenAtMs": NOW - 100_000, "seq": 1,
            "project": "puppet-master", "projectLabel": "Puppet Master",
            "stream": "F", "streamLabel": "PM7 follow-ups",
            "items": []
        })]
    }));
    assert_eq!(
        one_update(&built).who.as_deref(),
        Some("Puppet Master · PM7 follow-ups")
    );
}

#[test]
fn a_standalone_status_refusal_attaches_to_its_pin() {
    // F13: a pinned writer whose status.json was refused with no good copy carries the
    // refusal on its own pin, not only at panel level. The thread report matches the
    // pin by the normalised id (B8); the refusal matches the thread's writer.
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({ "t3Thread": "mcp:launch:t1", "doing": "Sweeping." }))],
        "statusProblems": [json!({
            "file": "writers/vm.pm7-build/status.json",
            "reason": "status.json looping.state is missing or not valid",
            "sinceMs": NOW - 600_000
        })]
    }));
    let view = compose(&built);
    let update = &view.updates[0];
    // The thread reports, so the pin is not "not reporting yet".
    assert_eq!(update.report_state, "fresh");
    let refusal = update.refusal.as_ref().expect("refusal on the pin");
    assert_eq!(refusal.file, "writers/vm.pm7-build/status.json");
    assert_eq!(refusal.reason, "status.json looping.state is missing or not valid");
    assert_eq!(refusal.since_ms, Some(NOW - 600_000));
    // The panel entry stays, too.
    assert_eq!(view.status_problems.len(), 1);
    assert_eq!(view.status_problems[0].file, "writers/vm.pm7-build/status.json");
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
    assert!(update.blocked.as_ref().unwrap().waiting_on_you);
    assert_eq!(
        update.blocked.as_ref().unwrap().text,
        "Waiting on you: Pick the reviewers for batch B09; Choose the icon set; \
         Say yes or no to the tray icon; Which tray icon?. Blocked: A reviewer for F-02 (waiting on review)."
    );
}

#[test]
fn only_question_judgement_and_action_cards_make_a_pin_wait_on_you() {
    // D10: an objection ("shipping tonight unless you object") and a sample card must
    // not read as "Waiting on you"; a judgement card and a kindless card must.
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({ "t3Thread": "t1" }))],
        "openCards": [
            json!({ "writer": "vm.pm7-build", "id": "obj", "seq": 1,
                    "title": "Shipping tonight unless you object.", "kind": "objection" }),
            json!({ "writer": "vm.pm7-build", "id": "sam", "seq": 1,
                    "title": "A format sample.", "kind": "sample" }),
            json!({ "writer": "vm.pm7-build", "id": "jud", "seq": 1,
                    "title": "Measure the tray at 125%?", "kind": "judgement" }),
            json!({ "writer": "vm.pm7-build", "id": "que", "seq": 1,
                    "title": "Which tray icon?" }),
        ]
    }));
    let update = one_update(&built);
    let blocked = update.blocked.as_ref().unwrap();
    assert!(blocked.waiting_on_you);
    assert_eq!(
        blocked.text,
        "Waiting on you: Measure the tray at 125%?; Which tray icon?."
    );
    // Objection and sample cards alone leave the pin without a wait.
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({ "t3Thread": "t1" }))],
        "openCards": [
            json!({ "writer": "vm.pm7-build", "id": "obj", "seq": 1,
                    "title": "Shipping tonight unless you object.", "kind": "objection" }),
            json!({ "writer": "vm.pm7-build", "id": "sam", "seq": 1,
                    "title": "A format sample.", "kind": "sample" }),
        ]
    }));
    assert_eq!(one_update(&built).blocked, None);
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
    assert_eq!(
        update.blocked.as_ref().unwrap().text,
        "Blocked; the report does not say on what."
    );
    assert!(!update.blocked.as_ref().unwrap().waiting_on_you);

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
    assert_eq!(
        update.blocked.as_ref().unwrap().text,
        "Blocked on B-1; the report does not say on what."
    );
    assert!(!update.blocked.as_ref().unwrap().waiting_on_you);

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
        update.blocked.as_ref().unwrap().text,
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
    // D8: with no report there is no invented "No task list." sentence or blocker
    // either: null, and the page says "No task lists yet".
    assert_eq!(update.percent_note, None);
    assert_eq!(update.blocked, None);
    assert_eq!(update.left, None);
    assert_eq!(update.done, None);
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
    assert_eq!(update.left.as_ref().unwrap()[0].id, "F-07");
    assert_eq!(update.done.as_ref().unwrap()[0].id, "F-08");
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
fn reviews_count_by_date_not_list_order() {
    // F11: four fails and then a pass, written newest first: by list order the pass
    // resets and the fails loop; by date the fails build and the pass clears it.
    let newest_first = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({ "t3Thread": "t1", "project": "pm7", "stream": "f" }))],
        "tasks": [json!({
            "writer": "vm.pm7-build", "writtenAtMs": NOW - 100_000, "seq": 40,
            "project": "pm7", "stream": "f",
            "items": [ { "id": "F-07", "title": "Tray icon", "state": "review",
                "reviews": [
                    { "atMs": NOW - 600_000, "verdict": "pass" },
                    { "atMs": NOW - 1_800_000, "verdict": "fail" },
                    { "atMs": NOW - 2_400_000, "verdict": "fail" },
                    { "atMs": NOW - 3_000_000, "verdict": "fail" },
                    { "atMs": NOW - 3_600_000, "verdict": "fail" }
                ] } ]
        })]
    }));
    assert_eq!(one_update(&newest_first).looping.answer, "no");
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
    // toward a loop, and since B3 it no longer resets the count either - free wording
    // between two fails must not hide the loop those fails make.
    let unclear = board(json!([
        { "atMs": NOW - 3_600_000, "verdict": "fail" },
        { "atMs": NOW - 1_800_000, "verdict": "fail" },
        { "atMs": NOW - 1_200_000, "verdict": "inconclusive: rerun the probes" },
        { "atMs": NOW - 600_000, "verdict": "fail" }
    ]));
    assert_eq!(one_update(&unclear).looping.answer, "yes");
    // Alone it is still never a loop: unclear rounds never count.
    let only_unclear = board(json!([
        { "atMs": NOW - 1_800_000, "verdict": "fail" },
        { "atMs": NOW - 1_200_000, "verdict": "inconclusive: rerun the probes" },
        { "atMs": NOW - 600_000, "verdict": "inconclusive: rerun the probes" }
    ]));
    assert_eq!(one_update(&only_unclear).looping.answer, "no");

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
fn reviews_outside_the_loop_window_do_not_loop_and_undated_ones_do() {
    // The window spans four days (B3): a daily-fail loop is still a loop, and only reviews
    // older than that stop counting.
    let outside = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({ "t3Thread": "t1", "project": "pm7", "stream": "f" }))],
        "tasks": [json!({
            "writer": "vm.pm7-build", "writtenAtMs": NOW - 100_000, "seq": 40,
            "project": "pm7", "stream": "f",
            "items": [ { "id": "F-07", "title": "Tray icon", "state": "fixing",
                "reviews": [
                    { "atMs": NOW - 6 * 24 * 60 * 60 * 1000, "verdict": "fail" },
                    { "atMs": NOW - 5 * 24 * 60 * 60 * 1000, "verdict": "fail" },
                    { "atMs": NOW - 4 * 24 * 60 * 60 * 1000 - 1, "verdict": "fail" }
                ] } ]
        })]
    }));
    // D8: with every review outside the window the board proves nothing about looping now,
    // so it says "Can't tell.", never an invented "No.".
    assert_eq!(one_update(&outside).looping.answer, "cantTell");

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
            thread(json!({ "writer": "vm.pm7-design", "t3Thread": "t1", "project": "pm7", "stream": "f",
                "blockedOn": { "kind": "owner", "what": "Pick reviewers", "since": null } })),
            thread(json!({ "writer": "vm.pm7-build", "t3Thread": "t2", "project": "pm7", "stream": "g" }))
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
    let left: Vec<&str> = update
        .left
        .as_ref()
        .unwrap()
        .iter()
        .map(|item| item.id.as_str())
        .collect();
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

#[test]
fn a_thread_waiting_on_a_question_card_is_never_stale() {
    // F4: waiting through an open question card is waiting on the owner. A waiting
    // report three hours past its ttl must not read as stale while its card is open.
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({
            "t3Thread": "t1", "state": "waiting", "ttlSeconds": 1800,
            "writtenAtMs": NOW - 3 * 60 * 60 * 1000
        }))],
        "openCards": [json!({
            "writer": "vm.pm7-build", "id": "card-1", "seq": 3,
            "title": "Which tray icon?", "kind": "question"
        })]
    }));
    let update = one_update(&built);
    assert_eq!(update.report_state, "fresh");
    let blocked = update.blocked.as_ref().unwrap();
    assert!(blocked.waiting_on_you);
    assert_eq!(blocked.text, "Waiting on you: Which tray icon?.");
    // An objection card asks nothing of the owner: the same report is stale.
    let objected = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({
            "t3Thread": "t1", "state": "waiting", "ttlSeconds": 1800,
            "writtenAtMs": NOW - 3 * 60 * 60 * 1000
        }))],
        "openCards": [json!({
            "writer": "vm.pm7-build", "id": "card-1", "seq": 3,
            "title": "Shipping tonight unless you object.", "kind": "objection"
        })]
    }));
    assert_eq!(one_update(&objected).report_state, "stale");
}

#[test]
fn a_blocked_or_waiting_state_without_a_heartbeat_is_never_stale() {
    // N4 (composer side): the documented null-ttl rule exempts blocked/waiting states.
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({
            "t3Thread": "t1", "state": "waiting", "ttlSeconds": null,
            "writtenAtMs": NOW - 3 * 24 * 60 * 60 * 1000
        }))]
    }));
    assert_eq!(one_update(&built).report_state, "fresh");
    // With a ttl set, the state alone does not exempt: the same report is stale.
    let set = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({
            "t3Thread": "t1", "state": "waiting", "ttlSeconds": 1800,
            "writtenAtMs": NOW - 3 * 24 * 60 * 60 * 1000
        }))]
    }));
    assert_eq!(one_update(&set).report_state, "stale");
}

#[test]
fn a_thread_waiting_on_the_owner_is_never_stale_and_names_the_moment() {
    // B2: a thread that ended its turn to wait on the owner cannot heartbeat; the older its
    // last report, the more the update must keep saying it waits. Stale would hide exactly
    // that, so it is never stale while the report says "waiting on you", whatever its ttl.
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({
            "t3Thread": "t1", "state": "blocked", "ttlSeconds": null,
            "writtenAtMs": NOW - 3 * 24 * 60 * 60 * 1000,
            "blockedOn": {
                "kind": "owner", "what": "Approve the redesign",
                "since": "2026-11-07T04:03:20Z", "sinceMs": NOW - 90 * 60 * 1000
            }
        }))]
    }));
    let update = one_update(&built);
    assert_eq!(update.report_state, "fresh");
    assert!(update.blocked.as_ref().unwrap().waiting_on_you);
    // NOW - 90 min reads 11:03 PM in the dashboard's zone (offset -300).
    assert_eq!(
        update.blocked.as_ref().unwrap().text,
        "Waiting on you since 11:03 PM: Approve the redesign."
    );
    // A heartbeat that lapsed while it waits says nothing either.
    let lapsed = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({
            "t3Thread": "t1", "state": "waiting", "ttlSeconds": 1800,
            "writtenAtMs": NOW - 3 * 60 * 60 * 1000,
            "blockedOn": { "kind": "owner", "what": "Pick the reviewers", "sinceMs": null }
        }))]
    }));
    let update = one_update(&lapsed);
    assert_eq!(update.report_state, "fresh");
    assert_eq!(
        update.blocked.as_ref().unwrap().text,
        "Waiting on you: Pick the reviewers."
    );
}

#[test]
fn the_loop_rule_matches_fail_wording_by_prefix() {
    // B3: reviewers here word a failing round "FAIL, one Medium"; the rule must read it as a
    // fail the way it already reads "PASS WITH NOTES" as a pass.
    let day = 24 * 60 * 60 * 1000i64;
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({ "t3Thread": "t1", "project": "pm7", "stream": "f" }))],
        "tasks": [json!({
            "writer": "vm.pm7-build", "writtenAtMs": NOW - 100_000, "seq": 40,
            "project": "pm7", "stream": "f",
            "items": [
                { "id": "S-01", "title": "Sweep", "state": "fixing", "reviews": [
                    { "atMs": NOW - 9 * 3_600_000, "verdict": "FAIL, one Medium" },
                    { "atMs": NOW - 6 * 3_600_000, "verdict": "FAIL, one Medium" },
                    { "atMs": NOW - 3 * 3_600_000, "verdict": "FAIL, one Medium" }
                ] },
                { "id": "S-02", "title": "Daily", "state": "fixing", "reviews": [
                    { "atMs": NOW - 4 * day + 3_600_000, "verdict": "fail" },
                    { "atMs": NOW - 3 * day + 3_600_000, "verdict": "fail" },
                    { "atMs": NOW - 2 * day + 3_600_000, "verdict": "fail" },
                    { "atMs": NOW - day + 3_600_000, "verdict": "fail" }
                ] },
                { "id": "S-03", "title": "Unclear", "state": "fixing", "reviews": [
                    { "atMs": NOW - 9 * 3_600_000, "verdict": "fail" },
                    { "atMs": NOW - 6 * 3_600_000, "verdict": "cannot verify on the phone" },
                    { "atMs": NOW - 3 * 3_600_000, "verdict": "fail" },
                    { "atMs": NOW - 2 * 3_600_000, "verdict": "fail" }
                ] },
                { "id": "S-04", "title": "Never a loop", "state": "fixing", "reviews": [
                    { "atMs": NOW - 9 * 3_600_000, "verdict": "cannot verify on the phone" },
                    { "atMs": NOW - 6 * 3_600_000, "verdict": "cannot verify on the phone" },
                    { "atMs": NOW - 3 * 3_600_000, "verdict": "cannot verify on the phone" },
                    { "atMs": NOW - 2 * 3_600_000, "verdict": "cannot verify on the phone" }
                ] }
            ]
        })]
    }));
    let view = compose(&built);
    let update = &view.updates[0];
    // Before the rule read these boards, every one of S-01..S-03 said "No." (an unknown
    // verdict reset the count; a day-old fail fell out of the window). Now the worst item
    // of the board loops and is named; S-04's unclear verdicts never loop on their own.
    assert_eq!(update.looping.answer, "yes");
    assert!(
        update
            .looping
            .text
            .starts_with("Yes. S-02 has gone back for fixes 4 times")
            || update
                .looping
                .text
                .starts_with("Yes. S-03 has gone back for fixes 4 times"),
        "the worst loop must say four rounds: {}",
        update.looping.text
    );
    assert_eq!(view.counts.looping, 1);
    // The prefix fail alone, on its own board: this fails without the prefix fix (F5).
    let prefix_only = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({ "t3Thread": "t1", "project": "pm7", "stream": "f" }))],
        "tasks": [json!({
            "writer": "vm.pm7-build", "writtenAtMs": NOW - 100_000, "seq": 40,
            "project": "pm7", "stream": "f",
            "items": [ { "id": "S-01", "title": "Sweep", "state": "fixing", "reviews": [
                { "atMs": NOW - 9 * 3_600_000, "verdict": "FAIL, one Medium" },
                { "atMs": NOW - 6 * 3_600_000, "verdict": "FAIL, one Medium" },
                { "atMs": NOW - 3 * 3_600_000, "verdict": "FAIL, one Medium" }
            ] } ]
        })]
    }));
    let prefix_update = one_update(&prefix_only);
    assert_eq!(prefix_update.looping.answer, "yes");
    assert_eq!(prefix_update.looping.task.as_deref(), Some("S-01"));
    // A board whose only reviews are unclear says "Can't tell", never "No.": unknown
    // verdicts prove nothing either way (F7).
    let unclear = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({ "t3Thread": "t1", "project": "pm7", "stream": "f" }))],
        "tasks": [json!({
            "writer": "vm.pm7-build", "writtenAtMs": NOW - 100_000, "seq": 40,
            "project": "pm7", "stream": "f",
            "items": [ { "id": "S-04", "title": "Never a loop", "state": "fixing", "reviews": [
                { "atMs": NOW - 9 * 3_600_000, "verdict": "cannot verify on the phone" },
                { "atMs": NOW - 6 * 3_600_000, "verdict": "cannot verify on the phone" },
                { "atMs": NOW - 3 * 3_600_000, "verdict": "cannot verify on the phone" }
            ] } ]
        })]
    }));
    let update = one_update(&unclear);
    assert_eq!(update.looping.answer, "cantTell");
}

#[test]
fn a_pinned_thread_with_only_status_json_still_gets_its_board() {
    // B8: a board used to be found only through thread@1's project and stream, so a thread
    // that reports only status.json never got one. The same writer's tasks.json is its board.
    let built = input(json!({
        "pins": [pin("mcp:launch:ba7eba5e", json!("a"))],
        "statuses": [json!({
            "writer": "vm.flash", "writtenAt": "2026-11-08T05:00:00Z",
            "writtenAtMs": NOW - 600_000, "seq": 3, "t3Thread": "mcp:launch:ba7eba5e",
            "summary": "Sweeping", "percent": 40,
            "left": [ { "id": "R-02", "title": "Second pass" } ],
            "done": [ { "id": "R-01", "title": "First pass" } ]
        })],
        "tasks": [json!({
            "writer": "vm.flash", "writtenAtMs": NOW - 300_000, "seq": 7,
            "project": "flash", "stream": "research",
            "items": [
                { "id": "R-01", "title": "First pass", "state": "done" },
                { "id": "R-02", "title": "Second pass", "state": "working" },
                { "id": "R-03", "title": "Write-up", "state": "todo" }
            ]
        })]
    }));
    let update = one_update(&built);
    assert_eq!(update.report_state, "fresh");
    // The board's own counts, not the status estimate and not "No task list.".
    assert_eq!(update.percent.as_ref().unwrap().text, "33%");
    assert_eq!(update.percent.as_ref().unwrap().done, Some(1));
    assert_eq!(update.percent.as_ref().unwrap().of, Some(3));
}

#[test]
fn the_pin_match_tolerates_the_id_forms_reports_carry() {
    // B8: the match was an exact string compare, so a report that spelled the id the way
    // T3's own links and run ids spell it read as "doesn't report its status yet".
    let hex = "320669b2e6cf23a317d2030821cc8a74289073eab144883a2c9b47acdade3c44";
    let plain = format!("mcp:launch:{hex}");
    // Percent-encoded, as run ids spell it: the thread.json matches.
    let encoded = input(json!({
        "pins": [pin(&plain, json!("a"))],
        "threads": [thread(json!({
            "t3Thread": format!("mcp%3Alaunch%3A{hex}"), "doing": "Encoded id."
        }))]
    }));
    let update = one_update(&encoded);
    assert_eq!(update.report_state, "fresh");
    assert_eq!(update.now.as_deref(), Some("Encoded id."));
    // A t3-thread:// link, as the thread panels spell it: the status document matches.
    let linked = input(json!({
        "pins": [pin(&plain, json!("a"))],
        "statuses": [json!({
            "writer": "vm.lead", "writtenAt": "2026-11-08T05:00:00Z",
            "writtenAtMs": NOW - 600_000, "seq": 2,
            "t3Thread": format!("t3-thread://v2/ef60010f-0371-4770-ad52-d8378888411b/{plain}"),
            "summary": "Linked id."
        })]
    }));
    assert_eq!(one_update(&linked).summary.as_deref(), Some("Linked id."));
    // The bare tail, and case, still match; another thread's id never does.
    let bare = input(json!({
        "pins": [pin(&plain, json!("a"))],
        "threads": [thread(json!({ "t3Thread": hex.to_uppercase(), "doing": "Bare id." }))]
    }));
    assert_eq!(one_update(&bare).now.as_deref(), Some("Bare id."));
    let other = input(json!({
        "pins": [pin(&plain, json!("a"))],
        "threads": [thread(json!({ "t3Thread": format!("mcp:launch:{hex}dead"), "doing": "Other." }))]
    }));
    let update = one_update(&other);
    assert_eq!(update.report_state, "none");
    assert_eq!(
        update.report_note.as_deref(),
        Some("This thread doesn't report its status yet.")
    );
}

#[test]
fn a_board_with_no_review_in_the_window_cannot_say_no_about_looping() {
    // D8: "Looping: No." appeared whenever a task list existed, even with no review recorded
    // anywhere. Only a review inside the window - or the thread's own answer - may say no.
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({ "t3Thread": "t1", "project": "pm7", "stream": "f" }))],
        "tasks": [json!({
            "writer": "vm.pm7-build", "writtenAtMs": NOW - 100_000, "seq": 40,
            "project": "pm7", "stream": "f",
            "items": [
                { "id": "F-01", "title": "First", "state": "todo", "reviews": [] },
                { "id": "F-02", "title": "Second", "state": "working", "reviews": [] }
            ]
        })]
    }));
    let update = one_update(&built);
    assert_eq!(update.looping.answer, "cantTell");
    assert_eq!(update.looping.text, "Can't tell.");
    // One review inside the window, and the board may answer.
    let with_review = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({ "t3Thread": "t1", "project": "pm7", "stream": "f" }))],
        "tasks": [json!({
            "writer": "vm.pm7-build", "writtenAtMs": NOW - 100_000, "seq": 41,
            "project": "pm7", "stream": "f",
            "items": [ { "id": "F-01", "title": "First", "state": "review",
                "reviews": [ { "atMs": NOW - 3_600_000, "verdict": "pass" } ] } ]
        })]
    }));
    assert_eq!(one_update(&with_review).looping.answer, "no");
}

#[test]
fn a_thread_with_no_task_list_sends_nulls_not_invented_all_clears() {
    // D8: with no task list the update carried "None." / "No task list." / empty lists, which
    // the page drew as "Nothing blocked" and "0 of 0 tasks". Null now, and the page says
    // "No task lists yet"; a real blocker still arrives with no board at all.
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({ "t3Thread": "t1", "doing": "Sweeping.", "state": "working" }))]
    }));
    let update = one_update(&built);
    assert_eq!(update.percent, None);
    assert_eq!(update.percent_note, None);
    assert_eq!(update.blocked, None);
    assert_eq!(update.left, None);
    assert_eq!(update.done, None);
    // A blocker named by the thread itself still shows without any board.
    let blocked = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({
            "t3Thread": "t1", "state": "blocked", "doing": "Waiting.",
            "blockedOn": { "kind": "owner", "what": "Approve the plan", "sinceMs": null }
        }))]
    }));
    let update = one_update(&blocked);
    assert_eq!(
        update.blocked.as_ref().unwrap().text,
        "Waiting on you: Approve the plan."
    );
    assert!(update.blocked.as_ref().unwrap().waiting_on_you);
}

#[test]
fn one_unknown_task_state_only_marks_its_row() {
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({ "t3Thread": "t1", "project": "project", "stream": "work" }))],
        "tasks": [{ "writer": "vm.pm7-build", "writtenAtMs": NOW, "seq": 1, "project": "project", "stream": "work",
            "items": [{ "id": "good", "title": "Good", "state": "done" }, { "id": "bad", "title": "Bad", "state": "queued" }] }]
    }));
    let update = one_update(&built);
    assert_eq!(update.done.unwrap().len(), 1);
    assert_eq!(update.left.unwrap()[0].title, "Bad (state not recognised: queued)");
    assert!(update.unreadable_note.is_none());
    assert!(update.refusal.is_none());
}

#[test]
fn v2_fields_reach_the_view_without_changing_existing_rendered_fields() {
    let stages = json!([{ "label": "Build", "state": "done" }, { "label": "Land", "state": "next" }]);
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({ "t3Thread": "t1", "title": "New title", "now": "New now", "next": "Land", "stages": stages }))]
    }));
    let update = serde_json::to_value(one_update(&built)).unwrap();
    assert_eq!(update["reported"]["stages"], stages);
    assert_eq!(update["reported"]["next"], "Land");
    assert_eq!(update["title"], "Thread t1");
}

#[test]
fn thread_only_progress_reaches_reported() {
    let progress = json!({ "done": 2, "total": 5, "unit": "items" });
    let built = input(json!({
        "pins": [pin("t1", json!("a"))],
        "threads": [thread(json!({ "t3Thread": "t1", "progress": progress }))]
    }));
    let update = serde_json::to_value(one_update(&built)).unwrap();
    assert_eq!(update["reported"]["progress"], progress);
}
