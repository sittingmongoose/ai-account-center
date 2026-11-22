//! Model Scoreboard base view, measures, grouping, ranking and source explanations. Input is
//! the existing accepted reader state; no share scan, store write or provider refresh occurs.
use crate::{
    read_context::{Context, Envelope, age, array, js_whitespace, number, plural, s},
    read_measures::{Index, decisive, measures, model, model_id, outcome},
};
use aac_num::view;
use serde_json::{Value as V, json};
use std::collections::{HashMap, HashSet};

fn label(verdict: &str) -> &str {
    match verdict {
        "recommended" => "Recommended",
        "fine" => "Fine",
        "avoid" => "Avoid for this",
        _ => "Not enough data yet",
    }
}
/// The order `sort_by(newest)` gives `items` (indices; the same stable sort and comparator), with
/// each item's epoch read once instead of at every comparison. The ids are compared only on a
/// tie of epochs, exactly as `newest` compares them, so the facts it reads are the same.
fn newest_order(items: &[&V], ctx: &Context) -> Vec<usize> {
    let mut order: Vec<usize> = (0..items.len()).collect();
    if items.len() < 2 {
        return order;
    }
    let epochs: Vec<Option<i64>> = items.iter().map(|item| ctx.epoch(&item["endedAt"])).collect();
    // Each id's collation rank, read once: `ctx.compare` on two ranked, different ids is the
    // order of their ranks; anything else goes to `ctx.compare` itself (equal ids, a missing rank).
    let ids: Vec<&str> = items.iter().map(|item| s(&item["id"])).collect();
    let ranks: Vec<Option<usize>> = ids.iter().map(|id| ctx.rank(id)).collect();
    order.sort_by(|&a, &b| {
        epochs[b].cmp(&epochs[a]).then_with(|| match (ranks[a], ranks[b]) {
            (Some(x), Some(y)) if ids[a] != ids[b] => x.cmp(&y),
            _ => ctx.compare(ids[a], ids[b]),
        })
    });
    order
}
fn newest(a: &V, b: &V, ctx: &Context) -> std::cmp::Ordering {
    ctx.epoch(&b["endedAt"])
        .cmp(&ctx.epoch(&a["endedAt"]))
        .then_with(|| ctx.compare(s(&a["id"]), s(&b["id"])))
}
fn summary(run: &V, models: &[V]) -> V {
    let id = model(run);
    let model_label = if id.is_null() {
        V::Null
    } else {
        models
            .iter()
            .find(|m| m["model"] == id)
            .map(|m| m["label"].clone())
            .unwrap_or(id.clone())
    };
    json!({"id":run["id"],"writer":run["writer"],"source":run["source"],"benchmark":run["benchmark"],"project":run["project"],"stream":run["stream"],
        "task":{"id":run["task"]["id"],"type":run["task"]["type"],"title":run["task"]["title"]},"model":id,"label":model_label,"runtime":run["model"]["harness"],"effort":run["model"]["effort"],
        "process":if run["process"].is_object(){json!({"id":run["process"]["id"],"version":run["process"]["version"]})}else{V::Null},"outcome":run["outcome"],"endedAt":run["endedAt"],"wallSeconds":run["wallSeconds"],"imported":run["imported"],
        "review":if run["review"].is_object(){json!({"of":run["review"]["of"],"verdict":run["review"]["verdict"],"overturned":run["review"]["overturned"],"disputed":run["review"]["disputed"],"by":run["review"]["by"]})}else{V::Null},"flags":array(&run["flags"]).len(),"evidence":run["evidence"]})
}
fn column(runs: &[V], source: &str, benchmark: &V, task: &V, catalog: &V, index: &Index, ctx: &Context) -> Option<V> {
    let values = measures(
        runs,
        s(&task["unit"]),
        task["measures"].as_array()?,
        catalog["measures"].as_array()?,
        index,
        ctx,
    )?;
    let mut newest_runs = runs.to_vec();
    newest_runs.sort_by(|a, b| newest(a, b, ctx));
    let flags:Vec<V>=newest_runs.iter().flat_map(|run|array(&run["flags"]).into_iter().map(|f|json!({"kind":f["kind"],"run":run["id"],"detail":f["detail"],"evidence":f["evidence"],"reportedBy":f["reportedBy"]}))).collect();
    let benchmark_label = if benchmark.is_null() {
        V::Null
    } else {
        array(&catalog["benchmarks"])
            .iter()
            .find(|b| b["id"] == *benchmark)
            .map(|b| b["label"].clone())
            .unwrap_or(benchmark.clone())
    };
    Some(
        json!({"source":source,"benchmark":benchmark,"benchmarkLabel":benchmark_label,"n":runs.len(),"imported":runs.iter().filter(|r|r["imported"].is_object() || r["imported"]==true).count(),
        "reviews":runs.iter().map(|r|index.of(s(&r["id"])).iter().filter(|r|decisive(r)).count()).sum::<usize>(),"ranked":false,"rank":null,"score":null,"verdict":"not-enough-data","verdictLabel":"Not enough data yet","why":"",
        "preliminary":values.as_object()?.values().any(|v|v["preliminary"]==true),"measures":values,"flags":flags,"runs":newest_runs.iter().take(20).map(|r|summary(r,catalog["models"].as_array().map(Vec::as_slice).unwrap_or(&[]))).collect::<Vec<_>>()}),
    )
}
fn below(column: &V, min: f64) -> Option<String> {
    let n = number(&column["n"])?;
    if n < min {
        Some(format!("too little data (n = {})", view::string(n)?))
    } else if column["source"] == "observed" && column["reviews"] == 0 {
        Some(format!("no blind review yet (n = {})", view::string(n)?))
    } else {
        None
    }
}
fn reviewed(c: &V) -> f64 {
    if c["measures"].get("quality").is_some() {
        number(&c["measures"]["quality"]["per"]).unwrap_or(0.0)
    } else {
        number(&c["reviews"]).unwrap_or(0.0)
    }
}
fn review_bar(c: &V, min: f64) -> bool {
    c["source"] == "observed"
        && number(&c["measures"]["cheating"]["value"]).is_none_or(|v| v < 0.2)
        && reviewed(c) < min
}
struct Mark {
    def: V,
    weight: f64,
    mark: f64,
    value: V,
}
struct Scored {
    at: usize,
    score: f64,
    coverage: f64,
    marks: Vec<Mark>,
}
fn judge(columns: &[V], usage: &V, task: &V, catalog: &V, ctx: &Context) -> Option<Vec<V>> {
    let min = number(&catalog["minRuns"])?;
    view::string(min)?;
    for column in columns {
        view::string(number(&column["n"])?)?;
    }
    let defs = catalog["measures"].as_array()?;
    let weighted: Vec<_> = array(&task["measures"])
        .iter()
        .filter_map(|id| {
            let def = defs.iter().find(|d| d["id"] == *id)?;
            let weight = number(&usage["weights"][s(id)])?;
            (weight > 0.0).then_some((def.clone(), weight))
        })
        .collect();
    let total: f64 = weighted.iter().map(|(_, w)| w).fold(0.0, |a, b| a + b);
    let eligible: Vec<_> = columns
        .iter()
        .enumerate()
        .filter(|(_, c)| below(c, min).is_none() && !review_bar(c, min))
        .collect();
    let mut best = HashMap::new();
    for (def, _) in &weighted {
        let known: Vec<_> = eligible
            .iter()
            .filter_map(|(_, c)| number(&c["measures"][s(&def["id"])]["value"]))
            .collect();
        if let Some(value) = if def["better"] == "lower" {
            known.into_iter().reduce(f64::min)
        } else {
            known.into_iter().reduce(f64::max)
        } {
            best.insert(s(&def["id"]).to_owned(), value);
        }
    }
    let mut scored = Vec::new();
    for (at, c) in eligible {
        let mut marks = Vec::new();
        for (def, weight) in &weighted {
            let v = &c["measures"][s(&def["id"])];
            let Some(value) = number(&v["value"]) else {
                continue;
            };
            let best = best.get(s(&def["id"])).copied().unwrap_or(0.0);
            let mark = if def["unit"] == "share" {
                let v = value.clamp(0.0, 1.0);
                if def["better"] == "higher" { v } else { 1.0 - v }
            } else if def["better"] == "lower" {
                if value == 0.0 { 1.0 } else { best / value }
            } else if best == 0.0 {
                1.0
            } else {
                value / best
            };
            marks.push(Mark {
                def: def.clone(),
                weight: *weight,
                mark,
                value: v.clone(),
            });
        }
        let known: f64 = marks.iter().map(|m| m.weight).sum();
        if known > 0.0 {
            scored.push(Scored {
                at,
                coverage: known / total,
                score: marks.iter().map(|m| m.mark * m.weight).fold(0.0, |a, b| a + b) / known,
                marks,
            });
        }
    }
    scored.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| {
                number(&columns[b.at]["n"])
                    .partial_cmp(&number(&columns[a.at]["n"]))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| ctx.compare(s(&columns[a.at]["runs"][0]["id"]), s(&columns[b.at]["runs"][0]["id"])))
    });
    let top = scored.first().map(|e| e.score).unwrap_or(0.0);
    let mut out = columns.to_vec();
    for (at, c) in out.iter_mut().enumerate() {
        let why = if let Some(why) = below(c, min) {
            why
        } else if review_bar(c, min) {
            format!(
                "too few blind reviews yet ({} of {} needed, n = {})",
                view::string(reviewed(c))?,
                view::string(min)?,
                view::string(number(&c["n"]).unwrap_or(0.0))?
            )
        } else {
            format!(
                "None of this use's measures is known yet (n = {}).",
                view::string(number(&c["n"]).unwrap_or(0.0))?
            )
        };
        c["why"] = json!(why);
        c["ranked"] = json!(false);
        c["rank"] = V::Null;
        c["score"] = V::Null;
        c["verdict"] = json!("not-enough-data");
        c["verdictLabel"] = json!("Not enough data yet");
        let _ = at;
    }
    for (position, e) in scored.iter().enumerate() {
        let c = &mut out[e.at];
        let n = view::string(number(&c["n"])?)?;
        let cheating = e.marks.iter().find(|m| m.def["id"] == "cheating");
        let weakest = e.marks.iter().min_by(|a, b| a.mark.total_cmp(&b.mark))?;
        let heavy = e
            .marks
            .iter()
            .filter(|m| m.def["unit"] == "share" && m.weight >= 3.0 && m.mark < 0.5)
            .min_by(|a, b| a.mark.total_cmp(&b.mark));
        let (verdict, mut why) =
            if let Some(m) = cheating.filter(|m| number(&m.value["value"]).is_some_and(|v| v >= 0.2)) {
                (
                    "avoid",
                    format!(
                        "Flagged for cheating in {} of {} runs (n = {n}).",
                        view::string(number(&m.value["of"])?)?,
                        view::string(number(&m.value["per"])?)?
                    ),
                )
            } else if let Some(m) = heavy {
                (
                    "avoid",
                    format!(
                        "{} is {}% (n = {}).",
                        s(&m.def["label"]),
                        view::round_word(number(&m.value["value"]).unwrap_or(0.0) * 100.0)?,
                        view::string(number(&m.value["n"])?)?
                    ),
                )
            } else if e.score < 0.6 {
                (
                    "avoid",
                    format!(
                        "Scores {} against {} for the best row, weakest on {} (n = {n}).",
                        view::string(view::precision(e.score, 2)?)?,
                        view::string(view::precision(top, 2)?)?,
                        s(&weakest.def["label"])
                    ),
                )
            } else if scored.len() < 2 {
                (
                    "fine",
                    format!("The only ranked row here, so nothing to compare it with (n = {n})."),
                )
            } else if e.coverage < 0.5 {
                ("fine", format!("Too few measures known to recommend it (n = {n})."))
            } else if e.score >= 0.8 && e.score >= top - 0.05 {
                let praised = e
                    .marks
                    .iter()
                    .filter(|m| m.def["better"] == "higher" || m.def["unit"] != "share")
                    .min_by(|a, b| (b.mark * b.weight).total_cmp(&(a.mark * a.weight)));
                let clean = if cheating.is_some_and(|m| number(&m.value["value"]) == Some(0.0)) {
                    ", with no cheating flags"
                } else {
                    ""
                };
                (
                    "recommended",
                    format!(
                        "{}{clean} (n = {n}).",
                        praised
                            .map(|m| format!("Strongest on {}", s(&m.def["label"])))
                            .unwrap_or_else(|| "Top marks".to_owned())
                    ),
                )
            } else {
                (
                    "fine",
                    format!("Behind the top row, weakest on {} (n = {n}).", s(&weakest.def["label"])),
                )
            };
        if e.marks.iter().any(|m| m.value["preliminary"] == true) {
            why.push_str(" Preliminary.");
        }
        c["ranked"] = json!(true);
        c["rank"] = json!(position + 1);
        c["score"] = json!(view::precision(e.score, 3)?);
        c["verdict"] = json!(verdict);
        c["verdictLabel"] = json!(label(verdict));
        c["why"] = json!(why);
        c["runs"] = json!(array(&c["runs"]).into_iter().take(5).collect::<Vec<_>>());
    }
    Some(out)
}
fn traced_run_problem(files: &[&str], problems: &[V], now: i64, ctx: &Context) -> Option<(V, V)> {
    let mut matched = false;
    let mut earliest: Option<i64> = None;
    for problem in problems {
        let p_file = s(&problem["file"]);
        if !p_file.starts_with("writers/") {
            continue;
        }
        let hits = files
            .iter()
            .any(|&file| file == p_file || file.strip_prefix(p_file).is_some_and(|rest| rest.starts_with('/')));
        if !hits {
            continue;
        }
        matched = true;
        if let Some(since) = ctx.epoch(&problem["since"])
            && earliest.is_none_or(|prev| since < prev)
        {
            earliest = Some(since);
        }
    }
    matched.then(|| (ctx.iso(earliest), age(now, earliest)))
}
/// The rows a retained blind-review file was applied to: a review whose file this scan could not
/// read still joins its target (the review runs keep their last good copy), so the target's row
/// carries the failure. A review that no longer joins, or a file never read, names no row.
fn review_problems(
    docs: &[&V],
    facts: &[&RowFacts],
    problems: &[V],
    index: &Index,
    ctx: &Context,
) -> HashMap<String, Option<i64>> {
    let mut out: HashMap<String, Option<i64>> = HashMap::new();
    for (at, doc) in docs.iter().enumerate() {
        if doc["review"].is_null() {
            continue;
        }
        let file = facts[at].file.as_str();
        if !file.starts_with("writers/") {
            continue;
        }
        let Some(problem) = problems.iter().find(|p| s(&p["file"]) == file) else {
            continue;
        };
        let target = s(&doc["review"]["of"]);
        if target.is_empty() || !index.of(target).iter().any(|r| r["id"] == doc["id"]) {
            continue;
        }
        let since = ctx.epoch(&problem["since"]);
        let slot = out.entry(target.to_owned()).or_insert(None);
        if let Some(since) = since
            && slot.is_none_or(|prev| since < prev)
        {
            *slot = Some(since);
        }
    }
    out
}
/// A ranked verdict rests on the retained catalog definitions. When this pass could not list the
/// catalog, the verdict says so, with the last good read's time (never an invented failure time).
fn catalog_verdict_health(catalog: &V, now: i64, ctx: &Context) -> Option<(V, V, V)> {
    if catalog["state"] != "error" {
        return None;
    }
    let at = ctx.epoch(&catalog["scannedAt"]);
    Some((json!("error"), ctx.iso(at), age(now, at)))
}
#[allow(clippy::too_many_arguments)]
fn base_rows(
    task: &V,
    entries: &[(V, &str)],
    catalog: &V,
    index: &Index,
    problems: &[V],
    review_health: &HashMap<String, Option<i64>>,
    now: i64,
    ctx: &Context,
) -> Option<Vec<V>> {
    let mut grouped: Vec<(String, V, Vec<V>, Vec<&str>)> = Vec::new();
    for (run, file) in entries {
        let model = model(run);
        if model.is_null() {
            continue;
        }
        let process = if task["byProcess"] == true {
            run["process"].clone()
        } else {
            V::Null
        };
        let mut parts = vec![s(&model), s(&run["model"]["harness"]), s(&run["model"]["effort"])];
        if task["byProcess"] == true {
            parts.extend([s(&process["id"]), s(&process["version"])]);
        }
        let key = parts.join("|");
        let at = grouped.iter().position(|(id, _, _, _)| id == &key).unwrap_or_else(|| {
            grouped.push((key.clone(), run.clone(), Vec::new(), Vec::new()));
            grouped.len() - 1
        });
        grouped[at].2.push(run.clone());
        grouped[at].3.push(*file);
    }
    let mut out = Vec::new();
    for (key, first, runs, files) in grouped {
        let observed: Vec<_> = runs.iter().filter(|r| r["source"] == "observed").cloned().collect();
        let mut ids: Vec<V> = Vec::new();
        for run in runs.iter().filter(|r| r["source"] == "benchmark") {
            if !ids.contains(&run["benchmark"]) {
                ids.push(run["benchmark"].clone());
            }
        }
        ids.sort_by(|a, b| ctx.compare(s(a), s(b)));
        let mut benchmarks = Vec::new();
        for id in ids {
            benchmarks.push(column(
                &runs
                    .iter()
                    .filter(|r| r["source"] == "benchmark" && r["benchmark"] == id)
                    .cloned()
                    .collect::<Vec<_>>(),
                "benchmark",
                &id,
                task,
                catalog,
                index,
                ctx,
            )?);
        }
        let mut row = json!({"key":key,"model":model(&first),"runtime":first["model"]["harness"],"effort":first["model"]["effort"],"process":if task["byProcess"]==true&&first["process"].is_object(){json!({"id":first["process"]["id"],"version":first["process"]["version"]})}else{V::Null},"observed":if observed.is_empty(){V::Null}else{column(&observed,"observed",&V::Null,task,catalog,index,ctx)?},"benchmarks":benchmarks});
        if let Some((as_of, age_seconds)) = traced_run_problem(&files, problems, now, ctx)
            .or_else(|| {
                let mut earliest: Option<i64> = None;
                let mut matched = false;
                for run in &runs {
                    let Some(since) = review_health.get(s(&run["id"])) else {
                        continue;
                    };
                    matched = true;
                    if let Some(since) = *since
                        && earliest.is_none_or(|prev| since < prev)
                    {
                        earliest = Some(since);
                    }
                }
                matched.then(|| (ctx.iso(earliest), age(now, earliest)))
            })
            && let Some(obj) = row.as_object_mut()
        {
            obj.insert("sourceState".to_owned(), json!("unreadable"));
            obj.insert("asOf".to_owned(), as_of);
            obj.insert("ageSeconds".to_owned(), age_seconds);
        }
        out.push(row);
    }
    Some(out)
}
fn judged_rows(base: &[V], task: &V, usage: &V, catalog: &V, ctx: &Context) -> Option<Vec<V>> {
    let observed: Vec<_> = base
        .iter()
        .filter(|r| r["observed"].is_object())
        .map(|r| r["observed"].clone())
        .collect();
    let observed_out = judge(&observed, usage, task, catalog, ctx)?;
    let mut oi = 0;
    let mut benchmark_out: Vec<(usize, usize, V)> = Vec::new();
    let mut ids = Vec::new();
    for row in base {
        for c in array(&row["benchmarks"]) {
            if !ids.contains(&c["benchmark"]) {
                ids.push(c["benchmark"].clone());
            }
        }
    }
    for id in ids {
        let mut columns = Vec::new();
        let mut positions = Vec::new();
        for (ri, row) in base.iter().enumerate() {
            for (ci, c) in array(&row["benchmarks"]).iter().enumerate() {
                if c["benchmark"] == id {
                    columns.push(c.clone());
                    positions.push((ri, ci));
                }
            }
        }
        for ((ri, ci), out) in positions.into_iter().zip(judge(&columns, usage, task, catalog, ctx)?) {
            benchmark_out.push((ri, ci, out));
        }
    }
    let mut rows = Vec::new();
    for (ri, base) in base.iter().enumerate() {
        let observed = if base["observed"].is_object() {
            let v = observed_out.get(oi)?.clone();
            oi += 1;
            v
        } else {
            V::Null
        };
        let benchmarks: Vec<_> = array(&base["benchmarks"])
            .iter()
            .enumerate()
            .map(|(ci, c)| {
                benchmark_out
                    .iter()
                    .find(|(r, i, _)| *r == ri && *i == ci)
                    .map(|(_, _, v)| v.clone())
                    .unwrap_or(c.clone())
            })
            .collect();
        let headline = if observed["ranked"] == true {
            Some(&observed)
        } else {
            benchmarks.iter().find(|c| c["ranked"] == true)
        };
        let mut choices: Vec<_> = std::iter::once(&observed)
            .filter(|v| v.is_object())
            .chain(benchmarks.iter())
            .collect();
        choices.sort_by(|a, b| {
            number(&b["n"])
                .partial_cmp(&number(&a["n"]))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let shown = headline.or_else(|| choices.first().copied())?;
        let why = if headline.is_some_and(|h| h["source"] == "benchmark") {
            format!(
                "In the benchmark {}: {}",
                headline
                    .and_then(|h| h["benchmarkLabel"].as_str())
                    .unwrap_or("with no id"),
                s(&shown["why"])
            )
        } else {
            s(&shown["why"]).to_owned()
        };
        let catalog_model = array(&catalog["models"])
            .into_iter()
            .find(|m| m["model"] == base["model"]);
        let mut row = json!({"key":base["key"],"model":base["model"],"label":catalog_model.as_ref().map(|m|m["label"].clone()).unwrap_or(base["model"].clone()),"inCatalog":catalog_model.is_some(),"runtime":base["runtime"],"effort":base["effort"],"process":base["process"],"n":{"observed":observed["n"].as_u64().unwrap_or(0),"benchmark":benchmarks.iter().filter_map(|c|c["n"].as_u64()).sum::<u64>()},"basis":headline.map(|h|h["source"].clone()),"benchmark":headline.map(|h|h["benchmark"].clone()),"ranked":headline.is_some(),"rank":headline.map(|h|h["rank"].clone()),"verdict":shown["verdict"],"verdictLabel":shown["verdictLabel"],"why":why,"observed":observed,"benchmarks":benchmarks});
        if let Some(source_state) = base.get("sourceState")
            && let Some(obj) = row.as_object_mut()
        {
            obj.insert("sourceState".to_owned(), source_state.clone());
            obj.insert("asOf".to_owned(), base.get("asOf").cloned().unwrap_or(V::Null));
            obj.insert(
                "ageSeconds".to_owned(),
                base.get("ageSeconds").cloned().unwrap_or(V::Null),
            );
        }
        rows.push(row);
    }
    let order = |r: &V| {
        if r["basis"] == "observed" {
            0
        } else if r["basis"] == "benchmark" {
            1
        } else {
            2
        }
    };
    rows.sort_by(|a, b| {
        order(a)
            .cmp(&order(b))
            .then_with(|| {
                number(&a["rank"])
                    .unwrap_or(0.0)
                    .total_cmp(&number(&b["rank"]).unwrap_or(0.0))
            })
            .then_with(|| {
                (number(&b["n"]["observed"]).unwrap_or(0.0) + number(&b["n"]["benchmark"]).unwrap_or(0.0)).total_cmp(
                    &(number(&a["n"]["observed"]).unwrap_or(0.0) + number(&a["n"]["benchmark"]).unwrap_or(0.0)),
                )
            })
            .then_with(|| ctx.compare(s(&a["key"]), s(&b["key"])))
    });
    Some(rows)
}
fn problem_list(problems: &[V]) -> String {
    let mut v: Vec<_> = problems
        .iter()
        .take(5)
        .map(|p| {
            format!(
                "{} ({})",
                s(&p["file"]).strip_prefix("writers/").unwrap_or(s(&p["file"])),
                s(&p["reason"])
            )
        })
        .collect();
    if problems.len() > 5 {
        v.push(format!("and {} more", problems.len() - 5));
    }
    v.join(", ")
}
fn source_base(id: &str, label: &str, at: Option<i64>, now: i64, ctx: &Context) -> V {
    json!({"id":id,"label":label,"asOf":ctx.iso(at),"ageSeconds":age(now,at),"staleAfterSeconds":180})
}
fn source_finish(mut base: V, state: &str, detail: String) -> V {
    base["state"] = json!(state);
    base["detail"] = json!(detail);
    base
}
fn counted(counts: &[(String, usize)], ctx: &Context) -> String {
    let mut counts = counts.to_vec();
    counts.sort_by(|a, b| ctx.compare(&a.0, &b.0));
    counts
        .iter()
        .take(8)
        .map(|(s, n)| format!("{s} ({})", plural(*n, "run")))
        .collect::<Vec<_>>()
        .join(", ")
}
fn runs_source(input: &V, docs: &[&V], unscored: usize, index: &Index, now: i64, ctx: &Context) -> Option<V> {
    let mut source = runs_source_of(input, docs, unscored, index, now, ctx)?;
    // A snapshot taken while the scan was still running is partial, whatever its state says.
    if input["runs"]["partial"] == true {
        source["detail"] = json!(format!(
            "{} The scan is still running; this view is partial.",
            s(&source["detail"])
        ));
    }
    Some(source)
}
fn runs_source_of(input: &V, docs: &[&V], unscored: usize, index: &Index, now: i64, ctx: &Context) -> Option<V> {
    let runs = &input["runs"];
    let at = ctx.epoch(&runs["scannedAt"]);
    let base = source_base("scoreboard-runs", "Agent runs", at, now, ctx);
    if runs["state"] == "disabled" {
        return Some(source_finish(
            base,
            "missing",
            "The run history store is disabled.".into(),
        ));
    }
    let Some(at) = at else {
        return Some(source_finish(base, "missing", "Waiting for the first scan.".into()));
    };
    if runs["state"] == "missing" {
        return Some(source_finish(
            base,
            "missing",
            format!("No agent reports folder at {}.", s(&input["root"])),
        ));
    }
    if runs["state"] == "error" {
        return Some(source_finish(
            base,
            "error",
            "The agent reports folder could not be listed.".into(),
        ));
    }
    if number(&base["ageSeconds"])? > 180.0 {
        return Some(source_finish(
            base,
            "stale",
            format!("The agent reports folder has not answered since {}.", ctx.clock(at)),
        ));
    }
    let bench = docs.iter().filter(|r| r["source"] == "benchmark").count();
    let mut parts = vec![format!(
        "{} from {} read {} ({} observed, {bench} benchmark).",
        plural(docs.len(), "run"),
        plural(runs["writers"].as_u64()? as usize, "writer"),
        ctx.clock(at),
        docs.len() - bench
    )];
    if unscored > 0 {
        parts.push(format!(
            "{} with no model or task type not scored.",
            plural(unscored, "run")
        ));
    }
    if index.ignored > 0 {
        parts.push(format!(
            "{} ignored: not a review run, no earlier known run, or its own run not scored.",
            plural(index.ignored, "review")
        ));
    }
    let mut unknown = Vec::new();
    for r in docs.iter().filter(|r| outcome(r) == 0) {
        let word = s(&r["outcome"])
            .split(js_whitespace)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        bump(&mut unknown, &word);
    }
    if !unknown.is_empty() {
        parts.push(format!("{} with an outcome that is neither done nor a failure, left out of reliability, overclaim and speed and counted as no accepted units: {}.",plural(unknown.iter().map(|(_,n)|n).sum(),"run"),counted(&unknown,ctx)));
    }
    let negative: Vec<_> = docs
        .iter()
        .filter(|r| {
            number(&r["cost"]["quota"]["deltaPct"]).unwrap_or(0.0) < 0.0
                || number(&r["cost"]["estimate"]["quota"]["deltaPct"]).unwrap_or(0.0) < 0.0
        })
        .map(|r| s(&r["id"]))
        .collect();
    if !negative.is_empty() {
        parts.push(format!(
            "{} with a negative quota move left out of the quota rates: {}{}.",
            plural(negative.len(), "run"),
            negative.iter().take(5).copied().collect::<Vec<_>>().join(", "),
            if negative.len() > 5 {
                format!(", and {} more", negative.len() - 5)
            } else {
                String::new()
            }
        ));
    }
    let provisional = runs["provisionalRunFiles"].as_u64().unwrap_or(0) as usize;
    if provisional > 0 {
        parts.push(format!(
            "{} not read under folders that could not be listed, counted from their last good listing: a provisional last-known count (files added or removed since are unknown), not a current count or a guaranteed lower bound.",
            plural(provisional, "run file")
        ));
    }
    let problems = array(&runs["problems"]);
    let files: Vec<_> = problems
        .iter()
        .filter(|p| !s(&p["file"]).starts_with("limits/"))
        .cloned()
        .collect();
    if !files.is_empty() {
        let since = files.iter().filter_map(|p| ctx.epoch(&p["since"])).min()?;
        parts.insert(
            0,
            format!(
                "{} unreadable since {}: {}.",
                plural(files.len(), "file"),
                ctx.clock(since),
                problem_list(&files)
            ),
        );
    }
    for p in problems.iter().filter(|p| s(&p["file"]).starts_with("limits/")) {
        parts.push(s(&p["reason"]).to_owned());
    }
    Some(source_finish(
        base,
        if files.is_empty() { "ok" } else { "unreadable" },
        parts.join(" "),
    ))
}
fn catalog_source(
    input: &V,
    missing_types: &[(String, usize)],
    missing_models: &[(String, usize)],
    now: i64,
    ctx: &Context,
) -> Option<V> {
    let mut source = catalog_source_of(input, missing_types, missing_models, now, ctx)?;
    // A snapshot taken while the scan was still running is partial, whatever its state says.
    if input["catalog"]["partial"] == true {
        source["detail"] = json!(format!(
            "{} The scan is still running; this view is partial.",
            s(&source["detail"])
        ));
    }
    Some(source)
}
fn catalog_source_of(
    input: &V,
    missing_types: &[(String, usize)],
    missing_models: &[(String, usize)],
    now: i64,
    ctx: &Context,
) -> Option<V> {
    let catalog = &input["catalog"];
    let at = ctx.epoch(&catalog["scannedAt"]);
    let base = source_base("scoreboard-catalog", "Scoreboard catalog", at, now, ctx);
    let Some(at) = at else {
        return Some(source_finish(base, "missing", "Waiting for the first scan.".into()));
    };
    if catalog["state"] == "missing" {
        return Some(source_finish(
            base,
            "missing",
            "No catalog folder in the agent reports.".into(),
        ));
    }
    if catalog["state"] == "error" {
        return Some(source_finish(
            base,
            "error",
            "The catalog folder could not be listed.".into(),
        ));
    }
    if number(&base["ageSeconds"])? > 180.0 {
        return Some(source_finish(
            base,
            "stale",
            format!("The catalog folder has not answered since {}.", ctx.clock(at)),
        ));
    }
    let missing = array(&catalog["missingFiles"]);
    let seeds = array(&input["seedFailures"]);
    let not_added = seeds
        .iter()
        .filter(|f| !missing.contains(&f["file"]))
        .map(|f| {
            format!(
                " Could not add new packaged entries to {}: {}.",
                s(&f["file"]),
                s(&f["code"])
            )
        })
        .collect::<String>();
    let problems = array(&catalog["problems"]);
    if !problems.is_empty() {
        let since = problems.iter().filter_map(|p| ctx.epoch(&p["since"])).min()?;
        return Some(source_finish(
            base,
            "unreadable",
            format!(
                "{} unreadable since {}: {}.{not_added}",
                plural(problems.len(), "catalog file"),
                ctx.clock(since),
                problem_list(&problems)
            ),
        ));
    }
    if !missing.is_empty() {
        let seeds = seeds
            .iter()
            .filter(|f| missing.contains(&f["file"]))
            .map(|f| format!(" Could not seed {}: {}.", s(&f["file"]), s(&f["code"])))
            .collect::<String>();
        return Some(source_finish(
            base,
            "missing",
            format!(
                "Catalog files missing: {}.{seeds}{not_added}",
                missing.iter().map(s).collect::<Vec<_>>().join(", ")
            ),
        ));
    }
    let mut parts = vec![format!("Catalog read {}.", ctx.clock(at))];
    if !missing_types.is_empty() {
        parts.push(format!(
            "Task types in runs but not in the catalog, not scored: {}.",
            counted(missing_types, ctx)
        ));
    }
    if !missing_models.is_empty() {
        parts.push(format!(
            "Models in runs but not in the catalog, not scored: {}.",
            counted(missing_models, ctx)
        ));
    }
    Some(source_finish(base, "ok", format!("{}{not_added}", parts.join(" "))))
}
/// [`bump`]'s counts in first-seen order, with the key's position held so a run's count is
/// one lookup (the list is searched per run, with up to one entry per distinct model).
#[derive(Default)]
struct Tally {
    order: Vec<(String, usize)>,
    at: HashMap<String, usize>,
}
impl Tally {
    fn bump(&mut self, key: &str) {
        if let Some(&at) = self.at.get(key) {
            self.order[at].1 += 1;
        } else {
            self.at.insert(key.to_owned(), self.order.len());
            self.order.push((key.to_owned(), 1));
        }
    }
}
fn bump(counts: &mut Vec<(String, usize)>, key: &str) {
    if let Some((_, n)) = counts.iter_mut().find(|(s, _)| s == key) {
        *n += 1;
    } else {
        counts.push((key.to_owned(), 1));
    }
}

/// What compose reads of one row, besides the docs it shows: from the run as stored (`raw`,
/// the row's `doc`) and its display-normalized doc. The R4 owner holds these per snapshot and
/// catalog; the envelope path makes them per call. Same values either way.
pub struct RowFacts {
    /// `model(doc)` as text (`""` when null).
    model_id: String,
    /// `raw.task.type`.
    kind: String,
    /// `raw.model.id` is null.
    raw_model_null: bool,
    /// `raw.model.id` as text.
    raw_model_id: String,
    /// The row's `file`.
    file: String,
    /// The doc carries a review (`Index` joins only those).
    has_review: bool,
}

pub fn row_facts(row: &V, doc: &V) -> RowFacts {
    let raw = &row["doc"];
    RowFacts {
        model_id: model_id(doc).into_owned(),
        kind: s(&raw["task"]["type"]).to_owned(),
        raw_model_null: raw["model"]["id"].is_null(),
        raw_model_id: s(&raw["model"]["id"]).to_owned(),
        file: s(&row["file"]).to_owned(),
        has_review: !doc["review"].is_null(),
    }
}

/// Each doc's position in `sort_by(newest)` over all `docs` (stable), under `facts`; None when
/// that sort could not be proved (an unreadable time or a missing rank), so the caller sorts
/// per request as before. Restricted to a subset, this order is that subset's stable sort.
pub fn newest_positions(docs: &[&V], facts: std::sync::Arc<crate::read_context::Presentation>) -> Option<Vec<usize>> {
    let ctx = Context::shared(facts, None);
    let order = newest_order(docs, &ctx);
    if !ctx.valid() {
        return None;
    }
    let mut positions = vec![0; docs.len()];
    for (at, &index) in order.iter().enumerate() {
        positions[index] = at;
    }
    Some(positions)
}

pub fn compose(envelope: Envelope) -> Option<V> {
    if crate::read_context::proto_named(&envelope.data) {
        return None;
    }
    compose_in(Context::new(envelope.presentation), envelope.data).map(|(view, _)| view)
}

/// R4 owned: the same view over facts held once per pushed facts generation (shared by every
/// request that names it) plus the request's own `now` and its date words, which a generation's
/// facts cannot hold. Byte-equal to [`compose`] of an envelope whose facts also hold `now`.
pub fn compose_fed(
    data: V,
    facts: std::sync::Arc<crate::read_context::Presentation>,
    now: (i64, crate::read_context::Date),
) -> Option<V> {
    compose_fed_keep(data, facts, now).map(|(view, _)| view)
}

/// [`compose_fed`], also handing back the (large) input so the caller can release it off its
/// request path.
pub fn compose_fed_keep(
    data: V,
    facts: std::sync::Arc<crate::read_context::Presentation>,
    now: (i64, crate::read_context::Date),
) -> Option<(V, V)> {
    if crate::read_context::proto_named(&data) {
        return None;
    }
    compose_in(Context::shared(facts, Some(now)).with_iso_any(), data)
}

fn compose_in(ctx: Context, data: V) -> Option<(V, V)> {
    let mut input = data;
    // Epoch facts normalize UTC seconds; the resolver separately retains all nine fractional digits.
    crate::run_corrections::resolve_state(&mut input["runs"], |v| ctx.epoch(v))?;
    // Borrowed, not cloned: the docs move into their normalized list; only a review is copied.
    let docs_in = std::mem::take(&mut input["docs"]);
    let V::Array(docs_in) = docs_in else {
        return None;
    };
    let normalized = {
        let effective: HashMap<&str, &V> = input["runs"]["runs"]
            .as_array()?
            .iter()
            .map(|r| (s(&r["doc"]["id"]), &r["doc"]))
            .collect();
        let mut normalized = Vec::new();
        for mut doc in docs_in {
            let Some(raw) = effective.get(s(&doc["id"])).copied() else {
                continue;
            };
            if raw["review"].is_object() {
                doc["review"] = raw["review"].clone();
            }
            normalized.push(doc);
        }
        normalized
    };
    input["docs"] = V::Array(normalized);
    let view = {
        let rows: Vec<&V> = input["runs"]["runs"].as_array()?.iter().collect();
        let docs: Vec<&V> = input["docs"].as_array()?.iter().collect();
        if rows.len() != docs.len() {
            return None;
        }
        let facts: Vec<RowFacts> = rows.iter().zip(&docs).map(|(row, doc)| row_facts(row, doc)).collect();
        let facts: Vec<&RowFacts> = facts.iter().collect();
        compose_core(&ctx, &input, &rows, &docs, &facts, None)?
    };
    Some((view, input))
}

/// R4 owned: [`compose_fed`] over rows and docs the caller holds, by reference. The caller
/// guarantees what the envelope path establishes itself: `rows` are the runs state's rows
/// (corrections resolved, unique run ids), `docs` the display-normalized doc of each row in
/// the same order with its run's review merged in, and neither holds a prototype name (checked
/// once per held snapshot). `data` is the envelope without `runs.runs` and `docs`.
#[allow(clippy::too_many_arguments)]
pub fn compose_fed_parts(
    data: &V,
    rows: &[&V],
    docs: &[&V],
    row_facts: &[&RowFacts],
    order: Option<&[usize]>,
    facts: std::sync::Arc<crate::read_context::Presentation>,
    now: (i64, crate::read_context::Date),
) -> Option<V> {
    if data["runs"]["correctionsResolved"] != true || crate::read_context::proto_named(data) {
        return None;
    }
    if rows.len() != docs.len() || row_facts.len() != rows.len() || order.is_some_and(|o| o.len() != rows.len()) {
        return None;
    }
    let ctx = Context::shared(facts, Some(now)).with_iso_any();
    compose_core(&ctx, data, rows, docs, row_facts, order)
}

/// `order`: each row's position in the stable newest order (see [`newest_positions`]), when
/// the caller holds it; otherwise the two sorts run here.
fn compose_core(
    ctx: &Context,
    input: &V,
    rows: &[&V],
    docs: &[&V],
    facts: &[&RowFacts],
    order: Option<&[usize]>,
) -> Option<V> {
    let now = input["now"].as_i64()?;
    let catalog = &input["catalog"];
    let raw: Vec<&V> = rows.iter().map(|r| &r["doc"]).collect();
    if raw.len() != docs.len() {
        return None;
    }
    let models = catalog["models"].as_array()?;
    let types = catalog["taskTypes"].as_array()?;
    let known_models: HashSet<_> = models.iter().map(|m| s(&m["model"])).collect();
    let known_types: HashSet<_> = types.iter().map(|t| s(&t["id"])).collect();
    let (mut no_model, mut no_type, mut unknown_model, mut unknown_type) = (0, 0, 0, 0);
    let (mut missing_types, mut missing_models, mut unknown_tally) =
        (Tally::default(), Tally::default(), Tally::default());
    // The unscored runs, as (index into `raw`, reason): only the ten shown are built.
    let mut examples: Vec<(usize, &'static str)> = Vec::new();
    let mut by_type: HashMap<String, Vec<(V, &str)>> = HashMap::new();
    for (at, run) in docs.iter().enumerate() {
        let fact = facts[at];
        let id = fact.model_id.as_str();
        let kind = fact.kind.as_str();
        if !id.is_empty() && !kind.is_empty() && !known_types.contains(kind) {
            missing_types.bump(kind);
        } else if !id.is_empty() && !kind.is_empty() && !known_models.contains(id) {
            missing_models.bump(id);
        }
        let reason = if fact.raw_model_null {
            no_model += 1;
            "no-model"
        } else if kind.is_empty() {
            no_type += 1;
            "no-task-type"
        } else if id.is_empty() || !known_models.contains(id) {
            unknown_model += 1;
            unknown_tally.bump(&fact.raw_model_id);
            "unknown-model"
        } else if !known_types.contains(kind) {
            unknown_type += 1;
            "unknown-task-type"
        } else {
            by_type
                .entry(kind.to_owned())
                .or_default()
                .push(((*run).clone(), fact.file.as_str()));
            continue;
        };
        examples.push((at, reason));
    }
    let run_files: HashSet<&str> = facts.iter().map(|f| f.file.as_str()).collect();
    let unreadable = array(&input["runs"]["problems"])
        .iter()
        .filter(|p| {
            let file = s(&p["file"]);
            file.starts_with("writers/")
                && file.contains("/runs/")
                && file.ends_with(".json")
                && !run_files.contains(file)
        })
        .count();
    let scan_limit = input["runs"]["skipped"]["runFiles"].as_u64().unwrap_or(0) as usize;
    let (missing_types, missing_models, mut unknown_counts) =
        (missing_types.order, missing_models.order, unknown_tally.order);
    unknown_counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| ctx.compare(&a.0, &b.0)));
    // `newest` reads `endedAt` and `id`, which each example copies from its run.
    let example_order = match order {
        Some(order) => {
            let mut picked: Vec<usize> = (0..examples.len()).collect();
            picked.sort_by_key(|&i| order[examples[i].0]);
            picked
        }
        None => {
            let example_runs: Vec<&V> = examples.iter().map(|&(at, _)| raw[at]).collect();
            newest_order(&example_runs, ctx)
        }
    };
    let unscored = json!({"total":scan_limit+unreadable+no_model+no_type+unknown_model+unknown_type,"scanLimit":scan_limit,"unreadable":unreadable,"noModel":no_model,"noTaskType":no_type,"unknownModel":unknown_model,"unknownTaskType":unknown_type,"unknownModels":unknown_counts.iter().take(20).map(|(id,n)|json!({"id":id,"runs":n})).collect::<Vec<_>>(),"examples":example_order.iter().take(10).map(|&i|{let (at,reason)=examples[i];let raw=raw[at];json!({"id":raw["id"],"model":raw["model"]["id"],"taskType":raw["task"]["type"],"reason":reason,"endedAt":raw["endedAt"]})}).collect::<Vec<_>>()});
    let join = array(&input["joinReviews"]);
    // No run with a review: the index is empty (Index::new would skip every run).
    let index = if facts.iter().all(|f| !f.has_review) && join.iter().all(|r| r["review"].is_null()) {
        Index::empty()
    } else {
        Index::new(docs, &join, &known_models, &known_types, ctx)?
    };
    let problems = array(&input["runs"]["problems"]);
    let review_health = review_problems(docs, facts, &problems, &index, ctx);
    let catalog_health = catalog_verdict_health(catalog, now, ctx);
    let mut bases = HashMap::new();
    for task in types {
        bases.insert(
            s(&task["id"]).to_owned(),
            base_rows(
                task,
                by_type.get(s(&task["id"])).map(Vec::as_slice).unwrap_or(&[]),
                catalog,
                &index,
                &problems,
                &review_health,
                now,
                ctx,
            )?,
        );
    }
    let mut uses = Vec::new();
    for usage in catalog["uses"].as_array()? {
        let requested = array(&usage["taskTypes"]);
        let wanted: Vec<_> = if requested.contains(&json!("*")) {
            types.iter().filter(|t| by_type.contains_key(s(&t["id"]))).collect()
        } else {
            requested
                .iter()
                .filter_map(|id| types.iter().find(|t| t["id"] == *id))
                .collect()
        };
        let mut task_types = Vec::new();
        for task in wanted {
            let mut rows = judged_rows(bases.get(s(&task["id"]))?, task, usage, catalog, ctx)?;
            // Every ranked verdict compared the retained catalog definitions with the runs; a
            // listing that failed this pass is disclosed on each of them. A row's own failure is
            // already named and stays the row's (a rarer case than the one this covers).
            if let Some((state, as_of, age_seconds)) = &catalog_health {
                for row in &mut rows {
                    if row["ranked"] == true && row.get("sourceState").is_none()
                        && let Some(obj) = row.as_object_mut()
                    {
                        obj.insert("sourceState".to_owned(), state.clone());
                        obj.insert("asOf".to_owned(), as_of.clone());
                        obj.insert("ageSeconds".to_owned(), age_seconds.clone());
                    }
                }
            }
            task_types.push(json!({"id":task["id"],"label":task["label"],"description":task["description"],"unit":task["unit"],"measures":task["measures"],"byProcess":task["byProcess"],"rows":rows}));
        }
        uses.push(
            json!({"id":usage["id"],"label":usage["label"],"description":usage["description"],"taskTypes":task_types}),
        );
    }
    let newest = match order {
        Some(order) => {
            let mut picked: Vec<usize> = (0..docs.len()).collect();
            picked.sort_by_key(|&i| order[i]);
            picked
        }
        None => newest_order(docs, ctx),
    };
    let recent: Vec<&V> = newest.into_iter().take(20).map(|i| docs[i]).collect();
    let mut view = json!({"version":1,"generatedAt":ctx.iso(Some(now)),"refresh":{"pollSeconds":60},"sources":[runs_source(input,docs,no_model+no_type,&index,now,ctx)?,catalog_source(input,&missing_types,&missing_models,now,ctx)?],"catalog":{"minRuns":catalog["minRuns"],"models":catalog["models"],"taskTypes":catalog["taskTypes"],"measures":catalog["measures"],"uses":catalog["uses"],"benchmarks":catalog["benchmarks"]},"uses":uses,"recent":recent.iter().map(|r|summary(r,models)).collect::<Vec<_>>(),"unscored":unscored});
    if let Some(finish) = input.get("finish") {
        for key in ["filters", "identity", "history"] {
            view[key] = finish.get(key)?.clone();
        }
        if finish["native"] == true {
            let typed = serde_json::from_value::<crate::scoreboard::ViewIn>(native_input(view.clone())).ok()?;
            {
                if let Ok(by_model) = crate::scoreboard::by_model(&typed) {
                    view["byModel"] = serde_json::to_value(by_model).ok()?;
                    if let Ok(groups) = crate::scoreboard::group_keys(&typed) {
                        view["groups"] = serde_json::to_value(groups).ok()?;
                    }
                }
            }
        }
        view["cached"] = json!(true);
        view["cacheAgeSeconds"] = json!(0);
        view["refreshing"] = json!(false);
    }
    ctx.valid().then_some(view)
}

// byModel and groups keep their struct field order: the NAPI module TS calls
// (`composeByModel`, `composeGroups`) serializes the same structs with `preserve_order`
// (this crate's serde_json feature), so both paths emit the same member order. The
// earlier key sort here mirrored a sorted-map build that no longer exists.

// JSON.stringify emits integral JS Numbers as integer tokens. Match the input the
// existing Node adapter parses, including numeric fields its DTO declares as i64.
fn native_input(value: V) -> V {
    match value {
        V::Number(number) => {
            if let Some(value) = number.as_f64()
                && value.fract() == 0.0
                && value.abs() <= 9007199254740991.0
            {
                return json!(value as i64);
            }
            V::Number(number)
        }
        V::Array(items) => json!(items.into_iter().map(native_input).collect::<Vec<_>>()),
        V::Object(object) => V::Object(
            object
                .into_iter()
                .map(|(key, value)| (key, native_input(value)))
                .collect(),
        ),
        value => value,
    }
}
