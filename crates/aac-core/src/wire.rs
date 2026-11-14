use crate::{Cause, Failure};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// A legacy spelling with source provenance and a shared cause, if it denotes failure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireWord {
    /// Original wire spelling; retained for exact round trips.
    pub word: String,
    /// Failure class; absent for success, progress and feature-only states.
    pub cause: Option<Cause>,
    /// Source file and declaration or assignment where this word occurs.
    pub sources: Vec<String>,
}

/// Load the generated, committed compatibility vocabulary once; fail closed on invalid data.
pub fn wire_words() -> Result<&'static [WireWord], Failure> {
    static TABLE: OnceLock<Result<Vec<WireWord>, Failure>> = OnceLock::new();
    TABLE
        .get_or_init(|| {
            serde_json::from_str(include_str!("../../../tests/spec/wire-words.json"))
                .map_err(|_| Failure::new(Cause::Bug, "invalid packaged wire table"))
        })
        .as_ref()
        .map(Vec::as_slice)
        .map_err(Clone::clone)
}

/// Resolve a legacy cause or state without discarding its original spelling.
pub fn wire_word(word: &str) -> Result<WireWord, Failure> {
    wire_words()?
        .iter()
        .find(|entry| entry.word == word)
        .cloned()
        .ok_or_else(|| Failure::new(Cause::Refused, "unknown wire word"))
}
