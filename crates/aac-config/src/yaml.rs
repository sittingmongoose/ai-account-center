//! Locate root entries from YAML events, never by deserializing retired values or expanding
//! aliases. Source bytes stay intact; only the temporary active input substitutes null values.
use crate::RETIRED_SECTIONS;
use aac_core::{Cause, Failure};
use std::collections::{BTreeMap, BTreeSet};
use yaml_rust2::parser::{Event, Parser};
use yaml_rust2::scanner::{Marker, Scanner, TScalarStyle, Token, TokenType};

pub(super) fn damaged() -> Failure {
    Failure::new(
        Cause::Damaged,
        "config.yaml: invalid or unsupported YAML document",
    )
}

struct Entry {
    key: String,
    start: usize,
    column: usize,
}
struct Root {
    entries: Vec<Entry>,
    end: usize,
    flow: bool,
}

// The scanner's Key marker includes an explicit '?' indicator, unlike the parser's Scalar
// marker. Pair each scalar token with its entry indicator, or its own marker for a bare flow
// key. The parser below decides which of those scalars are root keys. This one placement rule
// covers plain/quoted and implicit/explicit keys without inspecting arbitrary line prefixes.
fn scalar_sites(source: &str) -> Result<BTreeMap<usize, Marker>, Failure> {
    let mut scanner = Scanner::new(source.chars());
    let mut sites = BTreeMap::new();
    let mut indicator = None;
    for _ in 0..100_000 {
        let Token(mark, token) = scanner.next().ok_or_else(damaged)?;
        match token {
            TokenType::StreamEnd => {
                return if scanner.get_error().is_none() {
                    Ok(sites)
                } else {
                    Err(damaged())
                };
            }
            TokenType::Key => indicator = Some(mark),
            TokenType::Scalar(..) => {
                if sites
                    .insert(mark.index(), indicator.take().unwrap_or(mark))
                    .is_some()
                {
                    return Err(damaged());
                }
            }
            _ => indicator = None,
        }
    }
    Err(damaged())
}

// Parse events iteratively. Aliases remain single events, including recursive aliases; budgets
// apply even to opaque sections. Refuse multiple documents, complex/duplicate/merged root keys
// and root tags/anchors rather than reconstructing an ambiguous active document.
fn root_entries(source: &str) -> Result<Root, Failure> {
    let sites = scalar_sites(source)?;
    let mut parser = Parser::new_from_str(source);
    // The parser's character iterator advances its marker index once per Unicode scalar.
    // Convert those indices to UTF-8 byte boundaries before retaining any source spans.
    let offsets: Vec<_> = source
        .char_indices()
        .map(|(byte, _)| byte)
        .chain(std::iter::once(source.len()))
        .collect();
    let mut entries = Vec::new();
    let mut keys = BTreeSet::new();
    let mut depth = 0usize;
    let mut documents = 0usize;
    let mut root_seen = false;
    let mut expect_key = true;
    let mut root_end = None;
    let mut flow = false;
    for _ in 0..100_000 {
        let (event, mark) = parser.next_token().map_err(|_| damaged())?;
        let start = *offsets.get(mark.index()).ok_or_else(damaged)?;
        match event {
            Event::StreamStart | Event::DocumentEnd => {}
            Event::DocumentStart => {
                documents += 1;
                if documents > 1 {
                    return Err(damaged());
                }
            }
            Event::MappingStart(anchor, tag) if depth == 0 && !root_seen => {
                if anchor != 0 || tag.is_some() {
                    return Err(damaged());
                }
                root_seen = true;
                depth = 1;
                flow = source.as_bytes().get(start) == Some(&b'{');
            }
            Event::Scalar(key, style, anchor, tag) if depth == 1 && expect_key => {
                if anchor != 0
                    || tag.is_some()
                    || key.is_empty()
                    || key == "<<"
                    || !matches!(
                        style,
                        TScalarStyle::Plain
                            | TScalarStyle::SingleQuoted
                            | TScalarStyle::DoubleQuoted
                    )
                    || !keys.insert(key.clone())
                {
                    return Err(damaged());
                }
                let site = sites.get(&mark.index()).ok_or_else(damaged)?;
                let entry_start = *offsets.get(site.index()).ok_or_else(damaged)?;
                if entry_start > start {
                    return Err(damaged());
                }
                let column = site.col();
                if !flow
                    && entries
                        .first()
                        .is_some_and(|entry: &Entry| entry.column != column)
                {
                    return Err(damaged());
                }
                entries.push(Entry {
                    key,
                    start: entry_start,
                    column,
                });
                expect_key = false;
            }
            Event::MappingStart(..) | Event::SequenceStart(..) if depth > 0 => {
                if depth == 1 && expect_key {
                    return Err(damaged());
                }
                depth += 1;
                if depth > 128 {
                    return Err(damaged());
                }
            }
            Event::MappingEnd | Event::SequenceEnd if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    if !expect_key {
                        return Err(damaged());
                    }
                    root_end = Some(start);
                } else if depth == 1 {
                    expect_key = true;
                }
            }
            Event::Scalar(..) | Event::Alias(..) if depth > 0 => {
                if depth == 1 {
                    if expect_key {
                        return Err(damaged());
                    }
                    expect_key = true;
                }
            }
            Event::StreamEnd if root_seen && depth == 0 => {
                return Ok(Root {
                    entries,
                    end: root_end.ok_or_else(damaged)?,
                    flow,
                });
            }
            _ => return Err(damaged()),
        }
    }
    Err(damaged())
}

pub(super) fn separate_retired(
    original: &str,
) -> Result<(String, BTreeMap<String, String>), Failure> {
    // Match the TS loader's single leading-BOM normalization. Scanner, parser and active
    // deserialization all use this same view; Config retains the untouched original bytes.
    let source = original.strip_prefix('\u{feff}').unwrap_or(original);
    if source.trim().is_empty() {
        return Ok((source.into(), BTreeMap::new()));
    }
    let root = root_entries(source)?;
    let mut active = String::with_capacity(source.len());
    let mut retired = BTreeMap::new();
    let mut copied = 0;
    for (index, entry) in root.entries.iter().enumerate() {
        if !RETIRED_SECTIONS.contains(&entry.key.as_str()) {
            continue;
        }
        let next = root.entries.get(index + 1);
        let end = next.map_or(root.end, |entry| entry.start);
        active.push_str(source.get(copied..entry.start).ok_or_else(damaged)?);
        let original = source.get(entry.start..end).ok_or_else(damaged)?;
        retired.insert(entry.key.clone(), original.to_owned());
        let key = serde_json::to_string(&entry.key).map_err(|_| damaged())?;
        active.push_str(&key);
        active.push_str(": null");
        if root.flow {
            if next.is_some() {
                active.push_str(", ");
            }
        } else {
            active.push('\n');
            if let Some(next) = next {
                active.push_str(&" ".repeat(next.column));
            }
        }
        copied = end;
    }
    active.push_str(source.get(copied..).ok_or_else(damaged)?);
    Ok((active, retired))
}
