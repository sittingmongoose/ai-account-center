//! The Scoreboard's display model resolution (`model-identity.ts`), ported rule for rule:
//! every written spelling maps to a canonical catalog id by exactly one rule, in order —
//! the id itself, an explicit alias whose target is listed, the one catalog id ending
//! `/<id>` when the writing names no provider, then one effort suffix whose base resolves
//! by those rules (the run's own effort wins). Nothing else resolves. The ingest identity
//! (aac-store `run_identity::identify`) is a richer fact; this is the view's own lookup.

use serde_json::{Map, Value as Json};
use std::collections::HashSet;

/// One catalog model as the lookup reads it: its id and its listed efforts.
pub struct Model<'a> {
    pub model: &'a str,
    pub efforts: &'a [String],
}

/// The shared alias table: written ids to canonical ids, plus the effort suffixes.
pub struct AliasTable<'a> {
    pub aliases: &'a Map<String, Json>,
    pub effort_suffixes: &'a [String],
}

/// The resolved id and effort (`normalizeModel`'s answer).
#[derive(Debug, PartialEq)]
pub struct Resolved {
    pub id: String,
    pub effort: Option<String>,
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Resolve one written model id under the catalog and (optional) alias table.
pub fn normalize_model(
    id: &str,
    effort: Option<&str>,
    models: &[Model<'_>],
    table: Option<&AliasTable<'_>>,
) -> Resolved {
    let known: HashSet<&str> = models.iter().map(|model| model.model).collect();
    let aliased = |written: &str| -> Option<String> {
        let target = table?.aliases.get(written)?.as_str()?;
        known.contains(target).then(|| target.to_string())
    };
    let exact = |written: &str| -> Option<String> {
        models
            .iter()
            .find(|model| model.model == written)
            .map(|model| model.model.to_string())
    };
    let bare = |written: &str| -> Option<String> {
        if written.contains('/') || written.contains('\\') || written.contains(':') {
            return None;
        }
        let suffix = format!("/{written}");
        let mut found = models.iter().filter(|model| model.model.ends_with(&suffix));
        let first = found.next()?;
        found.next().is_none().then(|| first.model.to_string())
    };
    let base_of =
        |written: &str| -> Option<String> { exact(written).or_else(|| aliased(written)).or_else(|| bare(written)) };
    if let Some(direct) = base_of(id) {
        return Resolved {
            id: direct,
            effort: effort.map(str::to_string),
        };
    }
    // The suffix set: the table's first, then each model's catalog efforts, in order, deduped.
    let mut suffixes: Vec<String> = Vec::new();
    let mut push = |suffix: String| {
        if !suffixes.contains(&suffix) {
            suffixes.push(suffix);
        }
    };
    if let Some(table) = table {
        for suffix in table.effort_suffixes {
            push(suffix.clone());
        }
    }
    for model in models {
        for effort in model.efforts {
            push(format!("-{effort}"));
        }
    }
    // Longest first; equal lengths keep their order (the candidate count decides, not order).
    suffixes.sort_by_key(|suffix| std::cmp::Reverse(utf16_len(suffix)));
    let mut candidates: Vec<Resolved> = Vec::new();
    for suffix in &suffixes {
        if !id.ends_with(suffix.as_str()) || utf16_len(id) <= utf16_len(suffix) {
            continue;
        }
        let base = &id[..id.len() - suffix.len()];
        if let Some(base) = base_of(base) {
            candidates.push(Resolved {
                id: base,
                effort: Some(effort.map(str::to_string).unwrap_or_else(|| suffix[1..].to_string())),
            });
        }
    }
    if candidates.len() == 1 {
        return candidates.remove(0);
    }
    Resolved {
        id: id.to_string(),
        effort: effort.map(str::to_string),
    }
}
