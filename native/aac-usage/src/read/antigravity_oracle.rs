//! The Antigravity database reader as it was before R4 (every row's blob read before any was
//! parsed), kept verbatim as the oracle the streaming reader must match row for row.
#![allow(clippy::type_complexity)]
use super::*;
use rusqlite::types::Value as Sql;

struct MetaOracle<'a> {
    model: String,
    model_id: u64,
    named: bool,
    stamp: Option<f64>,
    usages: Vec<Fields<'a>>,
}

fn metadata_oracle(blob: &[u8], step: bool) -> Result<MetaOracle<'_>, AgyError> {
    let root = fields(blob)?;
    if !step && bytes(&root, 1).is_none() {
        return Err(AgyError::Invalid);
    }
    let data = if step { root } else { nested(&root, 1)? };
    let model = if step { nested(&data, 24)? } else { data.clone() };
    let mut usages = Vec::new();
    if let Some(usage) = bytes(&data, if step { 9 } else { 4 }) {
        usages.push(fields(usage)?);
    }
    for retry in data
        .get(&(if step { 28 } else { 17 }))
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let Field::Bytes(retry) = retry else {
            return Err(AgyError::Invalid);
        };
        let retry = fields(retry)?;
        if let Some(retry_usage) = bytes(&retry, 2) {
            usages.push(fields(retry_usage)?);
        }
    }
    let primary = text(&model, if step { 12 } else { 19 })?;
    let name = if primary.is_empty() {
        text(&model, if step { 8 } else { 21 })?
    } else {
        primary
    };
    let model_id = number(&model, if step { 1 } else { 3 });
    let stamp = if step {
        timestamp(&nested(&data, 8)?).or(timestamp(&nested(&data, 1)?))
    } else {
        timestamp(&nested(&nested(&data, 9)?, 4)?)
    };
    Ok(MetaOracle {
        model: model_name(&name, model_id),
        model_id,
        named: !name.is_empty(),
        stamp,
        usages,
    })
}

type Rows = Vec<(Sql, Option<Vec<u8>>)>;
use super::DbRead;

/// T3's `readDatabase`: one short read transaction with a 100 ms busy wait; never writes.
pub(super) fn read_db(path: &Path, fallback_ms: f64, deadline: Instant) -> Result<DbRead, AgyError> {
    let closed = closed_wal(path);
    let before = if closed { identity(path) } else { None };
    let (generations, trajectory_rows, steps): (Rows, Vec<Option<Vec<u8>>>, Rows) = {
        let connection = Connection::open_with_flags(
            uri(path, closed),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(Duration::from_millis(100))?;
        connection.progress_handler(20_000, Some(move || Instant::now() >= deadline));
        connection.execute_batch("BEGIN")?;
        let tables: HashSet<String> = {
            let mut statement = connection.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<_, _>>()?
        };
        if !tables.contains("gen_metadata") && !tables.contains("steps") {
            return Err(AgyError::NotConversation);
        }
        let blob = |value: Sql| match value {
            Sql::Blob(b) => Some(b),
            _ => None,
        };
        let read_pairs = |sql: &str| -> rusqlite::Result<Rows> {
            let mut statement = connection.prepare(sql)?;
            statement
                .query_map([], |row| Ok((row.get::<_, Sql>(0)?, blob(row.get::<_, Sql>(1)?))))?
                .collect()
        };
        let generations = if tables.contains("gen_metadata") {
            read_pairs("SELECT idx, data FROM gen_metadata ORDER BY idx")?
        } else {
            Vec::new()
        };
        let trajectory: Vec<Option<Vec<u8>>> = if tables.contains("trajectory_metadata_blob") {
            let mut statement = connection.prepare("SELECT data FROM trajectory_metadata_blob")?;
            statement
                .query_map([], |row| Ok(blob(row.get::<_, Sql>(0)?)))?
                .collect::<Result<_, _>>()?
        } else {
            Vec::new()
        };
        let steps = if tables.contains("steps") {
            read_pairs("SELECT idx, metadata FROM steps WHERE metadata IS NOT NULL ORDER BY idx")?
        } else {
            Vec::new()
        };
        (generations, trajectory, steps)
    };
    if closed && (wal_exists(path) || identity(path) != before) {
        return Err(AgyError::Retry);
    }
    let generations = entries(&generations, false, deadline)?;
    let mut trajectory = None;
    for data in &trajectory_rows {
        if trajectory.is_none() {
            let Some(data) = data else {
                return Err(AgyError::Invalid);
            };
            trajectory = timestamp(&nested(&fields(data)?, 2)?);
        }
    }
    let steps = entries(&steps, true, deadline)?;
    let mut names = Vec::new();
    for (_, meta) in steps.iter().chain(generations.iter()) {
        if meta.named && meta.model_id > 0 {
            names.push((meta.model_id, meta.model.clone()));
        }
    }
    let positional: HashMap<u64, String> = generations
        .iter()
        .map(|(idx, m)| (idx.to_bits(), m.model.clone()))
        .collect();
    let mut candidates = Vec::new();
    for (step, listed) in [(true, &steps), (false, &generations)] {
        for (idx, meta) in listed {
            for usage in &meta.usages {
                let output = number(usage, 3).max(number(usage, 9) + number(usage, 10));
                let tokens = [number(usage, 2), number(usage, 5), number(usage, 4), output];
                if tokens.iter().sum::<u64>() == 0 {
                    continue;
                }
                let mut keys = Vec::new();
                for key in [11u64, 12, 7] {
                    let value = text(usage, key)?;
                    if !value.is_empty() {
                        keys.push(format!("antigravity:{key}:{value}"));
                    }
                }
                let usage_id = number(usage, 1);
                let positional_model = if step {
                    positional.get(&idx.to_bits()).cloned().unwrap_or_default()
                } else {
                    String::new()
                };
                let mut t3_model = model_by_id(usage_id).map(str::to_string).unwrap_or_default();
                for fallback in [
                    meta.model.clone(),
                    positional_model,
                    model_name("", usage_id),
                    AGY_UNKNOWN.to_string(),
                ] {
                    if !t3_model.is_empty() {
                        break;
                    }
                    t3_model = fallback;
                }
                let static_model = model_by_id(usage_id)
                    .map(str::to_string)
                    .unwrap_or_else(|| if meta.named { meta.model.clone() } else { String::new() });
                candidates.push(Candidate {
                    keys,
                    stamp: meta.stamp.or(trajectory).unwrap_or(fallback_ms),
                    quality: if meta.stamp.is_some() {
                        2
                    } else if trajectory.is_some() {
                        1
                    } else {
                        0
                    },
                    tokens,
                    t3_model,
                    static_model,
                    ids: (usage_id, meta.model_id),
                });
            }
        }
    }
    Ok((candidates, names))
}

fn entries(rows: &Rows, step: bool, deadline: Instant) -> Result<Vec<(f64, MetaOracle<'_>)>, AgyError> {
    let mut found = Vec::new();
    for (index, (idx, data)) in rows.iter().enumerate() {
        if index % 256 == 255 && Instant::now() >= deadline {
            return Err(AgyError::Deadline);
        }
        let idx = match idx {
            Sql::Integer(v) => *v as f64,
            Sql::Real(v) => *v,
            _ => return Err(AgyError::Invalid),
        };
        let Some(data) = data else {
            return Err(AgyError::Invalid);
        };
        found.push((idx, metadata_oracle(data, step)?));
    }
    Ok(found)
}
