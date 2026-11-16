//! Correction chains shared by the history store and the native Scoreboard composer.
use serde_json::{Value as V, json};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Effective rows and fixed, displayable refusals, keyed by run id.
pub struct Resolution {
    pub rows: Vec<V>,
    pub refused: BTreeMap<String, String>,
}
fn s(v: &V) -> &str {
    v.as_str().unwrap_or("")
}

/// UTC whole seconds and the complete accepted fraction. Epoch providers supply only the
/// whole-second normalization; no sub-millisecond information is taken from their millisecond values.
fn written_at_parts(value: &V, epoch: &impl Fn(&V) -> Option<i64>) -> Option<(i64, u32)> {
    let text = value.as_str()?;
    let nanos = if let Some((_, suffix)) = text.split_once('.') {
        let digits: String = suffix.chars().take_while(char::is_ascii_digit).collect();
        if !(1..=9).contains(&digits.len()) {
            return None;
        }
        digits.parse::<u32>().ok()? * 10_u32.pow(9 - digits.len() as u32)
    } else {
        0
    };
    Some((epoch(value)?.div_euclid(1000), nanos))
}

/// Resolve complete, deduplicated history before a caller applies its filters.
pub fn resolve(records: &[V], epoch: impl Fn(&V) -> Option<i64>) -> Resolution {
    let known: HashMap<&str, &V> = records.iter().map(|r| (s(&r["doc"]["id"]), r)).collect();
    let mut refused = BTreeMap::<String, String>::new();
    let mut checked = HashSet::<String>::new();
    for id in known.keys() {
        let mut walk = Vec::<String>::new();
        let mut positions = HashMap::new();
        let mut at = Some((*id).to_owned());
        while let Some(id) = at {
            if checked.contains(&id) {
                break;
            }
            if let Some(pos) = positions.get(&id) {
                for member in &walk[*pos..] {
                    refused.insert(member.clone(), "corrects forms a cycle".into());
                }
                break;
            }
            positions.insert(id.clone(), walk.len());
            walk.push(id.clone());
            let doc = &known[id.as_str()]["doc"];
            let Some(target) = doc["corrects"].as_str() else {
                break;
            };
            if !target.starts_with(&format!("{}--", s(&doc["writer"]))) {
                refused.insert(id, "corrects names another writer's run".into());
                break;
            }
            if !known.contains_key(target) {
                refused.insert(id, "corrects names a missing run".into());
                break;
            }
            at = Some(target.to_owned());
        }
        for member in walk.into_iter().rev() {
            let target = s(&known[member.as_str()]["doc"]["corrects"]);
            if !refused.contains_key(&member) && refused.contains_key(target) {
                refused.insert(member.clone(), "corrects names a refused correction".into());
            }
            checked.insert(member);
        }
    }
    let mut successors = HashMap::<String, String>::new();
    let times: HashMap<_, _> = records
        .iter()
        .map(|r| (s(&r["doc"]["id"]), written_at_parts(&r["doc"]["writtenAt"], &epoch)))
        .collect();
    for record in records {
        let doc = &record["doc"];
        let id = s(&doc["id"]);
        let Some(target) = doc["corrects"].as_str() else {
            continue;
        };
        if refused.contains_key(id) {
            continue;
        }
        if let Some(held) = successors.get(target) {
            let old = &known[held.as_str()]["doc"];
            let newer = (times.get(id), doc["seq"].as_u64(), id)
                > (times.get(held.as_str()), old["seq"].as_u64(), held.as_str());
            let loser = if newer { held.as_str() } else { id };
            refused.insert(loser.to_owned(), "corrects conflict: a later correction wins".into());
            if !newer {
                continue;
            }
        }
        successors.insert(target.to_owned(), id.to_owned());
    }
    checked.clear();
    for id in known.keys() {
        let mut walk = Vec::new();
        let mut at = Some((*id).to_owned());
        while let Some(id) = at {
            if checked.contains(&id) || refused.contains_key(&id) {
                break;
            }
            walk.push(id.clone());
            at = known[id.as_str()]["doc"]["corrects"].as_str().map(str::to_owned);
        }
        for member in walk.into_iter().rev() {
            let target = s(&known[member.as_str()]["doc"]["corrects"]);
            if refused.contains_key(target) {
                refused.insert(member.clone(), "corrects names a refused correction".into());
            }
            checked.insert(member);
        }
    }
    let mut latest = HashMap::<String, String>::new();
    for id in known.keys() {
        if refused.contains_key(*id) {
            continue;
        }
        let mut walk = Vec::new();
        let mut at = (*id).to_owned();
        while !latest.contains_key(&at) {
            walk.push(at.clone());
            let Some(next) = successors.get(&at).filter(|next| !refused.contains_key(*next)) else {
                break;
            };
            at = next.clone();
        }
        let leaf = latest.get(&at).unwrap_or(&at).clone();
        for member in walk {
            latest.insert(member, leaf.clone());
        }
    }
    let mut voided: HashSet<String> = latest
        .iter()
        .filter(|(_, leaf)| known[leaf.as_str()]["doc"]["void"].is_object())
        .map(|(id, _)| id.clone())
        .collect();
    let mut reviewing = HashMap::<String, Vec<String>>::new();
    for record in records {
        let doc = &record["doc"];
        let id = s(&doc["id"]);
        if !doc["review"].is_object() || latest.get(id).map(String::as_str) != Some(id) {
            continue;
        }
        let target = s(&doc["review"]["of"]);
        reviewing
            .entry(latest.get(target).map(String::as_str).unwrap_or(target).to_owned())
            .or_default()
            .push(id.to_owned());
    }
    let mut queue: Vec<_> = voided.iter().cloned().collect();
    let mut pos = 0;
    while pos < queue.len() {
        for id in reviewing.get(&queue[pos]).into_iter().flatten() {
            if voided.insert(id.clone()) {
                queue.push(id.clone());
            }
        }
        pos += 1;
    }
    let rows = records
        .iter()
        .filter_map(|record| {
            let id = s(&record["doc"]["id"]);
            if latest.get(id).map(String::as_str) != Some(id) || voided.contains(id) {
                return None;
            }
            let mut row = record.clone();
            if row["doc"]["review"].is_object() {
                let target = s(&row["doc"]["review"]["of"]);
                if let Some(leaf) = latest.get(target) {
                    row["doc"]["review"]["of"] = json!(leaf);
                }
            }
            Some(row)
        })
        .collect();
    Resolution { rows, refused }
}

/// Resolve a RunsState envelope once, retaining source problems for the dashboard.
pub fn resolve_state(state: &mut V, epoch: impl Fn(&V) -> Option<i64>) -> Option<()> {
    if state["correctionsResolved"] == true {
        return Some(());
    }
    let records = state["runs"].as_array()?;
    let result = resolve(records, epoch);
    let problems: Vec<_> = records
        .iter()
        .filter_map(|r| {
            result
                .refused
                .get(s(&r["doc"]["id"]))
                .map(|reason| json!({"file":r["file"],"reason":reason,"since":r["firstSeenAt"]}))
        })
        .collect();
    state["problems"].as_array_mut()?.extend(problems);
    state["runs"] = json!(result.rows);
    state["correctionsResolved"] = json!(true);
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_fixture_agrees_with_typescript() {
        let fixture: V = serde_json::from_str(include_str!("../../tests/fixtures/run-corrections.json")).unwrap();
        let records: Vec<_> = fixture["runs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|doc| json!({"doc":doc}))
            .collect();
        // The fixture writes minute-separated UTC times. Epoch precision is exercised by the store suite.
        let result = resolve(&records, |v| {
            v.as_str().map(|v| if v.contains("16:01") { 60_000 } else { 0 })
        });
        let mut ids: Vec<_> = result.rows.iter().map(|r| r["doc"]["id"].clone()).collect();
        ids.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        assert_eq!(json!(ids), fixture["ids"]);
        for (id, reason) in fixture["refusals"].as_object().unwrap() {
            assert!(result.refused[&format!("vm.corrections--{id}")].contains(reason.as_str().unwrap()));
        }
        assert!(
            result
                .rows
                .iter()
                .any(|r| r["doc"]["review"]["of"] == "vm.corrections--build-new")
        );
    }
}
