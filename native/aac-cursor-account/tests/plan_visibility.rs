//! `api::empty_verdict`: what an events answer with no event at all means, by the plan meter
//! (2026-11-20 read; review wave 82, Medium 2 and Low 2).

use aac_cursor_account::api::{Empty, NOW_SKEW_MS, empty_verdict};
use serde_json::{Value, json};

const DAY: i64 = 86_400_000;
const NOW: i64 = 1_795_600_000_000;

fn plan(start: Option<i64>, end: Option<i64>, total: f64) -> Value {
    json!({"billingCycleStartMs": start, "billingCycleEndMs": end, "planUsage": {"totalPercentUsed": total}})
}

#[test]
fn use_is_placed_in_the_range_only_when_the_whole_metered_span_is_inside_it() {
    let used = plan(Some(NOW - 10 * DAY), Some(NOW + 20 * DAY), 49.36);
    // 31 days through now: the cycle's use so far is all inside.
    assert_eq!(empty_verdict(Some(&used), NOW - 31 * DAY, NOW, NOW), Empty::NotVisible);
    // The server's "now" is a moment before the reader's.
    assert_eq!(
        empty_verdict(Some(&used), NOW - 31 * DAY, NOW - NOW_SKEW_MS, NOW),
        Empty::NotVisible
    );
    // A range that ends before now, or starts after the cycle, holds only part of it.
    assert_eq!(
        empty_verdict(Some(&used), NOW - 31 * DAY, NOW - 5 * DAY, NOW),
        Empty::Unverified
    );
    assert_eq!(empty_verdict(Some(&used), NOW - 5 * DAY, NOW, NOW), Empty::Unverified);
    // A finished cycle wholly inside the range.
    let past = plan(Some(NOW - 40 * DAY), Some(NOW - 10 * DAY), 3.0);
    assert_eq!(
        empty_verdict(Some(&past), NOW - 41 * DAY, NOW - 9 * DAY, NOW),
        Empty::NotVisible
    );
}

#[test]
fn a_zero_proves_no_usage_only_for_a_range_inside_the_metered_span() {
    let unused = plan(Some(NOW - 10 * DAY), Some(NOW + 20 * DAY), 0.0);
    assert_eq!(empty_verdict(Some(&unused), NOW - 5 * DAY, NOW, NOW), Empty::NoUse);
    assert_eq!(
        empty_verdict(Some(&unused), NOW - 10 * DAY, NOW - DAY, NOW),
        Empty::NoUse
    );
    // The days before the cycle are not metered.
    assert_eq!(
        empty_verdict(Some(&unused), NOW - 31 * DAY, NOW, NOW),
        Empty::Unverified
    );
}

#[test]
fn an_unreadable_or_unplaceable_meter_is_unverified() {
    assert_eq!(empty_verdict(None, NOW - 31 * DAY, NOW, NOW), Empty::Unverified);
    assert_eq!(
        empty_verdict(Some(&plan(None, None, 49.0)), NOW - 31 * DAY, NOW, NOW),
        Empty::Unverified
    );
    let no_percent = json!({"billingCycleStartMs": NOW - DAY, "planUsage": {}});
    assert_eq!(
        empty_verdict(Some(&no_percent), NOW - 31 * DAY, NOW, NOW),
        Empty::Unverified
    );
    assert_eq!(
        empty_verdict(
            Some(&json!({"billingCycleStartMs": NOW - DAY})),
            NOW - 31 * DAY,
            NOW,
            NOW
        ),
        Empty::Unverified
    );
    // A cycle that starts in the future cannot be placed.
    assert_eq!(
        empty_verdict(Some(&plan(Some(NOW + DAY), None, 5.0)), NOW - 31 * DAY, NOW, NOW),
        Empty::Unverified
    );
    for key in ["autoPercentUsed", "apiPercentUsed"] {
        let one = json!({"billingCycleStartMs": NOW - DAY, "planUsage": {key: 0.5}});
        assert_eq!(
            empty_verdict(Some(&one), NOW - 31 * DAY, NOW, NOW),
            Empty::NotVisible,
            "{key}"
        );
    }
}
