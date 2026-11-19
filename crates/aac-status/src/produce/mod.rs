//! G16c: the Status feed's producers, moved out of TypeScript under the owned Status group.
//!
//! The TypeScript `StatusService` built the six fed generations from its own collectors and
//! its own `Intl`: pinned threads, the machine, browser and Simlock readings, the usage
//! sample, the answer history, and the presentation facts (dates, epochs and collation
//! ranks). This module is the Rust half of each, as pure functions over the readings another
//! layer (the service's command runner) hands in, so the fixtures can drive the same inputs
//! through both sides:
//!
//! - [`facts`]: `readFacts` + `localeRanks` + `dateWordsIn` over the six sources, with the
//!   display zone's offsets supplied as facts (the engine's `Intl` is the only part a
//!   server cannot reproduce; every word is computed here);
//! - [`browsers`]: `parseBrowserSlots` / `mergeBrowserRows` / `sessionUrl`;
//! - [`simlock`]: `parseSimlockStatus`;
//! - [`machines`]: `parseMachineProbe`, `busyBetween`, the probe commands and the local
//!   reading's shape;
//! - [`answered`]: `resolvedCardWaits` / `activeBlocker` / `closingCardVersions` as the feed
//!   producer needs them for the reports and pinned panels;
//! - [`pinned_input`]: the `composePinned` input the TypeScript collector built from the
//!   reports, answers and status documents;
//! - [`answers_feed`]: `mergeSharedAnswers` and the answer-history feed members.
//!
//! Everything here is synchronous and reads nothing: the service runs it on its blocking
//! pool, and the parity fixtures call it directly.

pub mod answered;
pub mod answers_feed;
pub mod browsers;
pub mod facts;
pub mod machines;
pub mod pinned_input;
pub mod simlock;
pub mod sources;
pub mod usage;

use serde::{Deserialize, Serialize};

/// One named part of a source (a host, a worker, a file) that failed while the rest was read:
/// the TypeScript `ReadProblem`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadProblem {
    /// The item as shown: a host label, a worker label, a file name.
    pub item: String,
    /// Fixed words, or the tool's own short text with secrets refused.
    pub reason: String,
    /// When this reason was first seen, epoch ms.
    pub since: i64,
}

/// `nextProblem`: the same item's problem keeps its `since` while the reason stays the same.
#[must_use]
pub fn next_problem(before: Option<&ReadProblem>, item: &str, reason: &str, now: i64) -> ReadProblem {
    ReadProblem {
        item: item.to_owned(),
        reason: reason.to_owned(),
        since: if before.is_some_and(|held| held.reason == reason) {
            before.map_or(now, |held| held.since)
        } else {
            now
        },
    }
}

/// The TypeScript `Collected<T>` wrapper the feed carries: a good reading, or the last good
/// one with a named error. `missing` is written only on a failure (TS `failed`), `problems`
/// only when the source names part-failures.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Collected<T> {
    pub data: Option<T>,
    pub as_of: Option<i64>,
    pub error: Option<CollectedError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problems: Option<Vec<ReadProblem>>,
}

/// A `Collected` error: the fixed sentence and when it began.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectedError {
    pub reason: String,
    pub since: i64,
}

impl<T> Collected<T> {
    /// `empty()`: nothing read yet.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            data: None,
            as_of: None,
            error: None,
            missing: None,
            problems: None,
        }
    }

    /// `succeeded()`.
    #[must_use]
    pub fn succeeded(data: T, now: i64) -> Self {
        Self {
            data: Some(data),
            as_of: Some(now),
            error: None,
            missing: None,
            problems: None,
        }
    }

    /// `failed()`: the last good reading with its own time and the failure since it began.
    #[must_use]
    pub fn failed(&self, reason: &str, now: i64, missing: bool) -> Self
    where
        T: Clone,
    {
        Self {
            data: self.data.clone(),
            as_of: self.as_of,
            error: Some(CollectedError {
                reason: reason.to_owned(),
                since: if self.error.as_ref().is_some_and(|held| held.reason == reason) {
                    self.error.as_ref().map_or(now, |held| held.since)
                } else {
                    now
                },
            }),
            missing: Some(missing),
            problems: self.problems.clone(),
        }
    }

    /// With part-failures named (`problems: []` when none, as the collectors write it).
    #[must_use]
    pub fn with_problems(mut self, problems: Vec<ReadProblem>) -> Self {
        self.problems = Some(problems);
        self
    }
}
