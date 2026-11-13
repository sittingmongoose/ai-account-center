//! Scoreboard v2 on the page: the by-model rows filtered, grouped Family > Model > Effort and
//! sorted. The server decides every key (native/aac-status `group_keys`); this only orders.
//! Families keep their order, a model's efforts stay together in effort order, and a sort
//! orders models within a family (or every row when grouping is off).

use serde::Deserialize;
use std::cmp::Ordering;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Groups {
    pub families: Vec<FamilyRef>,
    pub efforts: Vec<String>,
    /// View id to row key (`model|runtime|effort`) to its keys.
    pub views: BTreeMap<String, BTreeMap<String, RowKeys>>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FamilyRef {
    pub id: String,
    pub label: String,
    pub short: String,
    pub order: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RowKeys {
    pub family: String,
    pub model_order: i64,
    pub effort_order: i64,
    /// Cells recommended, fine and avoid.
    pub verdicts: Vec<i64>,
    pub rank: Option<i64>,
    pub runs: i64,
    pub passed: Option<i64>,
    pub decided: Option<i64>,
    pub cost: Option<f64>,
    pub cost_unit: Option<String>,
    pub cost_job: Option<String>,
    pub last_run_at: Option<String>,
}

impl RowKeys {
    fn verdict(&self, i: usize) -> i64 {
        self.verdicts.get(i).copied().unwrap_or(0)
    }
    pub fn pass_rate(&self) -> Option<f64> {
        match (self.passed, self.decided) {
            (Some(p), Some(d)) if d > 0 => Some(p as f64 / d as f64),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Verdict,
    Rank,
    Runs,
    PassRate,
    Cost,
    Recency,
}

/// (key, id, label) in the menu's order.
pub const SORTS: [(SortKey, &str, &str); 6] = [
    (SortKey::Verdict, "verdict", "Verdict"),
    (SortKey::Rank, "rank", "Rank"),
    (SortKey::Runs, "runs", "Runs"),
    (SortKey::PassRate, "pass", "Pass rate"),
    (SortKey::Cost, "cost", "Cost"),
    (SortKey::Recency, "recent", "Recent"),
];

impl SortKey {
    pub fn id(self) -> &'static str {
        SORTS
            .iter()
            .find(|s| s.0 == self)
            .map(|s| s.1)
            .unwrap_or("verdict")
    }
    pub fn from_id(id: &str) -> SortKey {
        SORTS
            .iter()
            .find(|s| s.1 == id)
            .map(|s| s.0)
            .unwrap_or(SortKey::Verdict)
    }
}

/// Unknown values sort after known ones whatever the direction.
fn known_first<T>(a: Option<T>, b: Option<T>, cmp: impl Fn(T, T) -> Ordering) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => cmp(x, y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn by_verdict(a: &RowKeys, b: &RowKeys) -> Ordering {
    b.verdict(0)
        .cmp(&a.verdict(0))
        .then(b.verdict(1).cmp(&a.verdict(1)))
        .then(a.verdict(2).cmp(&b.verdict(2)))
        .then(b.runs.cmp(&a.runs))
}

/// Best first for the chosen sort.
pub fn compare(sort: SortKey, a: &RowKeys, b: &RowKeys) -> Ordering {
    let primary = match sort {
        SortKey::Verdict => by_verdict(a, b),
        SortKey::Rank => known_first(a.rank, b.rank, |x, y| x.cmp(&y)),
        SortKey::Runs => b.runs.cmp(&a.runs),
        SortKey::PassRate => known_first(a.pass_rate(), b.pass_rate(), |x, y| y.total_cmp(&x))
            .then(b.decided.unwrap_or(0).cmp(&a.decided.unwrap_or(0))),
        SortKey::Cost => known_first(a.cost, b.cost, |x, y| x.total_cmp(&y)),
        SortKey::Recency => known_first(
            a.last_run_at.as_deref(),
            b.last_run_at.as_deref(),
            |x, y| y.cmp(x),
        ),
    };
    primary.then_with(|| by_verdict(a, b))
}

/// What the page's family, model and effort filters hold (`Some("")` is "effort not recorded").
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RowFilter {
    pub family: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl RowFilter {
    pub fn active(&self) -> usize {
        [&self.family, &self.model, &self.effort]
            .iter()
            .filter(|v| v.is_some())
            .count()
    }
}

/// A by-model row as this module needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct RowRef {
    pub key: String,
    pub label: String,
}

pub fn model_of(key: &str) -> &str {
    key.split('|').next().unwrap_or("")
}

pub fn effort_of(key: &str) -> &str {
    key.splitn(3, '|').nth(2).unwrap_or("")
}

/// One line of the arranged list.
#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    Family {
        id: String,
        label: String,
        short: String,
        runs: i64,
    },
    /// A model with more than one effort row: its name once, its efforts below. `first` is
    /// the index of its first row (for its mark).
    Model {
        model: String,
        label: String,
        first: usize,
    },
    /// A row by its index in the input; `sub` when it sits under a Model line.
    Row { index: usize, sub: bool },
}

const NONE: &str = "none";

fn family_of<'a>(keys: Option<&'a BTreeMap<String, RowKeys>>, key: &str) -> &'a str {
    keys.and_then(|k| k.get(key))
        .map(|k| k.family.as_str())
        .unwrap_or(NONE)
}

/// Rows the filters keep, by index.
pub fn kept(
    rows: &[RowRef],
    keys: Option<&BTreeMap<String, RowKeys>>,
    pick: &RowFilter,
) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, r)| {
            pick.family
                .as_deref()
                .is_none_or(|f| family_of(keys, &r.key) == f)
                && pick.model.as_deref().is_none_or(|m| model_of(&r.key) == m)
                && pick
                    .effort
                    .as_deref()
                    .is_none_or(|e| effort_of(&r.key) == e)
        })
        .map(|(i, _)| i)
        .collect()
}

/// The rows filtered, grouped and sorted.
pub fn arrange(
    rows: &[RowRef],
    groups: Option<&Groups>,
    view_id: &str,
    sort: SortKey,
    group: bool,
    pick: &RowFilter,
) -> Vec<Line> {
    let keys = groups.and_then(|g| g.views.get(view_id));
    let keep = kept(rows, keys, pick);
    let Some(keys) = keys else {
        // No keys from the server: its own order, ungrouped.
        return keep
            .into_iter()
            .map(|index| Line::Row { index, sub: false })
            .collect();
    };
    let blank = RowKeys::default();
    let k = |i: usize| keys.get(&rows[i].key).unwrap_or(&blank);
    let tie = |a: usize, b: usize| {
        k(a).model_order
            .cmp(&k(b).model_order)
            .then(rows[a].label.cmp(&rows[b].label))
            .then(k(a).effort_order.cmp(&k(b).effort_order))
            .then(rows[a].key.cmp(&rows[b].key))
    };
    if !group {
        let mut order = keep;
        order.sort_by(|&a, &b| compare(sort, k(a), k(b)).then_with(|| tie(a, b)));
        return order
            .into_iter()
            .map(|index| Line::Row { index, sub: false })
            .collect();
    }
    let families = groups.map(|g| g.families.clone()).unwrap_or_default();
    let mut out = Vec::new();
    let mut placed: Vec<&str> = families.iter().map(|f| f.id.as_str()).collect();
    if !placed.contains(&NONE) {
        placed.push(NONE);
    }
    for family in placed {
        let in_family: Vec<usize> = keep
            .iter()
            .copied()
            .filter(|&i| {
                k(i).family == family
                    || (family == NONE && !families.iter().any(|f| f.id == k(i).family))
            })
            .collect();
        if in_family.is_empty() {
            continue;
        }
        let info = families.iter().find(|f| f.id == family);
        out.push(Line::Family {
            id: family.to_string(),
            label: info
                .map(|f| f.label.clone())
                .unwrap_or_else(|| "Not in the catalog".into()),
            short: info
                .map(|f| f.short.clone())
                .unwrap_or_else(|| "Other".into()),
            runs: in_family.iter().map(|&i| k(i).runs).sum(),
        });
        // Models of the family, each led by its best row for the sort.
        let mut models: Vec<(String, Vec<usize>)> = Vec::new();
        for &i in &in_family {
            let model = model_of(&rows[i].key).to_string();
            match models.iter_mut().find(|(m, _)| *m == model) {
                Some((_, list)) => list.push(i),
                None => models.push((model, vec![i])),
            }
        }
        for (_, list) in models.iter_mut() {
            list.sort_by(|&a, &b| {
                k(a).effort_order
                    .cmp(&k(b).effort_order)
                    .then_with(|| tie(a, b))
            });
        }
        let best = |list: &[usize]| -> usize {
            *list
                .iter()
                .min_by(|&&a, &&b| compare(sort, k(a), k(b)).then_with(|| tie(a, b)))
                .expect("a model has rows")
        };
        models.sort_by(|(_, a), (_, b)| {
            let (x, y) = (best(a), best(b));
            compare(sort, k(x), k(y)).then_with(|| tie(x, y))
        });
        for (model, list) in models {
            let several = list.len() > 1;
            if several {
                out.push(Line::Model {
                    model,
                    label: rows[list[0]].label.clone(),
                    first: list[0],
                });
            }
            out.extend(list.into_iter().map(|index| Line::Row {
                index,
                sub: several,
            }));
        }
    }
    out
}

/// The filter menus' choices from every row: families in order, then models and efforts within
/// the chosen family (and model).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facets {
    pub families: Vec<(String, String)>,
    pub models: Vec<(String, String)>,
    /// (effort value, label); "" is "effort not recorded".
    pub efforts: Vec<(String, String)>,
}

pub fn facets(rows: &[RowRef], groups: Option<&Groups>, view_id: &str, pick: &RowFilter) -> Facets {
    let keys = groups.and_then(|g| g.views.get(view_id));
    let blank = RowKeys::default();
    let k = |key: &str| keys.and_then(|m| m.get(key)).unwrap_or(&blank);
    let mut families: Vec<(String, String)> = Vec::new();
    if let Some(g) = groups {
        for f in &g.families {
            if rows.iter().any(|r| family_of(keys, &r.key) == f.id) {
                families.push((f.id.clone(), f.label.clone()));
            }
        }
    }
    let in_family = |r: &&RowRef| {
        pick.family
            .as_deref()
            .is_none_or(|f| family_of(keys, &r.key) == f)
    };
    let mut models: Vec<(String, String, i64)> = Vec::new();
    for r in rows.iter().filter(in_family) {
        let m = model_of(&r.key);
        if !models.iter().any(|(id, _, _)| id == m) {
            models.push((m.to_string(), r.label.clone(), k(&r.key).model_order));
        }
    }
    models.sort_by(|a, b| a.2.cmp(&b.2).then(a.1.cmp(&b.1)));
    let mut efforts: Vec<(String, String, i64)> = Vec::new();
    for r in rows
        .iter()
        .filter(in_family)
        .filter(|r| pick.model.as_deref().is_none_or(|m| model_of(&r.key) == m))
    {
        let e = effort_of(&r.key);
        if !efforts.iter().any(|(id, _, _)| id == e) {
            let label = if e.is_empty() {
                "Not recorded".to_string()
            } else {
                e.to_string()
            };
            efforts.push((e.to_string(), label, k(&r.key).effort_order));
        }
    }
    efforts.sort_by(|a, b| a.2.cmp(&b.2).then(a.0.cmp(&b.0)));
    Facets {
        families,
        models: models
            .into_iter()
            .map(|(id, label, _)| (id, label))
            .collect(),
        efforts: efforts
            .into_iter()
            .map(|(id, label, _)| (id, label))
            .collect(),
    }
}

/// A pass rate as the page prints it ("71%"), or "–".
pub fn pass_text(keys: Option<&RowKeys>) -> String {
    keys.and_then(RowKeys::pass_rate)
        .map(|r| format!("{}%", (r * 100.0).round() as i64))
        .unwrap_or_else(|| "–".into())
}

/// The server's filters on the page (dates, real or benchmark, task type): each change reads
/// the view again with them as the query string.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerFilter {
    /// all, 7, 30, 90 or custom.
    pub dates: String,
    pub from: String,
    pub to: String,
    /// all, real or benchmark.
    pub source: String,
    /// A task type id, or empty for all.
    pub task: String,
}

impl Default for ServerFilter {
    fn default() -> Self {
        ServerFilter {
            dates: "all".into(),
            from: String::new(),
            to: String::new(),
            source: "all".into(),
            task: String::new(),
        }
    }
}

pub const DATES: [(&str, &str); 5] = [
    ("all", "All time"),
    ("7", "Last 7 days"),
    ("30", "Last 30 days"),
    ("90", "Last 90 days"),
    ("custom", "Custom"),
];

pub const SOURCES: [(&str, &str); 3] = [
    ("all", "Real + bench"),
    ("real", "Real work"),
    ("benchmark", "Benchmarks"),
];

fn is_day(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

impl ServerFilter {
    /// The filters that differ from every run.
    pub fn active(&self) -> usize {
        usize::from(self.dates != "all")
            + usize::from(self.source != "all")
            + usize::from(!self.task.is_empty())
    }

    /// The query string: `day(n)` is the local date n days ago. A preset of n days is from
    /// `day(n - 1)` on; a custom range sends only real days, earlier first.
    pub fn query(&self, day: impl Fn(i64) -> String) -> String {
        let mut parts: Vec<String> = Vec::new();
        match self.dates.as_str() {
            "7" | "30" | "90" => {
                let n: i64 = self.dates.parse().unwrap_or(1);
                parts.push(format!("from={}", day(n - 1)));
            }
            "custom" => {
                let (mut from, mut to) = (self.from.clone(), self.to.clone());
                if is_day(&from) && is_day(&to) && from > to {
                    std::mem::swap(&mut from, &mut to);
                }
                if is_day(&from) {
                    parts.push(format!("from={from}"));
                }
                if is_day(&to) {
                    parts.push(format!("to={to}"));
                }
            }
            _ => {}
        }
        if self.source == "real" || self.source == "benchmark" {
            parts.push(format!("source={}", self.source));
        }
        if !self.task.is_empty()
            && self
                .task
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            parts.push(format!("taskType={}", self.task));
        }
        parts.join("&")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(
        family: &str,
        model_order: i64,
        effort_order: i64,
        verdicts: [i64; 3],
        runs: i64,
    ) -> RowKeys {
        RowKeys {
            family: family.into(),
            model_order,
            effort_order,
            verdicts: verdicts.to_vec(),
            runs,
            ..RowKeys::default()
        }
    }

    /// Opus (xhigh, high), Sol (max, medium, none), GLM max, an unplaced model.
    fn fixture() -> (Vec<RowRef>, Groups) {
        let rows: Vec<RowRef> = [
            ("openai/gpt-6.1-sol|codex|max", "Sol 6.1"),
            ("anthropic/claude-opus-5-5|claude-code|high", "Opus 5.5"),
            ("zai/glm-5.3|zcode|max", "GLM 5.3"),
            ("openai/gpt-6.1-sol|codex|", "Sol 6.1"),
            ("anthropic/claude-opus-5-5|claude-code|xhigh", "Opus 5.5"),
            ("openai/gpt-6.1-sol|codex|medium", "Sol 6.1"),
            ("acme/x|omp|", "X"),
            ("openai/gpt-6-astra|codex|max", "Astra"),
        ]
        .iter()
        .map(|(k, l)| RowRef {
            key: k.to_string(),
            label: l.to_string(),
        })
        .collect();
        let mut map = BTreeMap::new();
        map.insert(
            "openai/gpt-6.1-sol|codex|max".into(),
            RowKeys {
                rank: Some(1),
                last_run_at: Some("2026-11-13T10:00:00Z".into()),
                passed: Some(9),
                decided: Some(10),
                ..keys("openai", 4, 6, [2, 0, 0], 358)
            },
        );
        map.insert(
            "anthropic/claude-opus-5-5|claude-code|high".into(),
            RowKeys {
                passed: Some(5),
                decided: Some(10),
                cost: Some(0.5),
                ..keys("anthropic", 0, 4, [0, 1, 0], 139)
            },
        );
        map.insert(
            "zai/glm-5.3|zcode|max".into(),
            RowKeys {
                cost: Some(0.1),
                ..keys("zai", 8, 6, [0, 0, 1], 18)
            },
        );
        map.insert(
            "openai/gpt-6.1-sol|codex|".into(),
            keys("openai", 4, 100, [0, 0, 0], 416),
        );
        map.insert(
            "anthropic/claude-opus-5-5|claude-code|xhigh".into(),
            RowKeys {
                rank: Some(2),
                last_run_at: Some("2026-11-13T12:00:00Z".into()),
                ..keys("anthropic", 0, 5, [1, 1, 0], 457)
            },
        );
        map.insert(
            "openai/gpt-6.1-sol|codex|medium".into(),
            keys("openai", 4, 3, [0, 1, 0], 97),
        );
        map.insert(
            "acme/x|omp|".into(),
            keys("none", 10_000, 100, [0, 0, 0], 3),
        );
        map.insert(
            "openai/gpt-6-astra|codex|max".into(),
            RowKeys {
                rank: Some(1),
                ..keys("openai", 5, 6, [1, 0, 0], 319)
            },
        );
        let groups = Groups {
            families: vec![
                FamilyRef {
                    id: "anthropic".into(),
                    label: "Anthropic · Claude".into(),
                    short: "Claude".into(),
                    order: 1,
                },
                FamilyRef {
                    id: "openai".into(),
                    label: "OpenAI · GPT, Codex".into(),
                    short: "GPT".into(),
                    order: 2,
                },
                FamilyRef {
                    id: "zai".into(),
                    label: "Z.ai · GLM".into(),
                    short: "GLM".into(),
                    order: 5,
                },
                FamilyRef {
                    id: "none".into(),
                    label: "Not in the catalog".into(),
                    short: "Other".into(),
                    order: 1000,
                },
            ],
            efforts: vec![],
            views: [("pipeline".to_string(), map)].into_iter().collect(),
        };
        (rows, groups)
    }

    fn shape(rows: &[RowRef], lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| match l {
                Line::Family { id, runs, .. } => format!("# {id} {runs}"),
                Line::Model { label, .. } => format!("= {label}"),
                Line::Row { index, sub } => {
                    format!("{}{}", if *sub { "  " } else { "" }, rows[*index].key)
                }
            })
            .collect()
    }

    #[test]
    fn families_in_order_models_together_and_efforts_in_order() {
        let (rows, groups) = fixture();
        let lines = arrange(
            &rows,
            Some(&groups),
            "pipeline",
            SortKey::Verdict,
            true,
            &RowFilter::default(),
        );
        assert_eq!(
            shape(&rows, &lines),
            vec![
                "# anthropic 596",
                "= Opus 5.5",
                "  anthropic/claude-opus-5-5|claude-code|high",
                "  anthropic/claude-opus-5-5|claude-code|xhigh",
                "# openai 1190",
                // Sol's best row (max: two recommended) leads Astra's (one).
                "= Sol 6.1",
                "  openai/gpt-6.1-sol|codex|medium",
                "  openai/gpt-6.1-sol|codex|max",
                "  openai/gpt-6.1-sol|codex|",
                "openai/gpt-6-astra|codex|max",
                "# zai 18",
                "zai/glm-5.3|zcode|max",
                "# none 3",
                "acme/x|omp|",
            ]
        );
    }

    #[test]
    fn a_sort_orders_models_within_a_family_and_every_row_when_ungrouped() {
        let (rows, groups) = fixture();
        let runs = arrange(
            &rows,
            Some(&groups),
            "pipeline",
            SortKey::Runs,
            true,
            &RowFilter::default(),
        );
        let openai: Vec<String> = shape(&rows, &runs)
            .into_iter()
            .skip_while(|l| l != "# openai 1190")
            .skip(1)
            .take(5)
            .collect();
        assert_eq!(openai[0], "= Sol 6.1", "Sol has the row with the most runs");
        let flat = |sort| -> Vec<String> {
            shape(
                &rows,
                &arrange(
                    &rows,
                    Some(&groups),
                    "pipeline",
                    sort,
                    false,
                    &RowFilter::default(),
                ),
            )
        };
        assert_eq!(
            flat(SortKey::Runs)[0],
            "anthropic/claude-opus-5-5|claude-code|xhigh"
        );
        // Rank: 1s first (by verdict between them), unranked last.
        assert_eq!(
            &flat(SortKey::Rank)[..3],
            &[
                "openai/gpt-6.1-sol|codex|max".to_string(),
                "openai/gpt-6-astra|codex|max".to_string(),
                "anthropic/claude-opus-5-5|claude-code|xhigh".to_string(),
            ]
        );
        assert_eq!(
            &flat(SortKey::PassRate)[..2],
            &[
                "openai/gpt-6.1-sol|codex|max".to_string(),
                "anthropic/claude-opus-5-5|claude-code|high".to_string(),
            ]
        );
        // Cost: cheapest known first; unknown never sorts as zero.
        assert_eq!(
            &flat(SortKey::Cost)[..2],
            &[
                "zai/glm-5.3|zcode|max".to_string(),
                "anthropic/claude-opus-5-5|claude-code|high".to_string(),
            ]
        );
        assert_eq!(
            flat(SortKey::Recency)[0],
            "anthropic/claude-opus-5-5|claude-code|xhigh"
        );
        assert_eq!(flat(SortKey::Verdict)[0], "openai/gpt-6.1-sol|codex|max");
    }

    #[test]
    fn filters_alone_and_combined() {
        let (rows, groups) = fixture();
        let only = |pick: RowFilter| -> Vec<String> {
            arrange(
                &rows,
                Some(&groups),
                "pipeline",
                SortKey::Verdict,
                false,
                &pick,
            )
            .iter()
            .map(|l| match l {
                Line::Row { index, .. } => rows[*index].key.clone(),
                _ => unreachable!(),
            })
            .collect()
        };
        assert_eq!(
            only(RowFilter {
                family: Some("zai".into()),
                ..RowFilter::default()
            }),
            vec!["zai/glm-5.3|zcode|max"]
        );
        assert_eq!(
            only(RowFilter {
                model: Some("openai/gpt-6.1-sol".into()),
                ..RowFilter::default()
            })
            .len(),
            3
        );
        assert_eq!(
            only(RowFilter {
                effort: Some("max".into()),
                ..RowFilter::default()
            })
            .len(),
            3
        );
        assert_eq!(
            only(RowFilter {
                effort: Some(String::new()),
                ..RowFilter::default()
            }),
            vec!["openai/gpt-6.1-sol|codex|", "acme/x|omp|"]
        );
        assert_eq!(
            only(RowFilter {
                family: Some("openai".into()),
                effort: Some("max".into()),
                ..RowFilter::default()
            }),
            vec![
                "openai/gpt-6.1-sol|codex|max",
                "openai/gpt-6-astra|codex|max"
            ]
        );
        assert!(
            only(RowFilter {
                family: Some("anthropic".into()),
                model: Some("zai/glm-5.3".into()),
                ..RowFilter::default()
            })
            .is_empty()
        );
        let grouped = arrange(
            &rows,
            Some(&groups),
            "pipeline",
            SortKey::Verdict,
            true,
            &RowFilter {
                effort: Some("max".into()),
                ..RowFilter::default()
            },
        );
        assert_eq!(
            shape(&rows, &grouped)[0],
            "# openai 677",
            "a family keeps only its kept rows and their runs"
        );
    }

    #[test]
    fn facets_follow_the_chosen_family_and_model() {
        let (rows, groups) = fixture();
        let all = facets(&rows, Some(&groups), "pipeline", &RowFilter::default());
        assert_eq!(
            all.families
                .iter()
                .map(|f| f.0.as_str())
                .collect::<Vec<_>>(),
            vec!["anthropic", "openai", "zai", "none"]
        );
        let openai = facets(
            &rows,
            Some(&groups),
            "pipeline",
            &RowFilter {
                family: Some("openai".into()),
                ..RowFilter::default()
            },
        );
        assert_eq!(
            openai.models,
            vec![
                ("openai/gpt-6.1-sol".to_string(), "Sol 6.1".to_string()),
                ("openai/gpt-6-astra".to_string(), "Astra".to_string())
            ]
        );
        let sol = facets(
            &rows,
            Some(&groups),
            "pipeline",
            &RowFilter {
                model: Some("openai/gpt-6.1-sol".into()),
                ..RowFilter::default()
            },
        );
        assert_eq!(
            sol.efforts.iter().map(|e| e.0.as_str()).collect::<Vec<_>>(),
            vec!["medium", "max", ""]
        );
        assert_eq!(sol.efforts[2].1, "Not recorded");
    }

    #[test]
    fn the_server_filters_become_a_query_string() {
        let day = |n: i64| format!("2026-11-{:02}", 13 - n);
        assert_eq!(ServerFilter::default().query(day), "");
        assert_eq!(ServerFilter::default().active(), 0);
        let week = ServerFilter {
            dates: "7".into(),
            ..ServerFilter::default()
        };
        assert_eq!(week.query(day), "from=2026-11-07");
        let custom = ServerFilter {
            dates: "custom".into(),
            from: "2026-11-10".into(),
            to: "2026-11-01".into(),
            source: "real".into(),
            task: "build".into(),
        };
        assert_eq!(
            custom.query(day),
            "from=2026-11-01&to=2026-11-10&source=real&taskType=build"
        );
        assert_eq!(custom.active(), 3);
        let half = ServerFilter {
            dates: "custom".into(),
            from: "2026-11-05".into(),
            to: "not a day".into(),
            ..ServerFilter::default()
        };
        assert_eq!(half.query(day), "from=2026-11-05");
        let odd = ServerFilter {
            task: "a b&c".into(),
            source: "everything".into(),
            ..ServerFilter::default()
        };
        assert_eq!(odd.query(day), "");
    }

    #[test]
    fn without_keys_the_server_order_stays_and_nothing_is_grouped() {
        let (rows, _) = fixture();
        let lines = arrange(
            &rows,
            None,
            "pipeline",
            SortKey::Runs,
            true,
            &RowFilter::default(),
        );
        assert_eq!(lines.len(), rows.len());
        assert_eq!(
            lines[0],
            Line::Row {
                index: 0,
                sub: false
            }
        );
        assert_eq!(SortKey::from_id("pass"), SortKey::PassRate);
        assert_eq!(SortKey::from_id("nonsense"), SortKey::Verdict);
        assert_eq!(pass_text(None), "–");
    }
}
