//! Conservative HTTP view assembly over the existing collectors' immutable read models.
//! Source acquisition, last-good caches and secret screening retain their existing owners.
//! The front door compares each candidate against its TS answer before admitting it.
use serde_json::{Value as V, json};

fn rows<'a>(value: &'a V, key: &str) -> Option<&'a Vec<V>> {
    value.get(key)?.as_array()
}
fn count(value: &V, field: &str) -> Option<u64> {
    value.get(field)?.as_u64().filter(|n| *n <= 1_000_000)
}
fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}
fn owner(update: &V) -> Option<String> {
    let board = &update["board"];
    if board["shared"] == true {
        let label = board["streamLabel"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| board["stream"].as_str())?;
        Some(format!("the {label} board kept by {}", board["writer"].as_str()?))
    } else {
        Some(update["title"].as_str()?.to_owned())
    }
}

/// Rebuild the Status headline from its source panels. Complex strip wording still belongs to
/// TS; it is retained verbatim until its own port, and equality is checked at the front door.
pub fn status(snapshot: &V) -> Option<V> {
    let headline = headline(snapshot)?;
    let mut result = snapshot.clone();
    result["headline"] = headline;
    Some(result)
}

/// [`status`] on a view the caller owns: the headline is set in place (R2: the copy of the whole view cost about a
/// millisecond on the large share).
pub fn status_owned(mut snapshot: V) -> Option<V> {
    snapshot["headline"] = headline(&snapshot)?;
    Some(snapshot)
}

fn headline(snapshot: &V) -> Option<V> {
    if snapshot["version"] != 1 {
        return None;
    }
    let cards = rows(&snapshot["cards"], "open")?;
    let sources = rows(snapshot, "sources")?;
    let projects = rows(snapshot, "projects")?;
    let threads = rows(&snapshot["threads"], "rows")?;
    let machines = rows(&snapshot["machines"], "rows")?;
    let warnings = rows(&snapshot["usage"], "warnings")?;
    let questions = cards
        .iter()
        .filter(|c| matches!(c["kind"].as_str(), Some("question" | "judgement" | "action")))
        .count();
    let decisions = cards.iter().filter(|c| c["kind"] == "objection").count();
    let stale = sources
        .iter()
        .filter(|s| !matches!(s["state"].as_str(), Some("ok" | "unconfigured")))
        .count();
    let working = threads
        .iter()
        .filter(|t| t["state"] == "working" && t["stale"] == false)
        .count();
    let mut blocked = 0;
    let mut total = 0;
    let mut dropped = 0;
    let mut completed = 0;
    for project in projects {
        let c = &project["counts"];
        blocked += count(c, "blocked")?;
        total += count(c, "total")?;
        dropped += count(c, "dropped")?;
        completed += count(c, "landed")? + count(c, "done")?;
    }
    let of = total.checked_sub(dropped)?;
    let quota_ok = sources.iter().any(|s| s["id"] == "quota-watch" && s["state"] == "ok");
    let over = if quota_ok {
        count(&snapshot["spend"], "overLimitBudgets")?
    } else {
        0
    };
    let mut loops: Vec<&V> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    let mut waiting = Vec::new();
    if let Some(updates) = snapshot["pinned"]["updates"].as_array() {
        for (index, update) in updates.iter().enumerate() {
            if update["blocked"]["waitingOnYou"] == true {
                waiting.push(update);
            }
            if update["looping"]["answer"] != "yes" {
                continue;
            }
            let key =
                if update["board"].is_object() && update["looping"]["task"].as_str().is_some_and(|s| !s.is_empty()) {
                    format!(
                        "board\n{}\n{}",
                        update["board"]["writer"].as_str()?,
                        update["looping"]["task"].as_str()?
                    )
                } else {
                    format!("pin\n{index}")
                };
            if let Some(at) = keys.iter().position(|k| k == &key) {
                if loops[at]["board"]["shared"] == true && update["board"]["shared"] != true {
                    loops[at] = update;
                }
            } else {
                keys.push(key);
                loops.push(update);
            }
        }
    }
    let mut worst: Option<&V> = None;
    for update in &loops {
        if worst.is_none_or(|w| {
            update["looping"]["rounds"].as_u64().unwrap_or(0) > w["looping"]["rounds"].as_u64().unwrap_or(0)
        }) {
            worst = Some(update);
        }
    }
    let mut runs = vec![];
    if questions > 0 {
        runs.extend([
            json!({"text":plural(questions,"question"),"strong":true,"tone":"warn"}),
            json!({"text":" for you"}),
        ]);
    } else {
        runs.push(json!({"text":"No questions for you"}));
    }
    if decisions > 0 {
        runs.extend([
            json!({"text":" · "}),
            json!({"text":plural(decisions,"decision"),"tone":"warn"}),
            json!({"text":" to look at"}),
        ]);
    }
    if let Some(worst) = worst {
        let task = worst["looping"]["task"]
            .as_str()
            .map(|t| format!(", task {t}"))
            .unwrap_or_default();
        let verb = if loops.len() == 1 { "is" } else { "are" };
        runs.extend([json!({"text":" · "}), json!({"text":format!("{} {verb} looping ({}{task})",plural(loops.len(),"pinned thread"),owner(worst)?),"strong":true,"tone":"warn"})]);
    }
    if let Some(first) = waiting.first() {
        let suffix = if waiting.len() == 1 { "s" } else { "" };
        runs.extend([json!({"text":" · "}),json!({"text":format!("{} wait{suffix} on you ({})",plural(waiting.len(),"pinned thread"),first["title"].as_str()?),"tone":"warn"})]);
    }
    if of > 0 {
        runs.push(json!({"text":format!(" · {completed} of {of} items landed")}));
    }
    if blocked > 0 {
        runs.extend([
            json!({"text":" · "}),
            json!({"text":format!("{blocked} blocked"),"tone":"warn"}),
        ]);
    }
    if over > 0 {
        let pronoun = if over == 1 { "its" } else { "their" };
        runs.extend([json!({"text":" · "}),json!({"text":format!("{} at or over {pronoun} limit (estimate)",plural(over as usize,"budget")),"tone":"bad"})]);
    }
    if stale > 0 {
        runs.extend([
            json!({"text":" · "}),
            json!({"text":format!("{} not current",plural(stale,"source")),"tone":"warn"}),
        ]);
    }
    let bad = cards.iter().any(|c| c["urgency"] == "now")
        || machines
            .iter()
            .any(|m| m["state"] == "unreachable" || m["warnings"].as_array().is_some_and(|w| !w.is_empty()))
        || warnings.iter().any(|w| w["tone"] == "bad")
        || over > 0;
    let warn = questions + decisions + stale + warnings.len() + loops.len() + waiting.len() > 0 || blocked > 0;
    Some(json!({
        "tone": if bad { "bad" } else if warn { "warn" } else { "good" },
        "runs": runs,
        "counts": {"openCards":cards.len(),"questions":questions,"decisions":decisions,"blockedItems":blocked,"workingThreads":working,"staleSources":stale,"loopingPinned":loops.len(),"waitingPinned":waiting.len()},
        "strip":snapshot.get("headline")?.get("strip")?
    }))
}

/// Recompose the existing native Scoreboard fields from the same immutable base view. When
/// the TS native reader was unavailable these fields remain absent; Rust never invents them.
pub fn scoreboard(snapshot: &V) -> Option<V> {
    if snapshot["version"] != 1 {
        return None;
    }
    snapshot.get("sources")?.as_array()?;
    snapshot.get("uses")?.as_array()?;
    snapshot.get("catalog")?.as_object()?;
    let mut result = snapshot.clone();
    if snapshot.get("byModel").is_some() {
        let input = serde_json::from_value(snapshot.clone()).ok()?;
        result["byModel"] = serde_json::to_value(crate::scoreboard::by_model(&input).ok()?).ok()?;
        if snapshot.get("groups").is_some() {
            result["groups"] = serde_json::to_value(crate::scoreboard::group_keys(&input).ok()?).ok()?;
        }
    }
    Some(result)
}
