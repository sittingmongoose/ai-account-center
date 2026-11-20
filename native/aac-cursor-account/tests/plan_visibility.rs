//! `api::plan_shows_use`: when an empty events read contradicts the plan meter (2026-11-20).

use aac_cursor_account::api::plan_shows_use;
use serde_json::json;

const WINDOW: (i64, i64) = (1_793_000_000_000, 1_795_000_000_000);

#[test]
fn a_used_plan_in_an_overlapping_cycle_shows_use() {
    let plan = |start: Option<i64>, end: Option<i64>, total: f64| {
        json!({"billingCycleStartMs": start, "billingCycleEndMs": end,
               "planUsage": {"totalPercentUsed": total}})
    };
    assert!(plan_shows_use(
        &plan(Some(1_794_000_000_000), Some(1_796_000_000_000), 49.36),
        WINDOW.0,
        WINDOW.1
    ));
    // A cycle that started before the window and is still open overlaps it.
    assert!(plan_shows_use(
        &plan(Some(1_790_000_000_000), None, 1.0),
        WINDOW.0,
        WINDOW.1
    ));
    // Unused, after the window, before the window, or a cycle without a start: no claim.
    assert!(!plan_shows_use(
        &plan(Some(1_794_000_000_000), None, 0.0),
        WINDOW.0,
        WINDOW.1
    ));
    assert!(!plan_shows_use(
        &plan(Some(1_795_000_000_001), None, 49.0),
        WINDOW.0,
        WINDOW.1
    ));
    assert!(!plan_shows_use(
        &plan(Some(1_790_000_000_000), Some(1_792_999_999_999), 49.0),
        WINDOW.0,
        WINDOW.1
    ));
    assert!(!plan_shows_use(&plan(None, None, 49.0), WINDOW.0, WINDOW.1));
}

#[test]
fn any_reported_percentage_counts_and_an_empty_plan_does_not() {
    let start = 1_794_000_000_000i64;
    for key in ["totalPercentUsed", "autoPercentUsed", "apiPercentUsed"] {
        let plan = json!({"billingCycleStartMs": start, "planUsage": {key: 0.5}});
        assert!(plan_shows_use(&plan, WINDOW.0, WINDOW.1), "{key}");
    }
    assert!(!plan_shows_use(
        &json!({"billingCycleStartMs": start, "planUsage": {}}),
        WINDOW.0,
        WINDOW.1
    ));
    assert!(!plan_shows_use(
        &json!({"billingCycleStartMs": start}),
        WINDOW.0,
        WINDOW.1
    ));
}
