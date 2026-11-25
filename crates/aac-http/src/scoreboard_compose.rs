//! Aggregate-backed Scoreboard view composer.
//!
//! Composes `/api/scoreboard` directly from the pre-aggregated `run_sb_*` buckets and bounded
//! recent windows in [`aac_store::scoreboard_agg::ScoreboardRead`], without materializing
//! `ResolvedHistory`, parsing full run documents, or building the multi-megabyte presentation
//! facts JSON. Produces byte-identical JSON to `aac_status::read_scoreboard::compose_fed_parts`.

use aac_num::view;
use aac_status::model_identity::{AliasTable, Model, normalize_model};
use aac_status::read_context::{
    PROTO_NAMES, age, array, median, number, plural, proto_named, rounded, s,
};
use aac_store::scoreboard_agg::{
    BucketAgg, BucketCostSample, BucketFlag, BucketJoinedReview, BucketKey, BucketRunSummary,
    ScoreboardFilter, ScoreboardRead, SpeedSample, newest_cmp,
};
use aac_views::decline::Decline;
use serde_json::{Map, Value as V, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

const NAME_WITHHELD: &str = "(name withheld)";

/// All inputs required to compose one store-backed Scoreboard view.
pub(crate) struct ComposeInput<'a> {
    pub(crate) now: i64,
    pub(crate) now_clock: &'a str,
    pub(crate) time_zone: &'a str,
    pub(crate) root: &'a str,
    pub(crate) catalog: &'a V,
    pub(crate) read: &'a ScoreboardRead,
    pub(crate) filter: &'a ScoreboardFilter,
    pub(crate) filter_from: Option<&'a str>,
    pub(crate) filter_to: Option<&'a str>,
    pub(crate) filter_source: &'a str,
    pub(crate) filter_task_type: Option<&'a str>,
    pub(crate) pass_state: &'a str,
    pub(crate) pass_problems: &'a [V],
    pub(crate) safe_paths: &'a HashMap<String, String>,
    pub(crate) seed_failures: &'a [V],
    pub(crate) importing: bool,
}

/// Composed Scoreboard view plus the runs problems list (for readers-check recording).
pub(crate) struct ComposedView {
    pub(crate) view: V,
    pub(crate) problems: Vec<V>,
}

struct ClockCtx<'a> {
    now: i64,
    now_clock: &'a str,
    zone: aac_num::intl::Zone,
}

impl<'a> ClockCtx<'a> {
    fn new(time_zone: &str, now: i64, now_clock: &'a str) -> Option<Self> {
        let offsets = crate::zone_facts::server_zone_facts(time_zone, now)?;
        Some(Self {
            now,
            now_clock,
            zone: offsets.zone,
        })
    }

    fn iso(&self, ms: Option<i64>) -> Option<V> {
        match ms {
            None => Some(V::Null),
            Some(m) => aac_num::ecma::to_iso_string(m).map(V::String),
        }
    }

    fn clock(&self, ms: i64) -> Option<String> {
        if ms == self.now {
            return Some(self.now_clock.to_string());
        }
        self.zone.words(ms).map(|(clock, _)| clock)
    }

    /// Match `read_context::Context::epoch`: `Ok(Some(ms))` = parsed instant, `Ok(None)` = null
    /// or ISO-prefixed NaN, `Err(())` = invalid (declines view).
    fn epoch(&self, value: &V) -> Result<Option<i64>, ()> {
        if value.is_null() {
            return Ok(None);
        }
        if let Some(ms) = value
            .as_i64()
            .filter(|ms| ms.unsigned_abs() <= aac_num::ecma::MAX_SAFE_INTEGER as u64)
        {
            return Ok(Some(ms));
        }
        if let Some(text) = value.as_str() {
            if let Some(ms) = aac_num::ecma::parse_instant(text) {
                return Ok(Some(ms));
            }
            if aac_num::ecma::iso_prefixed(text) {
                return Ok(None);
            }
        }
        Err(())
    }

    fn compare(&self, a: &str, b: &str) -> std::cmp::Ordering {
        if a == b {
            std::cmp::Ordering::Equal
        } else {
            aac_num::intl::collate(a, b)
        }
    }
}

fn is_proto_str(text: &str) -> bool {
    PROTO_NAMES.contains(&text)
}

fn unit_index(unit: &str) -> Option<usize> {
    match unit {
        "record" => Some(0),
        "file" => Some(1),
        "item" => Some(2),
        "word" => Some(3),
        "task" => Some(4),
        _ => None,
    }
}

fn units_for_size(task_size: &[Option<u64>; 5], unit_idx: usize) -> Option<u64> {
    match task_size[unit_idx] {
        Some(n) if n > 0 => Some(n),
        Some(_) => None,
        None if unit_idx == 4 => Some(1),
        _ => None,
    }
}

fn verdict_label(verdict: &str) -> &'static str {
    match verdict {
        "recommended" => "Recommended",
        "fine" => "Fine",
        "avoid" => "Avoid for this",
        _ => "Not enough data yet",
    }
}

/// Build the store-backed `runs.problems` array in the exact order of `ScoreboardOwned::state_rows`:
/// refused files (by pathHash), pass problems, then correction refusal problems (by cursor).
pub(crate) fn build_problems(
    read: &ScoreboardRead,
    pass_problems: &[V],
    safe_paths: &HashMap<String, String>,
) -> Vec<V> {
    let mut problems: Vec<V> = read
        .refused
        .iter()
        .map(|row| {
            let file = safe_paths
                .get(&row.path_hash)
                .cloned()
                .or_else(|| row.retained_file.clone())
                .unwrap_or_else(|| {
                    format!(
                        "refused/{NAME_WITHHELD} {}",
                        &row.path_hash[..row.path_hash.len().min(12)]
                    )
                });
            json!({
                "file": file,
                "reason": row.reason,
                "since": row.at_ms,
            })
        })
        .collect();
    problems.extend(pass_problems.iter().cloned());
    for cp in &read.correction_problems {
        problems.push(json!({
            "file": cp.file,
            "reason": cp.reason,
            "since": cp.since,
        }));
    }
    problems
}

/// Live-catalog model normalizer (same logic as `ScoreboardOwned::held_docs`).
struct CatalogNorm<'a> {
    models: Vec<Model<'a>>,
    aliases: &'a Map<String, V>,
    effort_suffixes: Vec<String>,
    memo: HashMap<(String, Option<String>), (String, Option<String>)>,
}

impl<'a> CatalogNorm<'a> {
    fn new(
        catalog: &'a V,
        efforts_buf: &'a [Vec<String>],
        empty_aliases: &'a Map<String, V>,
    ) -> Self {
        let models: Vec<Model<'a>> = catalog["models"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .zip(efforts_buf.iter())
                    .map(|(row, eff)| Model {
                        model: row["model"].as_str().unwrap_or_default(),
                        efforts: eff,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let aliases = catalog["aliases"].as_object().unwrap_or(empty_aliases);
        let effort_suffixes: Vec<String> = catalog["effortSuffixes"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(V::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            models,
            aliases,
            effort_suffixes,
            memo: HashMap::new(),
        }
    }

    fn normalize(&mut self, model_id: &str, effort: Option<&str>) -> (String, Option<String>) {
        if model_id.is_empty() {
            return (String::new(), effort.map(str::to_string));
        }
        let key = (model_id.to_string(), effort.map(str::to_string));
        if let Some(hit) = self.memo.get(&key) {
            return hit.clone();
        }
        let table = AliasTable {
            aliases: self.aliases,
            effort_suffixes: &self.effort_suffixes,
        };
        let resolved = normalize_model(model_id, effort, &self.models, Some(&table));
        let out = (resolved.id, resolved.effort);
        self.memo.insert(key, out.clone());
        out
    }
}

fn collect_catalog_efforts(catalog: &V) -> Vec<Vec<String>> {
    catalog["models"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    row["efforts"]
                        .as_array()
                        .map(|efforts| {
                            efforts
                                .iter()
                                .filter_map(V::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn summary_json(
    run: &BucketRunSummary,
    norm_model: &str,
    norm_effort: Option<&str>,
    models: &[V],
) -> V {
    let model_val = if norm_model.is_empty() {
        V::Null
    } else {
        json!(norm_model)
    };
    let model_label = if model_val.is_null() {
        V::Null
    } else {
        models
            .iter()
            .find(|m| m["model"] == model_val)
            .map(|m| m["label"].clone())
            .unwrap_or_else(|| model_val.clone())
    };
    let source = if run.source_benchmark {
        "benchmark"
    } else {
        "observed"
    };
    let process = match &run.process {
        Some((id, ver)) => json!({"id": id, "version": ver}),
        None => V::Null,
    };
    let imported = match &run.imported {
        Some((from, at)) => json!({"from": from, "at": at}),
        None => V::Null,
    };
    let review = match &run.review {
        Some(rev) => json!({
            "of": rev.of,
            "verdict": rev.verdict,
            "overturned": rev.overturned,
            "disputed": rev.disputed,
            "by": rev.by,
        }),
        None => V::Null,
    };
    let evidence: Vec<V> = run
        .evidence
        .iter()
        .map(|(label, path)| json!({"label": label, "path": path}))
        .collect();
    json!({
        "id": run.id,
        "writer": run.writer,
        "source": source,
        "benchmark": run.benchmark,
        "project": run.project,
        "stream": run.stream,
        "task": {"id": run.task_id, "type": run.task_type, "title": run.task_title},
        "model": model_val,
        "label": model_label,
        "runtime": run.harness,
        "effort": norm_effort,
        "process": process,
        "outcome": run.outcome,
        "endedAt": run.ended_at,
        "wallSeconds": run.wall_seconds,
        "imported": imported,
        "review": review,
        "flags": run.flags_len,
        "evidence": evidence,
    })
}

fn stat_json(values: &[f64]) -> Option<V> {
    Some(json!({
        "value": rounded(median(values))?,
        "n": values.len(),
        "low": rounded(values.iter().copied().reduce(f64::min))?,
        "high": rounded(values.iter().copied().reduce(f64::max))?,
    }))
}

fn blank_measure(unit: &str) -> V {
    json!({
        "value": null,
        "unit": unit,
        "n": 0,
        "low": null,
        "high": null,
        "interval": null,
        "of": null,
        "per": null,
        "preliminary": false,
    })
}

fn range_measure(unit: &str, values: &[f64]) -> Option<V> {
    let mut v = blank_measure(unit);
    for (k, item) in stat_json(values)?.as_object()? {
        v[k] = item.clone();
    }
    v["interval"] = if values.is_empty() {
        V::Null
    } else {
        json!("range")
    };
    Some(v)
}

fn share_measure(unit: &str, of: f64, per: f64, n: usize) -> Option<V> {
    let mut v = blank_measure(unit);
    v["n"] = json!(n);
    if per <= 0.0 {
        return Some(v);
    }
    v["value"] = json!(view::precision(of / per, 4)?);
    v["of"] = json!(view::precision(of, 10)?);
    v["per"] = json!(view::precision(per, 10)?);
    if of >= 0.0 && of <= per {
        let z = 1.6448536269514722;
        let z2 = z * z;
        let p = of / per;
        let den = 1.0 + z2 / per;
        let centre = (p + z2 / (2.0 * per)) / den;
        let margin = z * ((p * (1.0 - p)) / per + z2 / (4.0 * per * per)).sqrt() / den;
        v["low"] = json!(view::precision((centre - margin).max(0.0), 4)?);
        v["high"] = json!(view::precision((centre + margin).min(1.0), 4)?);
        v["interval"] = json!("wilson90");
    }
    Some(v)
}

struct ValidJoinedReview<'a> {
    rev: &'a BucketJoinedReview,
    in_filter: bool,
    reviewer_model_id: String,
    reviewer_effort: Option<String>,
}

fn review_in_filter(filter: &ScoreboardFilter, rev: &BucketJoinedReview) -> bool {
    filter.from_ms.is_none_or(|f| rev.review_cursor_ms >= f)
        && filter.to_ms.is_none_or(|t| rev.review_cursor_ms <= t)
        && filter
            .source_benchmark
            .is_none_or(|b| rev.review_source_benchmark == b)
        && filter
            .task_type
            .as_deref()
            .is_none_or(|tt| rev.review_task_type.as_deref() == Some(tt))
}

struct BucketRef<'a> {
    key: &'a BucketKey,
    agg: &'a BucketAgg,
    norm_model: String,
    norm_effort: Option<String>,
}

fn collect_valid_reviews<'a>(
    buckets: &[&BucketRef<'a>],
    filter: &ScoreboardFilter,
    known_models: &HashSet<&str>,
    known_types: &HashSet<&str>,
    norm: &mut CatalogNorm<'_>,
) -> Result<Vec<ValidJoinedReview<'a>>, Decline> {
    let review_type_known = known_types.contains("review");
    let mut out: Vec<ValidJoinedReview<'a>> = Vec::new();
    for b in buckets {
        for rev in &b.agg.joined_reviews {
            let in_f = review_in_filter(filter, rev);
            // Reviews outside the filter only join when `fromMs` admits them
            // (matches `scoreboard_owned.rs` `reviews_query`).
            if !in_f && !filter.from_ms.is_none_or(|f| rev.review_cursor_ms >= f) {
                continue;
            }
            if !in_f && rev.review_has_proto {
                return Err(Decline::Unsupported);
            }
            let Some(raw_rev_model) = rev.review_model_id.as_deref() else {
                continue;
            };
            if rev.review_task_type.as_deref() != Some("review") || !review_type_known {
                continue;
            }
            if rev.review_id == rev.target_id || rev.review_ended_epoch < rev.target_ended_epoch {
                continue;
            }
            let (rev_model_id, rev_effort) = if in_f {
                norm.normalize(raw_rev_model, rev.review_effort.as_deref())
            } else {
                (raw_rev_model.to_string(), rev.review_effort.clone())
            };
            if !known_models.contains(rev_model_id.as_str()) {
                continue;
            }
            out.push(ValidJoinedReview {
                rev,
                in_filter: in_f,
                reviewer_model_id: rev_model_id,
                reviewer_effort: rev_effort,
            });
        }
    }
    // Exact order of `Index::new` + `for run in runs`: ascending `(target_cursor_ms, target_r)`
    // outer, and within each target: ascending `review_ended_epoch` with `docs` before `join`
    // and ascending `(review_cursor_ms, review_r)`.
    out.sort_by(|a, b| {
        a.rev
            .target_cursor_ms
            .cmp(&b.rev.target_cursor_ms)
            .then_with(|| a.rev.target_r.cmp(&b.rev.target_r))
            .then_with(|| a.rev.review_ended_epoch.cmp(&b.rev.review_ended_epoch))
            .then_with(|| b.in_filter.cmp(&a.in_filter))
            .then_with(|| a.rev.review_cursor_ms.cmp(&b.rev.review_cursor_ms))
            .then_with(|| a.rev.review_r.cmp(&b.rev.review_r))
    });
    Ok(out)
}

fn compute_cost(
    buckets: &[&BucketRef<'_>],
    unit: &str,
    valid_reviews: &[ValidJoinedReview<'_>],
) -> Option<V> {
    let mut samples: Vec<&BucketCostSample> = Vec::new();
    for b in buckets {
        samples.extend(b.agg.cost_samples.iter());
    }
    // Preserve exact legacy RUN_BY_TIME `(cursor_ms, r)` addition order across arrival orders
    // and across buckets merged into one displayed column (IEEE-754 bit-exact with `read_measures`).
    samples.sort_by(|a, b| a.cursor_ms.cmp(&b.cursor_ms).then_with(|| a.r.cmp(&b.r)));

    let u_opt = unit_index(unit);
    let overturned_targets: HashSet<u32> = valid_reviews
        .iter()
        .filter(|vr| vr.rev.overturned && !vr.rev.disputed)
        .map(|vr| vr.rev.target_r)
        .collect();

    let eval_rate = |read: &dyn Fn(&BucketCostSample) -> Option<f64>, label: &str| -> Option<V> {
        let (mut total, mut accepted, mut n) = (0.0f64, 0.0f64, 0u64);
        for sample in &samples {
            let (Some(cost), Some(units)) = (
                read(sample),
                u_opt.and_then(|u| units_for_size(&sample.task_size, u)),
            ) else {
                continue;
            };
            n += 1;
            total += cost;
            if sample.outcome_done && !overturned_targets.contains(&sample.r) {
                accepted += units as f64;
            }
        }
        Some(json!({
            "value": if n > 0 && accepted > 0.0 {
                json!(view::precision(total / accepted, 4)?)
            } else {
                V::Null
            },
            "unit": format!("{label}/{unit}"),
            "n": n,
        }))
    };

    let eval_quotas = |estimated: bool| -> Option<V> {
        let mut windows: Vec<String> = samples
            .iter()
            .filter_map(|sample| {
                let q = if estimated {
                    sample.cost.estimate.as_ref()?.quota.as_ref()
                } else {
                    sample.cost.quota.as_ref()
                };
                q.map(|(w, _)| w.clone())
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        windows.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        let mut out = Vec::with_capacity(windows.len());
        for window in windows {
            let r = eval_rate(
                &|sample| {
                    let (w, d) = if estimated {
                        sample.cost.estimate.as_ref()?.quota.as_ref()?
                    } else {
                        sample.cost.quota.as_ref()?
                    };
                    (w == &window && *d >= 0.0).then_some(*d)
                },
                "%",
            )?;
            let mut item = json!({"window": window});
            for (k, v) in r.as_object()? {
                item[k] = v.clone();
            }
            out.push(item);
        }
        Some(json!(out))
    };

    let credits = eval_rate(&|sample| sample.cost.credits, "credits")?;
    let usd = eval_rate(&|sample| sample.cost.usd, "usd")?;
    let estimate_credits = eval_rate(
        &|sample| sample.cost.estimate.as_ref().and_then(|e| e.credits),
        "credits",
    )?;
    let estimated: Vec<&BucketCostSample> = samples
        .iter()
        .copied()
        .filter(|sample| sample.cost.estimate.is_some())
        .collect();

    let mut out = blank_measure(s(&credits["unit"]));
    out["value"] = credits["value"].clone();
    out["n"] = credits["n"].clone();
    out["quota"] = eval_quotas(false)?;
    out["usd"] = if usd["n"] != 0 { usd } else { V::Null };
    out["estimate"] = if estimated.is_empty() {
        V::Null
    } else {
        let mut methods: Vec<String> = estimated
            .iter()
            .filter_map(|sample| sample.cost.estimate.as_ref().map(|e| e.method.clone()))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        methods.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        let mut sources: Vec<String> = estimated
            .iter()
            .filter_map(|sample| sample.cost.estimate.as_ref().map(|e| e.source.clone()))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        sources.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        json!({
            "label": "estimate",
            "credits": if estimate_credits["n"] != 0 { estimate_credits } else { V::Null },
            "quota": eval_quotas(true)?,
            "methods": methods,
            "sources": sources,
        })
    };
    Some(out)
}

fn compute_quality(valid_reviews: &[ValidJoinedReview<'_>], unit: &str) -> Option<V> {
    type QualityItemKey<'a> = (Option<&'a str>, &'a str);
    let mut items: Vec<(QualityItemKey<'_>, Vec<&ValidJoinedReview<'_>>)> = Vec::new();
    let mut cannot = 0usize;
    let mut overturns: Vec<V> = Vec::new();
    for vr in valid_reviews {
        let rev = vr.rev;
        if rev.verdict == "cannot-verify" {
            cannot += 1;
            continue;
        }
        let key = (rev.target_project.as_deref(), rev.target_task_id.as_str());
        let pos = items
            .iter()
            .position(|(k, _)| *k == key)
            .unwrap_or_else(|| {
                items.push((key, Vec::new()));
                items.len() - 1
            });
        items[pos].1.push(vr);
        if rev.overturned {
            let model = if !vr.reviewer_model_id.is_empty() {
                let effort = vr.reviewer_effort.as_deref().unwrap_or("");
                json!(if effort.is_empty() {
                    vr.reviewer_model_id.clone()
                } else {
                    format!("{} {effort}", vr.reviewer_model_id)
                })
            } else {
                V::Null
            };
            let by = match &rev.by {
                Some(b) => json!(b),
                None => model,
            };
            overturns.push(json!({
                "run": rev.target_id,
                "review": rev.review_id,
                "by": by,
                "disputed": rev.disputed,
                "evidence": rev.evidence0_path,
            }));
        }
    }
    let (mut passed, mut failed, mut disputed, mut first_try) = (0usize, 0usize, 0usize, 0usize);
    let mut rounds: Vec<f64> = Vec::new();
    for (_, reviews) in &mut items {
        reviews.sort_by_key(|vr| vr.rev.review_ended_epoch);
        let last = reviews.last()?.rev;
        if last.overturned && last.disputed {
            disputed += 1;
        } else if last.verdict == "fail" || last.overturned {
            failed += 1;
        } else {
            passed += 1;
            let round = reviews
                .iter()
                .position(|vr| vr.rev.verdict == "pass" && !vr.rev.overturned)
                .map(|i| i + 1)
                .unwrap_or(0);
            if round == 1 {
                first_try += 1;
            }
            rounds.push(round as f64);
        }
    }
    let mut out = share_measure(
        unit,
        passed as f64,
        (passed + failed) as f64,
        passed + failed,
    )?;
    out["disputed"] = json!(disputed);
    out["cannotVerify"] = json!(cannot);
    out["rounds"] = stat_json(&rounds)?;
    out["firstTry"] = json!(first_try);
    out["overturns"] = json!(overturns);
    Some(out)
}

fn compute_overclaim(valid_reviews: &[ValidJoinedReview<'_>], unit: &str) -> Option<V> {
    // Group by target_r for done runs whose reviews include at least one decisive non-disputed review.
    let mut by_target: HashMap<u32, bool> = HashMap::new();
    for vr in valid_reviews {
        let rev = vr.rev;
        if !rev.target_outcome_done {
            continue;
        }
        if rev.verdict == "cannot-verify" || (rev.overturned && rev.disputed) {
            continue;
        }
        let is_overturn = rev.overturned && !rev.disputed;
        by_target
            .entry(rev.target_r)
            .and_modify(|ov| *ov |= is_overturn)
            .or_insert(is_overturn);
    }
    let claimed = by_target.len();
    let overturned = by_target.values().filter(|&&ov| ov).count();
    share_measure(unit, overturned as f64, claimed as f64, claimed)
}

fn compute_speed(buckets: &[&BucketRef<'_>], unit: &str) -> Option<V> {
    let u_opt = unit_index(unit);
    let mut per_unit: Vec<f64> = Vec::new();
    let mut per_task: Vec<f64> = Vec::new();
    for b in buckets {
        for SpeedSample {
            wall_seconds,
            task_size,
        } in &b.agg.speed_samples
        {
            per_task.push(*wall_seconds);
            if let Some(u) = u_opt
                && let Some(units) = units_for_size(task_size, u)
            {
                per_unit.push(*wall_seconds / (units as f64));
            }
        }
    }
    let mut out = range_measure(&format!("s/{unit}"), &per_unit)?;
    out["perTask"] = stat_json(&per_task)?;
    Some(out)
}

fn compute_thoroughness(buckets: &[&BucketRef<'_>], unit: &str) -> Option<V> {
    let mut mins: Vec<f64> = Vec::new();
    let mut files: Vec<f64> = Vec::new();
    let mut records: Vec<f64> = Vec::new();
    for b in buckets {
        mins.extend_from_slice(&b.agg.thoroughness_min);
        files.extend_from_slice(&b.agg.thoroughness_files);
        records.extend_from_slice(&b.agg.thoroughness_records);
    }
    let mut v = range_measure(unit, &mins)?;
    v["files"] = stat_json(&files)?;
    v["records"] = stat_json(&records)?;
    Some(v)
}

fn compute_measures(
    buckets: &[&BucketRef<'_>],
    total_n: usize,
    unit: &str,
    listed: &[V],
    defs: &[V],
    valid_reviews: &[ValidJoinedReview<'_>],
) -> Option<V> {
    let mut out = json!({});
    let total_metric_rows: usize = buckets.iter().map(|b| b.agg.metric_rows.len()).sum();
    for id_val in listed {
        let id = s(id_val);
        let Some(def) = defs.iter().find(|d| d["id"] == id) else {
            continue;
        };
        let label = s(&def["unit"]);
        let mut custom_behind: Option<Vec<&Vec<(String, f64)>>> = None;
        let mut value = if def["metric"].is_object() {
            let of_key = s(&def["metric"]["of"]);
            let per_key = s(&def["metric"]["per"]);
            let (mut of_sum, mut per_sum) = (0.0, 0.0);
            let mut behind: Vec<&Vec<(String, f64)>> = Vec::new();
            for b in buckets {
                for row in &b.agg.metric_rows {
                    let a = row.iter().find(|(k, _)| k == of_key).map(|(_, v)| *v);
                    let p = row.iter().find(|(k, _)| k == per_key).map(|(_, v)| *v);
                    if let (Some(a), Some(p)) = (a, p)
                        && a >= 0.0
                        && p > 0.0
                    {
                        of_sum += a;
                        per_sum += p;
                        behind.push(row);
                    }
                }
            }
            let n = behind.len();
            custom_behind = Some(behind);
            share_measure(label, of_sum, per_sum, n)?
        } else {
            match id {
                "speed" => compute_speed(buckets, unit)?,
                "quality" => compute_quality(valid_reviews, label)?,
                "thoroughness" => compute_thoroughness(buckets, label)?,
                "laziness" => {
                    let counted: u64 = buckets.iter().map(|b| b.agg.laziness_counted).sum();
                    let lazy: u64 = buckets.iter().map(|b| b.agg.laziness_lazy).sum();
                    share_measure(label, lazy as f64, counted as f64, counted as usize)?
                }
                "cheating" => {
                    let cheated: u64 = buckets.iter().map(|b| b.agg.cheating_count).sum();
                    share_measure(label, cheated as f64, total_n as f64, total_n)?
                }
                "reliability" => {
                    let done: u64 = buckets.iter().map(|b| b.agg.outcome_done).sum();
                    let known: u64 = buckets.iter().map(|b| b.agg.outcome_known).sum();
                    share_measure(label, done as f64, known as f64, known as usize)?
                }
                "overclaim" => compute_overclaim(valid_reviews, label)?,
                "cost" => compute_cost(buckets, unit, valid_reviews)?,
                _ => blank_measure(label),
            }
        };
        let preliminary = def["preliminaryUnless"].as_str().is_some_and(|name| {
            let row_lacks_one =
                |row: &Vec<(String, f64)>| !row.iter().any(|(k, v)| k == name && *v == 1.0);
            match &custom_behind {
                Some(rows) => rows.iter().any(|row| row_lacks_one(row)),
                None => {
                    total_metric_rows < total_n
                        || buckets
                            .iter()
                            .flat_map(|b| b.agg.metric_rows.iter())
                            .any(row_lacks_one)
                }
            }
        });
        value["preliminary"] = json!(preliminary);
        out[id] = value;
    }
    Some(out)
}

#[allow(clippy::too_many_arguments)]
fn compose_column(
    buckets: &[&BucketRef<'_>],
    source: &str,
    benchmark: &V,
    task: &V,
    catalog: &V,
    filter: &ScoreboardFilter,
    known_models: &HashSet<&str>,
    known_types: &HashSet<&str>,
    norm: &mut CatalogNorm<'_>,
) -> Result<(V, bool), Decline> {
    let valid_reviews = collect_valid_reviews(buckets, filter, known_models, known_types, norm)?;
    let used_review = !valid_reviews.is_empty();
    let total_n: usize = buckets.iter().map(|b| b.agg.n as usize).sum();
    let total_imported: usize = buckets.iter().map(|b| b.agg.imported as usize).sum();
    let decisive_reviews = valid_reviews
        .iter()
        .filter(|vr| vr.rev.verdict != "cannot-verify")
        .count();
    let task_measures = task["measures"].as_array().ok_or(Decline::Stale)?;
    let catalog_measures = catalog["measures"].as_array().ok_or(Decline::Stale)?;
    let values = compute_measures(
        buckets,
        total_n,
        s(&task["unit"]),
        task_measures,
        catalog_measures,
        &valid_reviews,
    )
    .ok_or(Decline::Stale)?;
    let mut all_flags: Vec<&BucketFlag> = buckets.iter().flat_map(|b| b.agg.flags.iter()).collect();
    all_flags.sort_by(|a, b| {
        newest_cmp(a.ended_epoch, &a.run_id, b.ended_epoch, &b.run_id)
            .then_with(|| a.flag_idx.cmp(&b.flag_idx))
    });
    let flags: Vec<V> = all_flags
        .into_iter()
        .map(|f| {
            json!({
                "kind": f.kind,
                "run": f.run_id,
                "detail": f.detail,
                "evidence": f.evidence,
                "reportedBy": f.reported_by,
            })
        })
        .collect();
    let mut recent_runs: Vec<(&BucketRunSummary, &str, Option<&str>)> = buckets
        .iter()
        .flat_map(|b| {
            b.agg
                .recent_runs
                .iter()
                .map(|r| (r, b.norm_model.as_str(), b.norm_effort.as_deref()))
        })
        .collect();
    recent_runs.sort_by(|a, b| newest_cmp(a.0.ended_epoch, &a.0.id, b.0.ended_epoch, &b.0.id));
    let models = catalog["models"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let runs_json: Vec<V> = recent_runs
        .into_iter()
        .take(20)
        .map(|(r, m, e)| summary_json(r, m, e, models))
        .collect();
    let benchmark_label = if benchmark.is_null() {
        V::Null
    } else {
        array(&catalog["benchmarks"])
            .iter()
            .find(|b| b["id"] == *benchmark)
            .map(|b| b["label"].clone())
            .unwrap_or_else(|| benchmark.clone())
    };
    let preliminary = values
        .as_object()
        .ok_or(Decline::Stale)?
        .values()
        .any(|v| v["preliminary"] == true);
    let col = json!({
        "source": source,
        "benchmark": benchmark,
        "benchmarkLabel": benchmark_label,
        "n": total_n,
        "imported": total_imported,
        "reviews": decisive_reviews,
        "ranked": false,
        "rank": null,
        "score": null,
        "verdict": "not-enough-data",
        "verdictLabel": "Not enough data yet",
        "why": "",
        "preliminary": preliminary,
        "measures": values,
        "flags": flags,
        "runs": runs_json,
    });
    Ok((col, used_review))
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

fn judge(columns: &[V], usage: &V, task: &V, catalog: &V, ctx: &ClockCtx<'_>) -> Option<Vec<V>> {
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
            let best_val = best.get(s(&def["id"])).copied().unwrap_or(0.0);
            let mark = if def["unit"] == "share" {
                let clamped = value.clamp(0.0, 1.0);
                if def["better"] == "higher" {
                    clamped
                } else {
                    1.0 - clamped
                }
            } else if def["better"] == "lower" {
                if value == 0.0 { 1.0 } else { best_val / value }
            } else if best_val == 0.0 {
                1.0
            } else {
                value / best_val
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
                score: marks
                    .iter()
                    .map(|m| m.mark * m.weight)
                    .fold(0.0, |a, b| a + b)
                    / known,
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
            .then_with(|| {
                ctx.compare(
                    s(&columns[a.at]["runs"][0]["id"]),
                    s(&columns[b.at]["runs"][0]["id"]),
                )
            })
    });
    let top = scored.first().map(|e| e.score).unwrap_or(0.0);
    let mut out = columns.to_vec();
    for c in &mut out {
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
        let (verdict, mut why) = if let Some(m) =
            cheating.filter(|m| number(&m.value["value"]).is_some_and(|v| v >= 0.2))
        {
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
            (
                "fine",
                format!("Too few measures known to recommend it (n = {n})."),
            )
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
                format!(
                    "Behind the top row, weakest on {} (n = {n}).",
                    s(&weakest.def["label"])
                ),
            )
        };
        if e.marks.iter().any(|m| m.value["preliminary"] == true) {
            why.push_str(" Preliminary.");
        }
        c["ranked"] = json!(true);
        c["rank"] = json!(position + 1);
        c["score"] = json!(view::precision(e.score, 3)?);
        c["verdict"] = json!(verdict);
        c["verdictLabel"] = json!(verdict_label(verdict));
        c["why"] = json!(why);
        c["runs"] = json!(array(&c["runs"]).into_iter().take(5).collect::<Vec<_>>());
    }
    Some(out)
}

#[allow(clippy::too_many_arguments)]
fn base_rows(
    task: &V,
    entries: &[BucketRef<'_>],
    catalog: &V,
    filter: &ScoreboardFilter,
    known_models: &HashSet<&str>,
    known_types: &HashSet<&str>,
    norm: &mut CatalogNorm<'_>,
    read: &ScoreboardRead,
    review_source: &Option<(V, V)>,
    now: i64,
    ctx: &ClockCtx<'_>,
) -> Result<Vec<V>, Decline> {
    let by_process = task["byProcess"] == true;
    struct Group<'a> {
        key: String,
        model: String,
        runtime: Option<String>,
        effort: Option<String>,
        process: Option<(String, Option<String>)>,
        buckets: Vec<&'a BucketRef<'a>>,
    }
    let mut groups: Vec<Group<'_>> = Vec::new();
    let mut group_idx: HashMap<String, usize> = HashMap::new();
    for b in entries {
        if b.norm_model.is_empty() {
            continue;
        }
        let harness = b.key.harness.as_str();
        let effort = b.norm_effort.as_deref().unwrap_or("");
        let key = if by_process {
            format!(
                "{}|{}|{}|{}|{}",
                b.norm_model, harness, effort, b.key.proc_id, b.key.proc_ver
            )
        } else {
            format!("{}|{}|{}", b.norm_model, harness, effort)
        };
        let pos = match group_idx.get(&key) {
            Some(&p) => p,
            None => {
                let p = groups.len();
                group_idx.insert(key.clone(), p);
                let proc_val = if by_process && !b.key.proc_id.is_empty() {
                    Some((
                        b.key.proc_id.clone(),
                        (!b.key.proc_ver.is_empty()).then(|| b.key.proc_ver.clone()),
                    ))
                } else {
                    None
                };
                groups.push(Group {
                    key,
                    model: b.norm_model.clone(),
                    runtime: (!b.key.harness.is_empty()).then(|| b.key.harness.clone()),
                    effort: b.norm_effort.clone(),
                    process: proc_val,
                    buckets: Vec::new(),
                });
                p
            }
        };
        groups[pos].buckets.push(b);
    }

    let mut out = Vec::with_capacity(groups.len());
    for g in groups {
        let observed_buckets: Vec<&BucketRef<'_>> = g
            .buckets
            .iter()
            .copied()
            .filter(|b| !b.key.source_benchmark)
            .collect();
        let mut bench_map: HashMap<&str, Vec<&BucketRef<'_>>> = HashMap::new();
        for &b in &g.buckets {
            if b.key.source_benchmark {
                bench_map
                    .entry(b.key.benchmark.as_str())
                    .or_default()
                    .push(b);
            }
        }
        let mut bench_ids: Vec<&str> = bench_map.keys().copied().collect();
        bench_ids.sort_by(|a, b| ctx.compare(a, b));
        let mut used_review = false;
        let mut benchmarks = Vec::with_capacity(bench_ids.len());
        for id in bench_ids {
            let bench_val = if id.is_empty() { V::Null } else { json!(id) };
            let col_buckets = bench_map.get(id).map(Vec::as_slice).unwrap_or(&[]);
            let (col, col_rev) = compose_column(
                col_buckets,
                "benchmark",
                &bench_val,
                task,
                catalog,
                filter,
                known_models,
                known_types,
                norm,
            )?;
            used_review |= col_rev;
            benchmarks.push(col);
        }
        let observed = if observed_buckets.is_empty() {
            V::Null
        } else {
            let (col, col_rev) = compose_column(
                &observed_buckets,
                "observed",
                &V::Null,
                task,
                catalog,
                filter,
                known_models,
                known_types,
                norm,
            )?;
            used_review |= col_rev;
            col
        };
        let proc_json = match g.process {
            Some((id, ver)) => json!({"id": id, "version": ver}),
            None => V::Null,
        };
        let mut row = json!({
            "key": g.key,
            "model": g.model,
            "runtime": g.runtime,
            "effort": g.effort,
            "process": proc_json,
            "observed": observed,
            "benchmarks": benchmarks,
        });
        let mut row_problem_matched = false;
        let mut row_problem_earliest: Option<i64> = None;
        for &b in &g.buckets {
            if read.problem_trace.bucket_matched.contains(b.key) {
                row_problem_matched = true;
                if let Some(Some(s)) = read.problem_trace.bucket_earliest_since.get(b.key)
                    && row_problem_earliest.is_none_or(|prev| *s < prev)
                {
                    row_problem_earliest = Some(*s);
                }
            }
        }
        let traced = if row_problem_matched {
            Some((
                ctx.iso(row_problem_earliest).ok_or(Decline::Stale)?,
                age(now, row_problem_earliest),
            ))
        } else if used_review {
            review_source.clone()
        } else {
            None
        };
        if let Some((as_of, age_seconds)) = traced
            && let Some(obj) = row.as_object_mut()
        {
            obj.insert("sourceState".to_owned(), json!("unreadable"));
            obj.insert("asOf".to_owned(), as_of);
            obj.insert("ageSeconds".to_owned(), age_seconds);
        }
        out.push(row);
    }
    Ok(out)
}

fn judged_rows(base: &[V], task: &V, usage: &V, catalog: &V, ctx: &ClockCtx<'_>) -> Option<Vec<V>> {
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
        for ((ri, ci), out) in positions
            .into_iter()
            .zip(judge(&columns, usage, task, catalog, ctx)?)
        {
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
                    .unwrap_or_else(|| c.clone())
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
        let mut row = json!({
            "key": base["key"],
            "model": base["model"],
            "label": catalog_model.as_ref().map(|m| m["label"].clone()).unwrap_or_else(|| base["model"].clone()),
            "inCatalog": catalog_model.is_some(),
            "runtime": base["runtime"],
            "effort": base["effort"],
            "process": base["process"],
            "n": {
                "observed": observed["n"].as_u64().unwrap_or(0),
                "benchmark": benchmarks.iter().filter_map(|c| c["n"].as_u64()).sum::<u64>(),
            },
            "basis": headline.map(|h| h["source"].clone()),
            "benchmark": headline.map(|h| h["benchmark"].clone()),
            "ranked": headline.is_some(),
            "rank": headline.map(|h| h["rank"].clone()),
            "verdict": shown["verdict"],
            "verdictLabel": shown["verdictLabel"],
            "why": why,
            "observed": observed,
            "benchmarks": benchmarks,
        });
        if let Some(source_state) = base.get("sourceState")
            && let Some(obj) = row.as_object_mut()
        {
            obj.insert("sourceState".to_owned(), source_state.clone());
            obj.insert(
                "asOf".to_owned(),
                base.get("asOf").cloned().unwrap_or(V::Null),
            );
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
                (number(&b["n"]["observed"]).unwrap_or(0.0)
                    + number(&b["n"]["benchmark"]).unwrap_or(0.0))
                .total_cmp(
                    &(number(&a["n"]["observed"]).unwrap_or(0.0)
                        + number(&a["n"]["benchmark"]).unwrap_or(0.0)),
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
                s(&p["file"])
                    .strip_prefix("writers/")
                    .unwrap_or(s(&p["file"])),
                s(&p["reason"])
            )
        })
        .collect();
    if problems.len() > 5 {
        v.push(format!("and {} more", problems.len() - 5));
    }
    v.join(", ")
}

fn source_base(id: &str, label: &str, at: Option<i64>, now: i64, ctx: &ClockCtx<'_>) -> Option<V> {
    Some(json!({
        "id": id,
        "label": label,
        "asOf": ctx.iso(at)?,
        "ageSeconds": age(now, at),
        "staleAfterSeconds": 180,
    }))
}

fn source_finish(mut base: V, state: &str, detail: String) -> V {
    base["state"] = json!(state);
    base["detail"] = json!(detail);
    base
}

fn counted(counts: &[(String, usize)], ctx: &ClockCtx<'_>) -> String {
    let mut counts = counts.to_vec();
    counts.sort_by(|a, b| ctx.compare(&a.0, &b.0));
    counts
        .iter()
        .take(8)
        .map(|(s, n)| format!("{s} ({})", plural(*n, "run")))
        .collect::<Vec<_>>()
        .join(", ")
}

fn catalog_verdict_health(catalog: &V, now: i64, ctx: &ClockCtx<'_>) -> Option<(V, V, V)> {
    if catalog["state"] == "error" {
        let failed = ctx.epoch(&catalog["failedSince"]).ok().flatten();
        let scanned = ctx.epoch(&catalog["scannedAt"]).ok().flatten();
        let at = failed.or(scanned);
        return Some((json!("error"), ctx.iso(at)?, age(now, at)));
    }
    if catalog["state"] != "ok" {
        return None;
    }
    let at = array(&catalog["problems"])
        .iter()
        .filter_map(|problem| ctx.epoch(&problem["since"]).ok().flatten())
        .min()?;
    Some((json!("unreadable"), ctx.iso(Some(at))?, age(now, Some(at))))
}

#[allow(clippy::too_many_arguments)]
fn runs_source(
    root: &str,
    runs_state: &str,
    total_runs: usize,
    total_bench: usize,
    writers_count: usize,
    unscored_no_model_or_type: usize,
    ignored_reviews: usize,
    unknown_outcomes: &[(String, usize)],
    negative_quota: &[(u64, u32, String)],
    problems: &[V],
    now: i64,
    ctx: &ClockCtx<'_>,
) -> Option<V> {
    let at = Some(now);
    let base = source_base("scoreboard-runs", "Agent runs", at, now, ctx)?;
    if runs_state == "disabled" {
        return Some(source_finish(
            base,
            "missing",
            "The run history store is disabled.".into(),
        ));
    }
    let at = at?;
    if runs_state == "missing" {
        return Some(source_finish(
            base,
            "missing",
            format!("No agent reports folder at {root}."),
        ));
    }
    if runs_state == "error" {
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
            format!(
                "The agent reports folder has not answered since {}.",
                ctx.clock(at)?
            ),
        ));
    }
    let mut parts = vec![format!(
        "{} from {} read {} ({} observed, {total_bench} benchmark).",
        plural(total_runs, "run"),
        plural(writers_count, "writer"),
        ctx.clock(at)?,
        total_runs - total_bench,
    )];
    if unscored_no_model_or_type > 0 {
        parts.push(format!(
            "{} with no model or task type not scored.",
            plural(unscored_no_model_or_type, "run")
        ));
    }
    if ignored_reviews > 0 {
        parts.push(format!(
            "{} ignored: not a review run, no earlier known run, or its own run not scored.",
            plural(ignored_reviews, "review")
        ));
    }
    if !unknown_outcomes.is_empty() {
        let sum: usize = unknown_outcomes.iter().map(|(_, n)| *n).sum();
        parts.push(format!(
            "{} with an outcome that is neither done nor a failure, left out of reliability, overclaim and speed and counted as no accepted units: {}.",
            plural(sum, "run"),
            counted(unknown_outcomes, ctx)
        ));
    }
    if !negative_quota.is_empty() {
        parts.push(format!(
            "{} with a negative quota move left out of the quota rates: {}{}.",
            plural(negative_quota.len(), "run"),
            negative_quota
                .iter()
                .take(5)
                .map(|(_, _, id)| id.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            if negative_quota.len() > 5 {
                format!(", and {} more", negative_quota.len() - 5)
            } else {
                String::new()
            }
        ));
    }
    let files: Vec<_> = problems
        .iter()
        .filter(|p| !s(&p["file"]).starts_with("limits/"))
        .cloned()
        .collect();
    if !files.is_empty() {
        let mut since: Option<i64> = None;
        for p in &files {
            let e = ctx.epoch(&p["since"]).ok()?;
            if let Some(m) = e
                && since.is_none_or(|prev| m < prev)
            {
                since = Some(m);
            }
        }
        let since = since?;
        parts.insert(
            0,
            format!(
                "{} unreadable since {}: {}.",
                plural(files.len(), "file"),
                ctx.clock(since)?,
                problem_list(&files)
            ),
        );
    }
    for p in problems
        .iter()
        .filter(|p| s(&p["file"]).starts_with("limits/"))
    {
        parts.push(s(&p["reason"]).to_owned());
    }
    Some(source_finish(
        base,
        if files.is_empty() { "ok" } else { "unreadable" },
        parts.join(" "),
    ))
}

fn catalog_source(
    catalog: &V,
    seed_failures: &[V],
    missing_types: &[(String, usize)],
    missing_models: &[(String, usize)],
    now: i64,
    ctx: &ClockCtx<'_>,
) -> Option<V> {
    let mut source = catalog_source_of(
        catalog,
        seed_failures,
        missing_types,
        missing_models,
        now,
        ctx,
    )?;
    if catalog["partial"] == true {
        source["detail"] = json!(format!(
            "{} The scan is still running; this view is partial.",
            s(&source["detail"])
        ));
    }
    Some(source)
}

fn catalog_source_of(
    catalog: &V,
    seed_failures: &[V],
    missing_types: &[(String, usize)],
    missing_models: &[(String, usize)],
    now: i64,
    ctx: &ClockCtx<'_>,
) -> Option<V> {
    let at = ctx.epoch(&catalog["scannedAt"]).ok()?;
    let base = source_base("scoreboard-catalog", "Scoreboard catalog", at, now, ctx)?;
    let Some(at) = at else {
        return Some(source_finish(
            base,
            "missing",
            "Waiting for the first scan.".into(),
        ));
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
            format!(
                "The catalog folder has not answered since {}.",
                ctx.clock(at)?
            ),
        ));
    }
    let missing = array(&catalog["missingFiles"]);
    let not_added = seed_failures
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
        let mut since: Option<i64> = None;
        for p in &problems {
            let e = ctx.epoch(&p["since"]).ok()?;
            if let Some(m) = e
                && since.is_none_or(|prev| m < prev)
            {
                since = Some(m);
            }
        }
        let since = since?;
        return Some(source_finish(
            base,
            "unreadable",
            format!(
                "{} unreadable since {}: {}.{not_added}",
                plural(problems.len(), "catalog file"),
                ctx.clock(since)?,
                problem_list(&problems)
            ),
        ));
    }
    if !missing.is_empty() {
        let seeds = seed_failures
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
    let mut parts = vec![format!("Catalog read {}.", ctx.clock(at)?)];
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
    Some(source_finish(
        base,
        "ok",
        format!("{}{not_added}", parts.join(" ")),
    ))
}

// Match `read_scoreboard::native_input`: emit integral JS Numbers as integer tokens before
// deserializing `aac_status::scoreboard::ViewIn`.
fn native_input(value: V) -> V {
    match value {
        V::Number(number) => {
            if let Some(value) = number.as_f64()
                && value.fract() == 0.0
                && value.abs() <= 9_007_199_254_740_991.0
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

/// Compose one complete `/api/scoreboard` view from pre-aggregated store buckets + live catalog.
pub(crate) fn compose(input: &ComposeInput<'_>) -> Result<ComposedView, Decline> {
    let ctx = ClockCtx::new(input.time_zone, input.now, input.now_clock).ok_or(Decline::Shape)?;
    let problems = build_problems(input.read, input.pass_problems, input.safe_paths);

    // Prototype-name gate on all small metadata (same rule as `compose_fed_parts` + `held_docs`).
    if is_proto_str(input.root)
        || is_proto_str(input.time_zone)
        || input.filter_from.is_some_and(is_proto_str)
        || input.filter_to.is_some_and(is_proto_str)
        || input.filter_task_type.is_some_and(is_proto_str)
        || proto_named(input.catalog)
        || proto_named(&input.read.identity)
        || problems.iter().any(proto_named)
        || input.seed_failures.iter().any(proto_named)
    {
        return Err(Decline::Unsupported);
    }

    let models = input.catalog["models"].as_array().ok_or(Decline::Stale)?;
    let types = input.catalog["taskTypes"]
        .as_array()
        .ok_or(Decline::Stale)?;
    let known_models: HashSet<&str> = models.iter().map(|m| s(&m["model"])).collect();
    let known_types: HashSet<&str> = types.iter().map(|t| s(&t["id"])).collect();

    let efforts_buf = collect_catalog_efforts(input.catalog);
    let empty_aliases = Map::new();
    let mut norm = CatalogNorm::new(input.catalog, &efforts_buf, &empty_aliases);

    let (mut no_model, mut no_type, mut unknown_model, mut unknown_type) =
        (0usize, 0usize, 0usize, 0usize);
    let mut missing_types: BTreeMap<String, usize> = BTreeMap::new();
    let mut missing_models: BTreeMap<String, usize> = BTreeMap::new();
    let mut unknown_tally: BTreeMap<String, usize> = BTreeMap::new();
    let mut unknown_outcomes: BTreeMap<String, usize> = BTreeMap::new();
    let mut negative_quota: Vec<(u64, u32, String)> = Vec::new();
    let mut writers: BTreeSet<u32> = BTreeSet::new();
    let mut total_runs = 0usize;
    let mut total_bench = 0usize;
    let mut ignored_reviews = 0usize;
    let mut by_type: HashMap<String, Vec<BucketRef<'_>>> = HashMap::new();
    let mut recent_candidates: Vec<(&BucketRunSummary, String, Option<String>)> = Vec::new();
    let mut unscored_examples: Vec<(&BucketRunSummary, &'static str)> = Vec::new();

    for (bkey, agg) in &input.read.buckets {
        if agg.proto_count > 0 {
            return Err(Decline::Unsupported);
        }
        let raw_model_null = bkey.model_id.is_empty();
        let raw_model_id = bkey.model_id.as_str();
        let kind = bkey.task_type.as_str();
        let (norm_id, norm_effort) = norm.normalize(
            raw_model_id,
            (!bkey.effort.is_empty()).then_some(bkey.effort.as_str()),
        );
        if is_proto_str(&norm_id) || norm_effort.as_deref().is_some_and(is_proto_str) {
            return Err(Decline::Unsupported);
        }

        let bucket_n = agg.n as usize;
        total_runs += bucket_n;
        if bkey.source_benchmark {
            total_bench += bucket_n;
        }
        writers.extend(agg.writers.iter().copied());
        for (word, count) in &agg.unknown_outcomes {
            *unknown_outcomes.entry(word.clone()).or_insert(0) += *count as usize;
        }
        negative_quota.extend(agg.negative_quota_runs.iter().cloned());
        for run in &agg.recent_runs {
            recent_candidates.push((run, norm_id.clone(), norm_effort.clone()));
        }

        // Exact precedence of `read_scoreboard.rs:1081-1106`.
        if !norm_id.is_empty() && !kind.is_empty() && !known_types.contains(kind) {
            *missing_types.entry(kind.to_owned()).or_insert(0) += bucket_n;
        } else if !norm_id.is_empty()
            && !kind.is_empty()
            && !known_models.contains(norm_id.as_str())
        {
            *missing_models.entry(norm_id.clone()).or_insert(0) += bucket_n;
        }

        let bucket_scored = !raw_model_null
            && !kind.is_empty()
            && !norm_id.is_empty()
            && known_models.contains(norm_id.as_str())
            && known_types.contains(kind);

        // Count `index.ignored` for reviews inside the filtered buckets.
        for own in &agg.own_review_refs {
            let target_in_filter = own.target_effective
                && own.target_r.is_some()
                && own.target_r != Some(own.self_r)
                && input
                    .filter
                    .from_ms
                    .is_none_or(|f| own.target_cursor_ms.is_some_and(|c| c >= f))
                && input
                    .filter
                    .to_ms
                    .is_none_or(|t| own.target_cursor_ms.is_some_and(|c| c <= t))
                && input
                    .filter
                    .source_benchmark
                    .is_none_or(|b| own.target_source_benchmark == Some(b))
                && input
                    .filter
                    .task_type
                    .as_deref()
                    .is_none_or(|tt| own.target_task_type.as_deref() == Some(tt));
            let valid = bucket_scored
                && own.self_task_type_is_review
                && target_in_filter
                && own
                    .target_ended_epoch
                    .is_some_and(|te| own.self_ended_epoch >= te);
            if !valid {
                ignored_reviews += 1;
            }
        }

        let reason = if raw_model_null {
            no_model += bucket_n;
            Some("no-model")
        } else if kind.is_empty() {
            no_type += bucket_n;
            Some("no-task-type")
        } else if norm_id.is_empty() || !known_models.contains(norm_id.as_str()) {
            unknown_model += bucket_n;
            *unknown_tally.entry(raw_model_id.to_owned()).or_insert(0) += bucket_n;
            Some("unknown-model")
        } else if !known_types.contains(kind) {
            unknown_type += bucket_n;
            Some("unknown-task-type")
        } else {
            by_type.entry(kind.to_owned()).or_default().push(BucketRef {
                key: bkey,
                agg,
                norm_model: norm_id,
                norm_effort,
            });
            None
        };
        if let Some(reason) = reason {
            for run in &agg.recent_runs {
                unscored_examples.push((run, reason));
            }
        }
    }

    // Order `negative_quota` by `(cursor_ms, r)` to match `RUN_BY_TIME` doc iteration.
    negative_quota.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    let unreadable = problems
        .iter()
        .filter(|p| {
            let file = s(&p["file"]);
            file.starts_with("writers/")
                && file.contains("/runs/")
                && file.ends_with(".json")
                && !input.read.problem_trace.retained_run_files.contains(file)
        })
        .count();
    let scan_limit = 0usize;

    let missing_types_vec: Vec<(String, usize)> = missing_types.into_iter().collect();
    let missing_models_vec: Vec<(String, usize)> = missing_models.into_iter().collect();
    let mut unknown_counts: Vec<(String, usize)> = unknown_tally.into_iter().collect();
    unknown_counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| ctx.compare(&a.0, &b.0)));

    unscored_examples
        .sort_by(|a, b| newest_cmp(a.0.ended_epoch, &a.0.id, b.0.ended_epoch, &b.0.id));
    let unscored = json!({
        "total": scan_limit + unreadable + no_model + no_type + unknown_model + unknown_type,
        "scanLimit": scan_limit,
        "unreadable": unreadable,
        "noModel": no_model,
        "noTaskType": no_type,
        "unknownModel": unknown_model,
        "unknownTaskType": unknown_type,
        "unknownModels": unknown_counts
            .iter()
            .take(20)
            .map(|(id, n)| json!({"id": id, "runs": n}))
            .collect::<Vec<_>>(),
        "examples": unscored_examples
            .iter()
            .take(10)
            .map(|(run, reason)| json!({
                "id": run.id,
                "model": run.model_id,
                "taskType": run.task_type,
                "reason": reason,
                "endedAt": run.ended_at,
            }))
            .collect::<Vec<_>>(),
    });

    let review_source = if input.read.problem_trace.review_source_matched {
        let earliest = input.read.problem_trace.review_source_earliest;
        Some((
            ctx.iso(earliest).ok_or(Decline::Stale)?,
            age(input.now, earliest),
        ))
    } else {
        None
    };
    let catalog_health = catalog_verdict_health(input.catalog, input.now, &ctx);

    let mut bases: HashMap<String, Vec<V>> = HashMap::new();
    for task in types {
        let tid = s(&task["id"]);
        let entries = by_type.get(tid).map(Vec::as_slice).unwrap_or(&[]);
        bases.insert(
            tid.to_owned(),
            base_rows(
                task,
                entries,
                input.catalog,
                input.filter,
                &known_models,
                &known_types,
                &mut norm,
                input.read,
                &review_source,
                input.now,
                &ctx,
            )?,
        );
    }

    let mut uses = Vec::new();
    for usage in input.catalog["uses"].as_array().ok_or(Decline::Stale)? {
        let requested = array(&usage["taskTypes"]);
        let wanted: Vec<&V> = if requested.contains(&json!("*")) {
            types
                .iter()
                .filter(|t| by_type.contains_key(s(&t["id"])))
                .collect()
        } else {
            requested
                .iter()
                .filter_map(|id| types.iter().find(|t| t["id"] == *id))
                .collect()
        };
        let mut task_types_out = Vec::new();
        for task in wanted {
            let base = bases.get(s(&task["id"])).ok_or(Decline::Stale)?;
            let mut rows =
                judged_rows(base, task, usage, input.catalog, &ctx).ok_or(Decline::Stale)?;
            if let Some((state, as_of, age_seconds)) = &catalog_health {
                for row in &mut rows {
                    if row["ranked"] == true
                        && row.get("sourceState").is_none()
                        && let Some(obj) = row.as_object_mut()
                    {
                        obj.insert("sourceState".to_owned(), state.clone());
                        obj.insert("asOf".to_owned(), as_of.clone());
                        obj.insert("ageSeconds".to_owned(), age_seconds.clone());
                    }
                }
            }
            task_types_out.push(json!({
                "id": task["id"],
                "label": task["label"],
                "description": task["description"],
                "unit": task["unit"],
                "measures": task["measures"],
                "byProcess": task["byProcess"],
                "rows": rows,
            }));
        }
        uses.push(json!({
            "id": usage["id"],
            "label": usage["label"],
            "description": usage["description"],
            "taskTypes": task_types_out,
        }));
    }

    recent_candidates
        .sort_by(|a, b| newest_cmp(a.0.ended_epoch, &a.0.id, b.0.ended_epoch, &b.0.id));
    let recent: Vec<V> = recent_candidates
        .into_iter()
        .take(20)
        .map(|(r, m, e)| summary_json(r, &m, e.as_deref(), models))
        .collect();

    let unknown_outcomes_vec: Vec<(String, usize)> = unknown_outcomes.into_iter().collect();
    let runs_state = if input.pass_state.is_empty() {
        "ok"
    } else {
        input.pass_state
    };
    let src_runs = runs_source(
        input.root,
        runs_state,
        total_runs,
        total_bench,
        writers.len(),
        no_model + no_type,
        ignored_reviews,
        &unknown_outcomes_vec,
        &negative_quota,
        &problems,
        input.now,
        &ctx,
    )
    .ok_or(Decline::Stale)?;
    let src_catalog = catalog_source(
        input.catalog,
        input.seed_failures,
        &missing_types_vec,
        &missing_models_vec,
        input.now,
        &ctx,
    )
    .ok_or(Decline::Stale)?;

    let filter_task_types: Vec<V> = types
        .iter()
        .map(|row| json!({"id": row["id"], "label": row["label"]}))
        .collect();

    let mut view = json!({
        "version": 1,
        "generatedAt": ctx.iso(Some(input.now)).ok_or(Decline::Stale)?,
        "refresh": {"pollSeconds": 60},
        "sources": [src_runs, src_catalog],
        "catalog": {
            "minRuns": input.catalog["minRuns"],
            "models": input.catalog["models"],
            "taskTypes": input.catalog["taskTypes"],
            "measures": input.catalog["measures"],
            "uses": input.catalog["uses"],
            "benchmarks": input.catalog["benchmarks"],
        },
        "uses": uses,
        "recent": recent,
        "unscored": unscored,
    });
    view["filters"] = json!({
        "from": input.filter_from,
        "to": input.filter_to,
        "source": input.filter_source,
        "taskType": input.filter_task_type,
        "available": true,
        "taskTypes": filter_task_types,
    });
    view["identity"] = input.read.identity.clone();
    view["history"] = json!({
        "store": "on",
        "runs": input.read.effective_runs_total,
        "importing": input.importing,
    });

    let typed =
        serde_json::from_value::<aac_status::scoreboard::ViewIn>(native_input(view.clone()))
            .map_err(|_| Decline::Stale)?;
    if let Ok(by_model) = aac_status::scoreboard::by_model(&typed) {
        view["byModel"] = serde_json::to_value(by_model).map_err(|_| Decline::Stale)?;
        if let Ok(groups) = aac_status::scoreboard::group_keys(&typed) {
            view["groups"] = serde_json::to_value(groups).map_err(|_| Decline::Stale)?;
        }
    }
    view["cached"] = json!(true);
    view["cacheAgeSeconds"] = json!(0);
    view["refreshing"] = json!(false);

    Ok(ComposedView { view, problems })
}
