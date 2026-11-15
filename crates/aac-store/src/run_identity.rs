//! Run identity (Scoreboard v2 DESIGN.md sections 1 to 3): the model, its family, the effort
//! and the harness of a run, each as its own dimension with where it came from. Ported from
//! `src/web-server/scoreboard/model-identity.ts` (its tests are the behaviour spec) and
//! extended: effort spellings, more suffix forms, case-insensitive unique matches, harness
//! inference and correction, and vendor families. Unknown stays null; a name that resolves
//! whole is never split, so `qwen3.8-max` stays a model.

use serde::Serialize;
use serde_json::Value as Json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Effort levels in order. Anything after them is an unrecognized spelling, then unknown.
pub const EFFORT_ORDER: [&str; 8] = ["none", "minimal", "low", "medium", "high", "xhigh", "max", "ultrathink"];

/// Words that mean "no level recorded".
const EFFORT_UNSET: [&str; 8] = ["default", "auto", "unset", "unknown", "n/a", "na", "null", "inherit"];

/// One canonical effort level for a written spelling (case, `-`, `_` and spaces ignored).
pub fn effort_level(word: &str) -> Option<&'static str> {
    let key: String = word
        .trim()
        .chars()
        .filter(|c| !matches!(c, '-' | '_' | ' '))
        .flat_map(char::to_lowercase)
        .collect();
    Some(match key.as_str() {
        "none" | "off" | "noreasoning" => "none",
        "minimal" | "min" | "minimum" => "minimal",
        "low" | "lo" => "low",
        "medium" | "med" | "mid" | "moderate" => "medium",
        "high" | "hi" => "high",
        "xhigh" | "extrahigh" | "veryhigh" | "xh" => "xhigh",
        "max" | "maximum" | "xxhigh" => "max",
        "ultrathink" | "ultra" => "ultrathink",
        _ => return None,
    })
}

/// The order of an effort for sorting: levels first, then other spellings, then unknown.
pub fn effort_rank(effort: Option<&str>) -> u32 {
    match effort {
        None => 100,
        Some(e) => EFFORT_ORDER
            .iter()
            .position(|l| *l == e)
            .map(|p| p as u32)
            .unwrap_or(50),
    }
}

/// A shared catalog version: `models.json` and `model-aliases.json`, addressed by content.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    /// SHA-256 hex of `aac-run-catalog-v1`, NUL, models text, NUL, aliases text.
    pub hash: String,
    pub models: Vec<CatalogModel>,
    pub aliases: BTreeMap<String, String>,
    pub effort_suffixes: Vec<String>,
    pub models_text: Option<String>,
    pub aliases_text: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CatalogModel {
    pub model: String,
    pub label: String,
    pub family: Option<String>,
    pub runtimes: Vec<String>,
    pub efforts: Vec<String>,
}

pub fn catalog_hash(models: Option<&str>, aliases: Option<&str>) -> String {
    let mut h = Sha256::new();
    h.update(b"aac-run-catalog-v1\0");
    h.update(models.unwrap_or("").as_bytes());
    h.update(b"\0");
    h.update(aliases.unwrap_or("").as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn strings(v: Option<&Json>) -> Vec<String> {
    v.and_then(Json::as_array)
        .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// `models.json` (`catalog-models@1`) rows, or None when the text is not that file.
pub fn parse_models(text: &str) -> Option<Vec<CatalogModel>> {
    let doc: Json = serde_json::from_str(text).ok()?;
    if doc.get("schema").and_then(Json::as_str) != Some("catalog-models@1") {
        return None;
    }
    let rows = doc.get("models")?.as_array()?;
    let mut out = Vec::new();
    for row in rows {
        let (Some(model), Some(label)) = (
            row.get("model").and_then(Json::as_str),
            row.get("label").and_then(Json::as_str),
        ) else {
            return None;
        };
        out.push(CatalogModel {
            model: model.to_string(),
            label: label.to_string(),
            family: row
                .get("family")
                .and_then(Json::as_str)
                .filter(|f| !f.is_empty())
                .map(str::to_string),
            runtimes: strings(row.get("runtimes")),
            efforts: strings(row.get("efforts")),
        });
    }
    Some(out)
}

/// `model-aliases.json` (`agent-reports/model-aliases@1`), or None when the text is not that file.
pub fn parse_aliases(text: &str) -> Option<(BTreeMap<String, String>, Vec<String>)> {
    let doc: Json = serde_json::from_str(text).ok()?;
    if doc.get("schema").and_then(Json::as_str) != Some("agent-reports/model-aliases@1") {
        return None;
    }
    let mut aliases = BTreeMap::new();
    for (k, v) in doc.get("aliases")?.as_object()? {
        aliases.insert(k.clone(), v.as_str()?.to_string());
    }
    Some((aliases, strings(doc.get("effortSuffixes"))))
}

impl Catalog {
    /// A catalog from both files' text. A file that does not parse is None here; the caller
    /// keeps the last good version instead (a broken write never re-keys history).
    pub fn from_texts(models: Option<&str>, aliases: Option<&str>) -> Option<Catalog> {
        let parsed_models = match models {
            Some(t) => parse_models(t)?,
            None => Vec::new(),
        };
        let (parsed_aliases, suffixes) = match aliases {
            Some(t) => parse_aliases(t)?,
            None => (BTreeMap::new(), Vec::new()),
        };
        Some(Catalog {
            hash: catalog_hash(models, aliases),
            models: parsed_models,
            aliases: parsed_aliases,
            effort_suffixes: suffixes,
            models_text: models.map(str::to_string),
            aliases_text: aliases.map(str::to_string),
        })
    }

    pub fn model(&self, id: &str) -> Option<&CatalogModel> {
        self.models.iter().find(|m| m.model == id)
    }

    fn exact(&self, id: &str) -> Option<String> {
        self.model(id).map(|m| m.model.clone())
    }

    fn aliased(&self, id: &str) -> Option<String> {
        self.aliases.get(id).filter(|t| self.model(t).is_some()).cloned()
    }

    fn bare(&self, id: &str) -> Option<String> {
        if id.contains(['/', '\\', ':']) {
            return None;
        }
        let tail = format!("/{id}");
        let found: Vec<_> = self.models.iter().filter(|m| m.model.ends_with(&tail)).collect();
        (found.len() == 1).then(|| found[0].model.clone())
    }

    fn case_insensitive(&self, id: &str) -> Option<String> {
        let lower = id.to_lowercase();
        let mut found: Vec<String> = Vec::new();
        let mut add = |m: &str| {
            if !found.iter().any(|f| f == m) {
                found.push(m.to_string());
            }
        };
        for m in &self.models {
            if m.model.to_lowercase() == lower {
                add(&m.model);
            }
        }
        for (k, v) in &self.aliases {
            if k.to_lowercase() == lower && self.model(v).is_some() {
                add(v);
            }
        }
        if !lower.contains(['/', '\\', ':']) {
            let tail = format!("/{lower}");
            for m in &self.models {
                if m.model.to_lowercase().ends_with(&tail) {
                    add(&m.model);
                }
            }
        }
        (found.len() == 1).then(|| found.remove(0))
    }

    /// Rules 1 to 3 of model-identity.ts, then their unique case-insensitive form.
    fn whole(&self, id: &str) -> Option<(String, &'static str)> {
        if let Some(m) = self.exact(id) {
            return Some((m, "exact"));
        }
        if let Some(m) = self.aliased(id) {
            return Some((m, "alias"));
        }
        if let Some(m) = self.bare(id) {
            return Some((m, "bare"));
        }
        self.case_insensitive(id).map(|m| (m, "case"))
    }
}

/// The vendor families, in the page's order (DESIGN.md 1.4).
pub const FAMILIES: [(&str, &str, &str, &str); 9] = [
    ("anthropic", "anthropic", "Anthropic · Claude", "Claude"),
    ("openai", "openai", "OpenAI · GPT, Codex", "GPT"),
    ("google", "google", "Google · Gemini", "Gemini"),
    ("muse", "muse", "Muse", "Muse"),
    ("zai", "zai", "Z.ai · GLM", "GLM"),
    ("alibaba", "qwen", "Alibaba · Qwen", "Qwen"),
    ("deepseek", "deepseek", "DeepSeek", "DeepSeek"),
    ("xai", "xai", "xAI · Grok", "Grok"),
    ("moonshot", "moonshot", "Moonshot · Kimi", "Kimi"),
];

/// A family's id, label, short label and order.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Family {
    pub id: String,
    pub label: String,
    pub short: String,
    pub order: u32,
}

/// The family of a canonical model: its vendor prefix, else the catalog's own family word.
pub fn family_of(catalog: &Catalog, model_id: &str) -> Option<Family> {
    let vendor = model_id.split_once('/').map(|(v, _)| v).unwrap_or("");
    if let Some((i, (_, id, label, short))) = FAMILIES.iter().enumerate().find(|(_, f)| f.0 == vendor) {
        return Some(Family {
            id: id.to_string(),
            label: label.to_string(),
            short: short.to_string(),
            order: i as u32 + 1,
        });
    }
    let word = catalog.model(model_id).and_then(|m| m.family.clone())?;
    let slug: String = word
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    Some(Family {
        id: format!("catalog-{slug}"),
        short: word.clone(),
        label: word,
        order: 50,
    })
}

/// The family id of a model, for the stored row.
pub fn family_id(catalog: &Catalog, model_id: Option<&str>) -> Option<String> {
    model_id.and_then(|m| family_of(catalog, m)).map(|f| f.id)
}

/// Canonical harness words for written spellings.
fn harness_word(written: &str) -> Option<&'static str> {
    let key: String = written.trim().to_lowercase().replace(['_', ' '], "-");
    Some(match key.as_str() {
        "claude-code" | "claude" | "claudecode" => "claude-code",
        "codex" | "codex-cli" => "codex",
        "cursor" | "cursor-agent" => "cursor",
        "muse" => "muse",
        "omp" => "omp",
        "zcode" => "zcode",
        "antigravity" | "agy" => "antigravity",
        "opencode" => "opencode",
        "grok" | "grok-cli" => "grok",
        "gemini-cli" | "gemini" => "gemini-cli",
        "kimi" | "kimi-cli" => "kimi",
        _ => return None,
    })
}

/// Which vendor families a harness can run; None means any.
fn harness_accepts(harness: &str) -> Option<&'static [&'static str]> {
    match harness {
        "codex" => Some(&["openai"]),
        "claude-code" => Some(&["anthropic", "zai", "alibaba", "deepseek", "moonshot", "minimax"]),
        "muse" => Some(&["muse"]),
        "zcode" => Some(&["zai"]),
        "antigravity" => Some(&["google", "anthropic", "openai"]),
        "grok" => Some(&["xai"]),
        "gemini-cli" => Some(&["google"]),
        "kimi" => Some(&["moonshot"]),
        _ => None,
    }
}

/// The one harness a vendor's models run in by default, when there is exactly one.
fn vendor_harness(vendor: &str) -> Option<&'static str> {
    match vendor {
        "anthropic" => Some("claude-code"),
        "openai" => Some("codex"),
        "muse" => Some("muse"),
        "zai" => Some("zcode"),
        "google" => Some("antigravity"),
        _ => None,
    }
}

/// What a run says about its model, as written.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Written {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub harness: Option<String>,
}

/// One run's identity under one catalog version (DATA-ARCHITECTURE section 2 names).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub model_raw: Option<String>,
    /// The canonical catalog id; None when the model is unmapped or missing.
    pub model_id: Option<String>,
    /// exact, alias, bare, case, suffix, unmapped or none.
    pub model_source: String,
    pub model_family: Option<String>,
    pub effort_raw: Option<String>,
    pub effort: Option<String>,
    /// reported, suffix, unset, unrecognized or none.
    pub effort_source: String,
    pub effort_conflict: bool,
    pub harness_raw: Option<String>,
    pub harness: Option<String>,
    /// reported, inferred, corrected, unrecognized or unknown.
    pub harness_source: String,
    pub model_catalog_hash: String,
}

impl Identity {
    /// The model as the view shows it: canonical, else as written.
    pub fn model_shown(&self) -> Option<&str> {
        self.model_id.as_deref().or(self.model_raw.as_deref())
    }
}

/// A trailing effort word whose base resolves whole: `-high`, `_xhigh`, ` max`, `@low`,
/// `:medium`, `(max)`, `[high]`. Exactly one candidate, else none.
fn suffix_split(catalog: &Catalog, id: &str) -> Option<(String, &'static str)> {
    let mut found: Vec<(String, &'static str)> = Vec::new();
    let mut add = |m: String, e: &'static str| {
        if !found.iter().any(|(fm, fe)| *fm == m && *fe == e) {
            found.push((m, e));
        }
    };
    let trimmed = id.trim_end();
    if let Some(open) = trimmed.rfind(['(', '['])
        && (trimmed.ends_with(')') || trimmed.ends_with(']'))
        && let Some(level) = effort_level(&trimmed[open + 1..trimmed.len() - 1])
        && let Some((m, _)) = catalog.whole(trimmed[..open].trim_end())
    {
        add(m, level);
    }
    for (i, c) in trimmed.char_indices().rev() {
        if !matches!(c, '-' | '_' | ' ' | '@' | ':') || i == 0 {
            continue;
        }
        let word = &trimmed[i + 1..];
        if word.is_empty()
            || !word
                .chars()
                .all(|c| c.is_ascii_alphabetic() || matches!(c, '-' | '_' | ' '))
        {
            continue;
        }
        if let Some(level) = effort_level(word)
            && let Some((m, _)) = catalog.whole(trimmed[..i].trim_end())
        {
            add(m, level);
        }
    }
    // The table's own effort suffixes, exactly as model-identity.ts reads them.
    for suffix in catalog.effort_suffixes.iter().cloned().chain(
        catalog
            .models
            .iter()
            .flat_map(|m| m.efforts.iter().map(|e| format!("-{e}"))),
    ) {
        if trimmed.len() > suffix.len()
            && trimmed.ends_with(&suffix)
            && let Some(level) = effort_level(&suffix[1..])
            && let Some((m, _)) = catalog.whole(&trimmed[..trimmed.len() - suffix.len()])
        {
            add(m, level);
        }
    }
    (found.len() == 1).then(|| found.remove(0))
}

/// A run's identity under `catalog` (DESIGN.md section 3).
pub fn identify(catalog: &Catalog, written: &Written) -> Identity {
    let model_raw = written
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let mut out = Identity {
        model_raw: model_raw.clone(),
        model_source: "none".into(),
        effort_raw: written.effort.clone(),
        harness_raw: written.harness.clone(),
        model_catalog_hash: catalog.hash.clone(),
        ..Identity::default()
    };
    let mut suffix_effort: Option<&'static str> = None;
    if let Some(raw) = &model_raw {
        if let Some((m, how)) = catalog.whole(raw) {
            out.model_id = Some(m);
            out.model_source = how.into();
        } else if let Some((m, level)) = suffix_split(catalog, raw) {
            out.model_id = Some(m);
            out.model_source = "suffix".into();
            suffix_effort = Some(level);
        } else {
            out.model_source = "unmapped".into();
        }
    }
    out.model_family = family_id(catalog, out.model_id.as_deref());

    // Effort: the run's own field wins; the id's suffix fills it only when the field is empty.
    let written_effort = written.effort.as_deref().map(str::trim).filter(|s| !s.is_empty());
    match written_effort {
        Some(word) if EFFORT_UNSET.contains(&word.to_lowercase().as_str()) => {
            out.effort = suffix_effort.map(str::to_string);
            out.effort_source = if suffix_effort.is_some() { "suffix" } else { "unset" }.into();
        }
        Some(word) => match effort_level(word) {
            Some(level) => {
                out.effort = Some(level.into());
                out.effort_source = "reported".into();
                out.effort_conflict = suffix_effort.is_some_and(|s| s != level);
            }
            None => {
                out.effort = Some(word.to_string());
                out.effort_source = "unrecognized".into();
            }
        },
        None => {
            out.effort = suffix_effort.map(str::to_string);
            out.effort_source = if suffix_effort.is_some() { "suffix" } else { "none" }.into();
        }
    }

    // Harness: as written when it can run the model's family; a launch path or nothing is
    // inferred from the family; a harness that cannot run the family is corrected.
    let vendor = out
        .model_id
        .as_deref()
        .and_then(|m| m.split_once('/'))
        .map(|(v, _)| v.to_string());
    let default = vendor.as_deref().and_then(vendor_harness);
    let written_harness = written.harness.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let inferred = |out: &mut Identity| match default {
        Some(h) => {
            out.harness = Some(h.into());
            out.harness_source = "inferred".into();
        }
        None => {
            out.harness = None;
            out.harness_source = "unknown".into();
        }
    };
    match written_harness {
        None => inferred(&mut out),
        Some(h) if h.eq_ignore_ascii_case("t3-delegate") || h.eq_ignore_ascii_case("t3") => inferred(&mut out),
        Some(h) => match harness_word(h) {
            None => {
                out.harness = Some(h.to_string());
                out.harness_source = "unrecognized".into();
            }
            Some(word) => {
                let fits = match (harness_accepts(word), vendor.as_deref()) {
                    (Some(accepts), Some(v)) => accepts.contains(&v),
                    _ => true,
                };
                if fits {
                    out.harness = Some(word.into());
                    out.harness_source = "reported".into();
                } else {
                    out.harness = default.map(str::to_string);
                    out.harness_source = if default.is_some() { "corrected" } else { "unknown" }.into();
                }
            }
        },
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A catalog shaped like the share's, with synthetic content only.
    fn catalog() -> Catalog {
        let models = serde_json::json!({ "schema": "catalog-models@1", "models": [
            { "model": "anthropic/claude-opus-5-5", "label": "Opus 5.5", "family": "Claude", "runtimes": ["claude-code"], "efforts": ["high", "xhigh", "max"] },
            { "model": "openai/gpt-6.1-sol", "label": "Sol 6.1", "family": "GPT-6", "runtimes": ["codex"], "efforts": ["medium", "high", "max"] },
            { "model": "openai/gpt-6-astra", "label": "Astra", "family": "GPT-6", "runtimes": ["codex"], "efforts": ["max"] },
            { "model": "google/gemini-4-argon", "label": "Gemini 4 Argon", "family": "Gemini", "runtimes": ["antigravity"], "efforts": ["high"] },
            { "model": "alibaba/qwen3.8-max", "label": "Qwen 3.8 Max", "family": "Qwen", "runtimes": [], "efforts": [] },
            { "model": "alibaba/qwen4-max-preview", "label": "Qwen 4 Max Preview", "family": "Qwen", "runtimes": ["omp"], "efforts": [] },
            { "model": "deepseek/deepseek-v4.1-flash", "label": "DeepSeek V4.1 Flash", "family": "DeepSeek", "runtimes": ["opencode-go"], "efforts": [] },
            { "model": "zai/glm-5.3", "label": "GLM 5.3", "family": "GLM", "runtimes": ["zcode"], "efforts": ["max"] },
            { "model": "muse/muse-ember-1.0", "label": "Muse Ember 1.0", "family": "Muse", "runtimes": ["muse"], "efforts": ["max"] },
            { "model": "xai/grok-4.7", "label": "Grok 4.7", "family": "Grok", "runtimes": ["cursor", "grok"], "efforts": [] },
            { "model": "acme/widget-2", "label": "Widget 2", "family": "Widget", "runtimes": [], "efforts": [] }
        ]})
        .to_string();
        let aliases = serde_json::json!({ "schema": "agent-reports/model-aliases@1",
            "effortSuffixes": ["-minimal", "-low", "-medium", "-high", "-xhigh", "-max"],
            "aliases": {
                "gpt-6.1-sol": "openai/gpt-6.1-sol", "codex/gpt-6.1-sol": "openai/gpt-6.1-sol",
                "claude-opus-5-5": "anthropic/claude-opus-5-5", "qwen3.8-max": "alibaba/qwen3.8-max",
                "alibaba-token-plan/qwen3.8-max": "alibaba/qwen3.8-max",
                "builtin:zai-coding-plan\\GLM-5.3": "zai/glm-5.3", "glm-5.3": "zai/glm-5.3",
                "opencode-go/deepseek-v4.1-flash": "deepseek/deepseek-v4.1-flash",
                "dangling": "nowhere/missing-1"
            }
        })
        .to_string();
        Catalog::from_texts(Some(&models), Some(&aliases)).unwrap()
    }

    fn id(model: &str, effort: Option<&str>, harness: Option<&str>) -> Identity {
        identify(
            &catalog(),
            &Written {
                model: Some(model.into()),
                effort: effort.map(Into::into),
                harness: harness.map(Into::into),
            },
        )
    }

    #[test]
    fn every_effort_spelling_reads_as_one_level() {
        for (spelling, level) in [
            ("none", "none"),
            ("off", "none"),
            ("minimal", "minimal"),
            ("MIN", "minimal"),
            ("minimum", "minimal"),
            ("low", "low"),
            ("Lo", "low"),
            ("medium", "medium"),
            ("med", "medium"),
            ("mid", "medium"),
            ("Moderate", "medium"),
            ("high", "high"),
            ("HI", "high"),
            (" High ", "high"),
            ("xhigh", "xhigh"),
            ("x-high", "xhigh"),
            ("x_high", "xhigh"),
            ("X High", "xhigh"),
            ("extra-high", "xhigh"),
            ("extra_high", "xhigh"),
            ("extrahigh", "xhigh"),
            ("very-high", "xhigh"),
            ("XH", "xhigh"),
            ("max", "max"),
            ("MAX", "max"),
            ("maximum", "max"),
            ("xx-high", "max"),
            ("ultrathink", "ultrathink"),
            ("ultra-think", "ultrathink"),
            ("Ultra", "ultrathink"),
        ] {
            assert_eq!(effort_level(spelling), Some(level), "{spelling}");
            let identity = id("gpt-6.1-sol", Some(spelling), None);
            assert_eq!(identity.effort.as_deref(), Some(level), "{spelling}");
            assert_eq!(identity.effort_source, "reported");
        }
        for unset in ["default", "Auto", "unset", "unknown", "n/a"] {
            let identity = id("gpt-6.1-sol", Some(unset), None);
            assert_eq!(identity.effort, None, "{unset}");
            assert_eq!(identity.effort_source, "unset");
        }
        let odd = id("gpt-6.1-sol", Some("turbo"), None);
        assert_eq!(
            (odd.effort.as_deref(), odd.effort_source.as_str()),
            (Some("turbo"), "unrecognized")
        );
        assert!(effort_rank(Some("xhigh")) < effort_rank(Some("max")));
        assert!(effort_rank(Some("max")) < effort_rank(Some("ultrathink")));
        assert!(effort_rank(Some("ultrathink")) < effort_rank(Some("turbo")));
        assert!(effort_rank(Some("turbo")) < effort_rank(None));
    }

    #[test]
    fn names_that_end_in_an_effort_word_are_never_split() {
        let qwen = id("qwen3.8-max", None, None);
        assert_eq!(
            (qwen.model_id.as_deref(), qwen.effort.as_deref()),
            (Some("alibaba/qwen3.8-max"), None)
        );
        let plan = id("alibaba-token-plan/qwen3.8-max", None, Some("omp"));
        assert_eq!(plan.model_id.as_deref(), Some("alibaba/qwen3.8-max"));
        assert_eq!(plan.effort, None);
        let bare = id("alibaba/qwen3.8-max", None, None);
        assert_eq!(bare.model_id.as_deref(), Some("alibaba/qwen3.8-max"));
        let preview = id("alibaba/qwen4-max-preview", None, None);
        assert_eq!(preview.model_id.as_deref(), Some("alibaba/qwen4-max-preview"));
        // An unknown name that ends in -max stays unmapped, kept as written, family null.
        let unknown = id("foo-max", None, None);
        assert_eq!(
            (unknown.model_id.as_deref(), unknown.model_source.as_str()),
            (None, "unmapped")
        );
        assert_eq!(
            (unknown.effort.as_deref(), unknown.model_family.as_deref()),
            (None, None)
        );
        assert_eq!(unknown.model_shown(), Some("foo-max"));
        // A listed -max name with an effort suffix after it splits at the outer word only.
        let both = id("qwen3.8-max-high", None, None);
        assert_eq!(
            (both.model_id.as_deref(), both.effort.as_deref()),
            (Some("alibaba/qwen3.8-max"), Some("high"))
        );
    }

    #[test]
    fn suffix_forms_move_the_word_to_effort() {
        for (written, model, effort) in [
            ("google/gemini-4-argon-high", "google/gemini-4-argon", "high"),
            ("gpt-6.1-sol-max", "openai/gpt-6.1-sol", "max"),
            ("gpt-6.1-sol_xhigh", "openai/gpt-6.1-sol", "xhigh"),
            ("gpt-6.1-sol-x-high", "openai/gpt-6.1-sol", "xhigh"),
            ("gpt-6.1-sol (max)", "openai/gpt-6.1-sol", "max"),
            ("gpt-6.1-sol[medium]", "openai/gpt-6.1-sol", "medium"),
            ("gpt-6.1-sol@low", "openai/gpt-6.1-sol", "low"),
            ("gpt-6.1-sol:minimal", "openai/gpt-6.1-sol", "minimal"),
            ("claude-opus-5-5 ultrathink", "anthropic/claude-opus-5-5", "ultrathink"),
        ] {
            let identity = id(written, None, None);
            assert_eq!(identity.model_id.as_deref(), Some(model), "{written}");
            assert_eq!(identity.effort.as_deref(), Some(effort), "{written}");
            assert_eq!(
                (identity.model_source.as_str(), identity.effort_source.as_str()),
                ("suffix", "suffix")
            );
        }
    }

    #[test]
    fn the_runs_own_effort_wins_and_a_conflict_is_recorded() {
        let identity = id("gpt-6.1-sol-high", Some("max"), None);
        assert_eq!(identity.effort.as_deref(), Some("max"));
        assert!(identity.effort_conflict);
        let same = id("gpt-6.1-sol-high", Some("HIGH"), None);
        assert!(!same.effort_conflict);
    }

    #[test]
    fn resolution_order_and_case() {
        assert_eq!(id("openai/gpt-6.1-sol", None, None).model_source, "exact");
        assert_eq!(id("codex/gpt-6.1-sol", None, None).model_source, "alias");
        assert_eq!(id("gpt-6-astra", None, None).model_source, "bare");
        let upper = id("GPT-6.1-Sol", None, None);
        assert_eq!(
            (upper.model_id.as_deref(), upper.model_source.as_str()),
            (Some("openai/gpt-6.1-sol"), "case")
        );
        assert_eq!(
            id("builtin:zai-coding-plan\\GLM-5.3", None, None).model_id.as_deref(),
            Some("zai/glm-5.3")
        );
        // A prefix the table does not list is not stripped; a dangling alias resolves nothing.
        assert_eq!(id("party/gpt-6.1-sol", None, None).model_source, "unmapped");
        assert_eq!(id("dangling", None, None).model_source, "unmapped");
        assert_eq!(identify(&catalog(), &Written::default()).model_source, "none");
    }

    #[test]
    fn harness_is_inferred_corrected_or_kept() {
        let delegate = id("claude-opus-5-5", None, Some("t3-delegate"));
        assert_eq!(
            (delegate.harness.as_deref(), delegate.harness_source.as_str()),
            (Some("claude-code"), "inferred")
        );
        let missing = id("gpt-6.1-sol", None, None);
        assert_eq!(
            (missing.harness.as_deref(), missing.harness_source.as_str()),
            (Some("codex"), "inferred")
        );
        let wrong = id("gpt-6.1-sol", None, Some("claude-code"));
        assert_eq!(
            (wrong.harness.as_deref(), wrong.harness_source.as_str()),
            (Some("codex"), "corrected")
        );
        let proxy = id("qwen4-max-preview", None, Some("claude-code"));
        assert_eq!(
            (proxy.harness.as_deref(), proxy.harness_source.as_str()),
            (Some("claude-code"), "reported")
        );
        let omp = id("opencode-go/deepseek-v4.1-flash", None, Some("omp"));
        assert_eq!(omp.harness_source, "reported");
        let spelled = id("claude-opus-5-5", None, Some("Claude Code"));
        assert_eq!(
            (spelled.harness.as_deref(), spelled.harness_source.as_str()),
            (Some("claude-code"), "reported")
        );
        let grok = id("xai/grok-4.7", None, None);
        assert_eq!((grok.harness, grok.harness_source.as_str()), (None, "unknown"));
        let none = identify(
            &catalog(),
            &Written {
                harness: Some("codex".into()),
                ..Written::default()
            },
        );
        assert_eq!(
            (none.harness.as_deref(), none.harness_source.as_str()),
            (Some("codex"), "reported")
        );
    }

    #[test]
    fn families_follow_the_vendor_then_the_catalog_word() {
        let c = catalog();
        assert_eq!(family_of(&c, "openai/gpt-6.1-sol").unwrap().id, "openai");
        assert_eq!(family_of(&c, "alibaba/qwen3.8-max").unwrap().id, "qwen");
        assert_eq!(family_of(&c, "acme/widget-2").unwrap().id, "catalog-widget");
        assert_eq!(family_of(&c, "nowhere/x"), None);
        assert!(
            family_of(&c, "anthropic/claude-opus-5-5").unwrap().order
                < family_of(&c, "openai/gpt-6.1-sol").unwrap().order
        );
    }

    /// DESIGN.md 1.1: every model string written in the share's runs (2026-11-13) under the
    /// packaged catalog, which equals the share's. Model names only, no run content.
    #[test]
    fn every_written_model_string_maps_as_the_design_table_says() {
        let packaged = Catalog::from_texts(
            Some(include_str!("../../../docs/agent-reports/catalog/models.json")),
            Some(include_str!("../../../docs/agent-reports/catalog/model-aliases.json")),
        )
        .unwrap();
        let table: [(&str, Option<&str>, Option<&str>, &str, Option<&str>); 35] = [
            (
                "openai/gpt-6.1-sol",
                Some("openai/gpt-6.1-sol"),
                None,
                "exact",
                Some("openai"),
            ),
            ("gpt-6.1-sol", Some("openai/gpt-6.1-sol"), None, "alias", Some("openai")),
            (
                "codex/gpt-6.1-sol",
                Some("openai/gpt-6.1-sol"),
                None,
                "alias",
                Some("openai"),
            ),
            (
                "openai/gpt-6-astra",
                Some("openai/gpt-6-astra"),
                None,
                "exact",
                Some("openai"),
            ),
            ("gpt-6-astra", Some("openai/gpt-6-astra"), None, "alias", Some("openai")),
            (
                "codex/gpt-6-astra",
                Some("openai/gpt-6-astra"),
                None,
                "alias",
                Some("openai"),
            ),
            (
                "openai/gpt-6-luna",
                Some("openai/gpt-6-luna"),
                None,
                "exact",
                Some("openai"),
            ),
            (
                "anthropic/claude-opus-5-5",
                Some("anthropic/claude-opus-5-5"),
                None,
                "exact",
                Some("anthropic"),
            ),
            (
                "claude-opus-5-5",
                Some("anthropic/claude-opus-5-5"),
                None,
                "alias",
                Some("anthropic"),
            ),
            (
                "anthropic/claude-fable-5-2",
                Some("anthropic/claude-fable-5-2"),
                None,
                "exact",
                Some("anthropic"),
            ),
            (
                "claude-fable-5-2",
                Some("anthropic/claude-fable-5-2"),
                None,
                "alias",
                Some("anthropic"),
            ),
            (
                "anthropic/claude-fable-5-1",
                Some("anthropic/claude-fable-5-1"),
                None,
                "exact",
                Some("anthropic"),
            ),
            (
                "anthropic/claude-sonnet-5-5",
                Some("anthropic/claude-sonnet-5-5"),
                None,
                "exact",
                Some("anthropic"),
            ),
            (
                "anthropic/claude-haiku-5-5",
                Some("anthropic/claude-haiku-5-5"),
                None,
                "exact",
                Some("anthropic"),
            ),
            (
                "google/gemini-4-argon-high",
                Some("google/gemini-4-argon"),
                Some("high"),
                "suffix",
                Some("google"),
            ),
            (
                "google/gemini-4-argon",
                Some("google/gemini-4-argon"),
                None,
                "exact",
                Some("google"),
            ),
            (
                "muse/muse-wonder-full-1.0",
                Some("muse/muse-wonder-full-1.0"),
                None,
                "exact",
                Some("muse"),
            ),
            (
                "muse-wonder-full-1.0",
                Some("muse/muse-wonder-full-1.0"),
                None,
                "alias",
                Some("muse"),
            ),
            (
                "muse/muse-ember-1.0",
                Some("muse/muse-ember-1.0"),
                None,
                "exact",
                Some("muse"),
            ),
            (
                "muse-ember-1.0",
                Some("muse/muse-ember-1.0"),
                None,
                "alias",
                Some("muse"),
            ),
            (
                "muse-spark-1.3",
                Some("muse/muse-spark-1.3-contributor"),
                None,
                "alias",
                Some("muse"),
            ),
            ("zai/glm-5.3", Some("zai/glm-5.3"), None, "exact", Some("zai")),
            ("glm-5.3", Some("zai/glm-5.3"), None, "alias", Some("zai")),
            (
                "builtin:zai-coding-plan\\GLM-5.3",
                Some("zai/glm-5.3"),
                None,
                "alias",
                Some("zai"),
            ),
            ("zcode/glm-5.3", Some("zai/glm-5.3"), None, "alias", Some("zai")),
            (
                "builtin:bigmodel-coding-plan\\GLM-5.3",
                Some("zai/glm-5.3"),
                None,
                "alias",
                Some("zai"),
            ),
            (
                "alibaba/qwen4-max-preview",
                Some("alibaba/qwen4-max-preview"),
                None,
                "exact",
                Some("qwen"),
            ),
            (
                "alibaba-token-plan/qwen3.8-max",
                Some("alibaba/qwen3.8-max"),
                None,
                "alias",
                Some("qwen"),
            ),
            (
                "opencode-go/deepseek-v4.1-flash",
                Some("deepseek/deepseek-v4.1-flash"),
                None,
                "alias",
                Some("deepseek"),
            ),
            (
                "deepseek-v4.1-flash",
                Some("deepseek/deepseek-v4.1-flash"),
                None,
                "alias",
                Some("deepseek"),
            ),
            ("xai/grok-4.7", Some("xai/grok-4.7"), None, "exact", Some("xai")),
            ("grok-4.7", Some("xai/grok-4.7"), None, "alias", Some("xai")),
            ("cursor/grok-4.7", Some("xai/grok-4.7"), None, "alias", Some("xai")),
            ("gemini-4-hydrogen", None, None, "unmapped", None),
            ("qwen3.8-max", Some("alibaba/qwen3.8-max"), None, "alias", Some("qwen")),
        ];
        for (written, model, effort, source, family) in table {
            let identity = identify(
                &packaged,
                &Written {
                    model: Some(written.into()),
                    ..Written::default()
                },
            );
            assert_eq!(identity.model_id.as_deref(), model, "{written}");
            assert_eq!(identity.effort.as_deref(), effort, "{written}");
            assert_eq!(identity.model_source, source, "{written}");
            assert_eq!(identity.model_family.as_deref(), family, "{written}");
        }
    }

    #[test]
    fn a_catalog_that_does_not_parse_is_none_and_the_hash_is_content_addressed() {
        assert!(Catalog::from_texts(Some("{"), None).is_none());
        assert!(Catalog::from_texts(Some("{\"schema\":\"other\"}"), None).is_none());
        let a = catalog_hash(Some("m"), Some("a"));
        assert_ne!(a, catalog_hash(Some("ma"), Some("")));
        assert_eq!(a.len(), 64);
    }
}
