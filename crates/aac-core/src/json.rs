use crate::{Cause, Failure};
use serde::de::DeserializeOwned;
use std::io::Read;

/// Consume at most `max + 1` bytes, detecting growth instead of trusting metadata.
pub fn bounded_read(reader: impl Read, max: usize) -> Result<Vec<u8>, Failure> {
    let limit = max
        .checked_add(1)
        .ok_or_else(|| Failure::new(Cause::Refused, "invalid byte cap"))?;
    let mut bytes = Vec::new();
    reader.take(limit as u64).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(Failure::new(Cause::OutputTooLarge, "byte cap exceeded"));
    }
    Ok(bytes)
}

/// Parse exactly one bounded JSON document; parser errors never include source text.
pub fn bounded_json<T: DeserializeOwned>(reader: impl Read, max: usize) -> Result<T, Failure> {
    serde_json::from_slice(&bounded_read(reader, max)?)
        .map_err(|_| Failure::new(Cause::Damaged, "invalid JSON document"))
}
