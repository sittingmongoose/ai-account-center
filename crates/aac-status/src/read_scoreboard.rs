//! Model Scoreboard base view, measures, grouping, ranking and source explanations. Input is
//! the existing accepted reader state; no share scan, store write or provider refresh occurs.
use crate::{
    read_context::{Context, Envelope, age, array, js_whitespace, number, plural, s},
    read_measures::{Index, decisive, measures, outcome},
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
fn newest(a: &V, b: &V, ctx: &Context) -> std::cmp::Ordering {
    ctx.epoch(&b["endedAt"])
        .cmp(&ctx.epoch(&a["endedAt"]))
        .then_with(|| ctx.compare(s(&a["id"]), s(&b["id"])))
}
fn model(run: &V) -> V {
    let id = s(&run["model"]["id"]);
    let p = s(&run["model"]["provider"]);
    if id.is_empty() {
        V::Null
    } else {
        json!(if p.is_empty() || id.starts_with(&format!("{p}/")) {
            id.to_owned()
        } else {
            format!("{p}/{id}")
        })
    }
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
fn base_rows(task: &V, runs: &[V], catalog: &V, index: &Index, ctx: &Context) -> Option<Vec<V>> {
    let mut grouped: Vec<(String, V, Vec<V>)> = Vec::new();
    for run in runs {
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
        let at = grouped.iter().position(|(id, _, _)| id == &key).unwrap_or_else(|| {
            grouped.push((key.clone(), run.clone(), Vec::new()));
            grouped.len() - 1
        });
        grouped[at].2.push(run.clone());
    }
    let mut out = Vec::new();
    for (key, first, runs) in grouped {
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
        out.push(json!({"key":key,"model":model(&first),"runtime":first["model"]["harness"],"effort":first["model"]["effort"],"process":if task["byProcess"]==true&&first["process"].is_object(){json!({"id":first["process"]["id"],"version":first["process"]["version"]})}else{V::Null},"observed":if observed.is_empty(){V::Null}else{column(&observed,"observed",&V::Null,task,catalog,index,ctx)?},"benchmarks":benchmarks}));
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
        rows.push(json!({"key":base["key"],"model":base["model"],"label":catalog_model.as_ref().map(|m|m["label"].clone()).unwrap_or(base["model"].clone()),"inCatalog":catalog_model.is_some(),"runtime":base["runtime"],"effort":base["effort"],"process":base["process"],"n":{"observed":observed["n"].as_u64().unwrap_or(0),"benchmark":benchmarks.iter().filter_map(|c|c["n"].as_u64()).sum::<u64>()},"basis":headline.map(|h|h["source"].clone()),"benchmark":headline.map(|h|h["benchmark"].clone()),"ranked":headline.is_some(),"rank":headline.map(|h|h["rank"].clone()),"verdict":shown["verdict"],"verdictLabel":shown["verdictLabel"],"why":why,"observed":observed,"benchmarks":benchmarks}));
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
fn runs_source(input: &V, docs: &[V], unscored: usize, index: &Index, now: i64, ctx: &Context) -> Option<V> {
    let runs = &input["runs"];
    let at = ctx.epoch(&runs["scannedAt"]);
    let base = source_base("scoreboard-runs", "Agent runs", at, now, ctx);
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
            "{} ignored: not a review run, or no earlier known run.",
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
fn bump(counts: &mut Vec<(String, usize)>, key: &str) {
    if let Some((_, n)) = counts.iter_mut().find(|(s, _)| s == key) {
        *n += 1;
    } else {
        counts.push((key.to_owned(), 1));
    }
}

pub fn compose(envelope: Envelope) -> Option<V> {
    if crate::read_context::proto_named(&envelope.data) {
        return None;
    }
    let ctx = Context::new(envelope.presentation);
    let mut input = envelope.data;
    // Epoch facts normalize UTC seconds; the resolver separately retains all nine fractional digits.
    crate::run_corrections::resolve_state(&mut input["runs"], |v| ctx.epoch(v))?;
    let effective: HashMap<String, V> = input["runs"]["runs"]
        .as_array()?
        .iter()
        .map(|r| (s(&r["doc"]["id"]).to_owned(), r["doc"].clone()))
        .collect();
    let mut normalized = Vec::new();
    for doc in input["docs"].as_array()? {
        if let Some(raw) = effective.get(s(&doc["id"])) {
            let mut doc = doc.clone();
            if raw["review"].is_object() {
                doc["review"] = raw["review"].clone();
            }
            normalized.push(doc);
        }
    }
    input["docs"] = json!(normalized);
    let now = input["now"].as_i64()?;
    let catalog = &input["catalog"];
    let docs = input["docs"].as_array()?;
    let raw: Vec<_> = input["runs"]["runs"]
        .as_array()?
        .iter()
        .map(|r| r["doc"].clone())
        .collect();
    if raw.len() != docs.len() {
        return None;
    }
    let models = catalog["models"].as_array()?;
    let types = catalog["taskTypes"].as_array()?;
    let known_models: HashSet<_> = models.iter().map(|m| s(&m["model"])).collect();
    let known_types: HashSet<_> = types.iter().map(|t| s(&t["id"])).collect();
    let (mut no_model, mut no_type, mut unknown_model, mut unknown_type) = (0, 0, 0, 0);
    let (mut missing_types, mut missing_models, mut unknown_counts) = (Vec::new(), Vec::new(), Vec::new());
    let mut examples = Vec::new();
    let mut by_type: HashMap<String, Vec<V>> = HashMap::new();
    for (raw, run) in raw.iter().zip(docs) {
        let id = model(run);
        let id = s(&id);
        let kind = s(&raw["task"]["type"]);
        if !id.is_empty() && !kind.is_empty() && !known_types.contains(kind) {
            bump(&mut missing_types, kind);
        } else if !id.is_empty() && !kind.is_empty() && !known_models.contains(id) {
            bump(&mut missing_models, id);
        }
        let reason = if raw["model"]["id"].is_null() {
            no_model += 1;
            "no-model"
        } else if kind.is_empty() {
            no_type += 1;
            "no-task-type"
        } else if id.is_empty() || !known_models.contains(id) {
            unknown_model += 1;
            bump(&mut unknown_counts, s(&raw["model"]["id"]));
            "unknown-model"
        } else if !known_types.contains(kind) {
            unknown_type += 1;
            "unknown-task-type"
        } else {
            by_type.entry(kind.to_owned()).or_default().push(run.clone());
            continue;
        };
        examples.push(json!({"id":raw["id"],"model":raw["model"]["id"],"taskType":raw["task"]["type"],"reason":reason,"endedAt":raw["endedAt"]}));
    }
    let run_files: HashSet<_> = input["runs"]["runs"]
        .as_array()?
        .iter()
        .map(|r| s(&r["file"]))
        .collect();
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
    unknown_counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| ctx.compare(&a.0, &b.0)));
    examples.sort_by(|a, b| newest(a, b, &ctx));
    let unscored = json!({"total":scan_limit+unreadable+no_model+no_type+unknown_model+unknown_type,"scanLimit":scan_limit,"unreadable":unreadable,"noModel":no_model,"noTaskType":no_type,"unknownModel":unknown_model,"unknownTaskType":unknown_type,"unknownModels":unknown_counts.iter().take(20).map(|(id,n)|json!({"id":id,"runs":n})).collect::<Vec<_>>(),"examples":examples.into_iter().take(10).collect::<Vec<_>>()});
    let index = Index::new(docs, &array(&input["joinReviews"]), &ctx)?;
    let mut bases = HashMap::new();
    for task in types {
        bases.insert(
            s(&task["id"]).to_owned(),
            base_rows(
                task,
                by_type.get(s(&task["id"])).map(Vec::as_slice).unwrap_or(&[]),
                catalog,
                &index,
                &ctx,
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
            task_types.push(json!({"id":task["id"],"label":task["label"],"description":task["description"],"unit":task["unit"],"measures":task["measures"],"byProcess":task["byProcess"],"rows":judged_rows(bases.get(s(&task["id"]))?,task,usage,catalog,&ctx)?}));
        }
        uses.push(
            json!({"id":usage["id"],"label":usage["label"],"description":usage["description"],"taskTypes":task_types}),
        );
    }
    let mut recent = docs.clone();
    recent.sort_by(|a, b| newest(a, b, &ctx));
    let mut view = json!({"version":1,"generatedAt":ctx.iso(Some(now)),"refresh":{"pollSeconds":60},"sources":[runs_source(&input,docs,no_model+no_type,&index,now,&ctx)?,catalog_source(&input,&missing_types,&missing_models,now,&ctx)?],"catalog":{"minRuns":catalog["minRuns"],"models":catalog["models"],"taskTypes":catalog["taskTypes"],"measures":catalog["measures"],"uses":catalog["uses"],"benchmarks":catalog["benchmarks"]},"uses":uses,"recent":recent.iter().take(20).map(|r|summary(r,models)).collect::<Vec<_>>(),"unscored":unscored});
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
