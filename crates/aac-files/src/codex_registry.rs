//! The Codex profile registry (`codex-profiles.yaml`) read, validated, mutated and re-emitted
//! byte-for-byte as `js-yaml` 4 writes it (`yaml.dump(data, {indent: 2, lineWidth: -1})`). Ports
//! `codex-profile-registry.ts` so the sign-in install callbacks register a profile in Rust while
//! the caller holds the registry directory lease. A registry this module cannot parse, validate
//! and round-trip exactly is declined (`None`); the TypeScript owner answers it.
use std::path::Path;

/// The schema version a registry without a string version carries (`CODEX_PROFILE_SCHEMA_VERSION`).
pub const SCHEMA_VERSION: &str = "1.0";

/// One metadata field value: a string or null. The registry's known fields are only these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Field {
    /// A string scalar.
    Str(String),
    /// A null scalar.
    Null,
}

/// A profile's metadata fields in their original insertion order.
pub type Meta = Vec<(String, Field)>;

/// Refusal to validate or mutate a profile registry entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileError {
    /// The name violates the registry's profile name rules.
    InvalidName,
    /// A profile with that name already exists.
    AlreadyExists,
    /// No profile has that name.
    Missing,
    /// Removing the default while other profiles exist requires an explicit override.
    DefaultInUse,
}

/// A validated registry: version, default pointer and profiles in insertion order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registry {
    /// The schema version string.
    pub version: String,
    /// The default profile name, or null.
    pub default: Option<String>,
    /// Profiles in insertion order.
    pub profiles: Vec<(String, Meta)>,
}

impl Registry {
    /// An empty registry (`emptyRegistry`).
    pub fn empty() -> Self {
        Self {
            version: SCHEMA_VERSION.to_owned(),
            default: None,
            profiles: Vec::new(),
        }
    }

    /// Whether a profile name is present (`hasProfile`), after the name rule.
    pub fn has(&self, name: &str) -> bool {
        profile_name_error(name).is_ok() && self.profiles.iter().any(|(n, _)| n == name)
    }

    /// The profile names in insertion order (`listProfiles`).
    pub fn list(&self) -> Vec<&str> {
        self.profiles.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// A profile's metadata (`getProfile`), or `None` when absent.
    pub fn get(&self, name: &str) -> Option<&Meta> {
        self.profiles
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, m)| m)
    }

    fn get_mut(&mut self, name: &str) -> Option<&mut Meta> {
        self.profiles
            .iter_mut()
            .find(|(n, _)| n == name)
            .map(|(_, m)| m)
    }

    /// Insert a new profile (`createProfile`); `Err` when the name is invalid or already present.
    /// `meta` is spread over the base `{type, created, last_used}` in TypeScript's order.
    pub fn create(&mut self, name: &str, created: &str, meta: Meta) -> Result<(), ProfileError> {
        profile_name_error(name)?;
        if self.profiles.iter().any(|(n, _)| n == name) {
            return Err(ProfileError::AlreadyExists);
        }
        let mut fields: Meta = vec![
            ("type".to_owned(), Field::Str("codex".to_owned())),
            ("created".to_owned(), Field::Str(created.to_owned())),
            ("last_used".to_owned(), Field::Null),
        ];
        for (key, value) in meta {
            if let Some(slot) = fields.iter_mut().find(|(k, _)| *k == key) {
                slot.1 = value;
            } else {
                fields.push((key, value));
            }
        }
        self.profiles.push((name.to_owned(), fields));
        Ok(())
    }

    /// Merge a partial update into a profile (`updateProfile`); `Err` when absent or invalid.
    pub fn update(&mut self, name: &str, partial: Meta) -> Result<(), ProfileError> {
        profile_name_error(name)?;
        let fields = self.get_mut(name).ok_or(ProfileError::Missing)?;
        for (key, value) in partial {
            if let Some(slot) = fields.iter_mut().find(|(k, _)| *k == key) {
                slot.1 = value;
            } else {
                fields.push((key, value));
            }
        }
        Ok(())
    }

    /// Remove a profile (`removeProfile`); `Err` on the default-with-others guard or absence.
    pub fn remove(&mut self, name: &str, force_default: bool) -> Result<(), ProfileError> {
        profile_name_error(name)?;
        let index = self
            .profiles
            .iter()
            .position(|(n, _)| n == name)
            .ok_or(ProfileError::Missing)?;
        if self.default.as_deref() == Some(name) && self.profiles.len() > 1 && !force_default {
            return Err(ProfileError::DefaultInUse);
        }
        self.profiles.remove(index);
        if self.default.as_deref() == Some(name) {
            self.default = None;
        }
        Ok(())
    }
}

/// The profile name rule (`getCodexProfileNameError`): `Ok(())` when valid.
pub fn profile_name_error(name: &str) -> Result<(), ProfileError> {
    if name.is_empty() || name == "default" || name == "current" {
        return Err(ProfileError::InvalidName);
    }
    if name.contains('/') || name.contains('\\') || name.len() > 64 {
        return Err(ProfileError::InvalidName);
    }
    let mut chars = name.chars();
    let first = chars.next().ok_or(ProfileError::InvalidName)?;
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(ProfileError::InvalidName);
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
        return Err(ProfileError::InvalidName);
    }
    Ok(())
}

// ── js-yaml scalar quoting (dumper.js, single-line, lineWidth -1, single quotes) ──────────────

fn is_printable(c: char) -> bool {
    let c = c as u32;
    (0x20..=0x7e).contains(&c)
        || ((0xa1..=0xd7ff).contains(&c) && c != 0x2028 && c != 0x2029)
        || ((0xe000..=0xfffd).contains(&c) && c != 0xfeff)
        || (0x10000..=0x10ffff).contains(&c)
}

fn is_whitespace(c: char) -> bool {
    c == ' ' || c == '\t'
}

fn is_ns_char_or_whitespace(c: char) -> bool {
    is_printable(c) && c != '\u{feff}' && c != '\r' && c != '\n'
}

fn is_plain_safe(c: char, prev: Option<char>, inblock: bool) -> bool {
    let ns_or_ws = is_ns_char_or_whitespace(c);
    let ns = ns_or_ws && !is_whitespace(c);
    let safe = if inblock {
        ns_or_ws
    } else {
        ns_or_ws && !matches!(c, ',' | '[' | ']' | '{' | '}')
    };
    (safe && c != '#' && !(prev == Some(':') && !ns))
        || (prev.is_some_and(|p| is_ns_char_or_whitespace(p) && !is_whitespace(p)) && c == '#')
        || (prev == Some(':') && ns)
}

fn is_plain_safe_first(c: char) -> bool {
    is_printable(c)
        && c != '\u{feff}'
        && !is_whitespace(c)
        && !matches!(
            c,
            '-' | '?'
                | ':'
                | ','
                | '['
                | ']'
                | '{'
                | '}'
                | '#'
                | '&'
                | '*'
                | '!'
                | '|'
                | '='
                | '>'
                | '\''
                | '"'
                | '%'
                | '@'
                | '`'
        )
}

fn is_plain_safe_last(c: char) -> bool {
    !is_whitespace(c) && c != ':'
}

fn resolve_null(s: &str) -> bool {
    s == "~" || s == "null" || s == "Null" || s == "NULL"
}

fn resolve_bool(s: &str) -> bool {
    matches!(s, "true" | "True" | "TRUE" | "false" | "False" | "FALSE")
}

fn resolve_merge(s: &str) -> bool {
    s == "<<"
}

fn is_dec(c: char) -> bool {
    c.is_ascii_digit()
}

fn resolve_int(s: &str) -> bool {
    let b: Vec<char> = s.chars().collect();
    if b.is_empty() {
        return false;
    }
    let mut i = 0;
    let mut has_digits = false;
    if b[i] == '-' || b[i] == '+' {
        i += 1;
    }
    if i >= b.len() {
        return false;
    }
    let mut ch = b[i];
    if ch == '0' {
        if i + 1 == b.len() {
            return true;
        }
        i += 1;
        ch = b[i];
        let base = match ch {
            'b' => Some(2),
            'x' => Some(16),
            'o' => Some(8),
            _ => None,
        };
        if let Some(base) = base {
            i += 1;
            while i < b.len() {
                ch = b[i];
                if ch != '_' {
                    let ok = match base {
                        2 => ch == '0' || ch == '1',
                        8 => ('0'..='7').contains(&ch),
                        _ => ch.is_ascii_hexdigit(),
                    };
                    if !ok {
                        return false;
                    }
                    has_digits = true;
                }
                i += 1;
            }
            return has_digits && ch != '_';
        }
    }
    if ch == '_' {
        return false;
    }
    while i < b.len() {
        ch = b[i];
        if ch != '_' {
            if !is_dec(ch) {
                return false;
            }
            has_digits = true;
        }
        i += 1;
    }
    has_digits && ch != '_'
}
fn resolve_float(s: &str) -> bool {
    if s.ends_with('_') {
        return false;
    }
    let (sign, body) = match s.strip_prefix(['-', '+']) {
        Some(body) => (true, body),
        None => (false, s),
    };
    if matches!(body, ".inf" | ".Inf" | ".INF") {
        return true;
    }
    if matches!(body, ".nan" | ".NaN" | ".NAN") {
        return !sign;
    }
    let digit_underscore = |c: char| c.is_ascii_digit() || c == '_';
    fn split_exp(text: &str) -> (&str, &str) {
        match text.find(['e', 'E']) {
            Some(at) => (&text[..at], &text[at..]),
            None => (text, ""),
        }
    }
    let exp_ok = |rest: &str| -> bool {
        if rest.is_empty() {
            return true;
        }
        let b = rest.as_bytes();
        if b[0] != b'e' && b[0] != b'E' {
            return false;
        }
        let mut i = 1;
        if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
            i += 1;
        }
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        i > start && i == b.len()
    };
    // Branch 1: [-+]?[0-9][0-9_]*(\.[0-9_]*)?([eE][-+]?[0-9]+)?
    if body.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        let (mantissa, exponent) = split_exp(body);
        if let Some((int_part, frac)) = mantissa.split_once('.') {
            if !int_part.chars().next().is_some_and(|c| c.is_ascii_digit())
                || !int_part.chars().all(digit_underscore)
                || !frac.chars().all(digit_underscore)
            {
                return false;
            }
        } else if !mantissa.chars().all(digit_underscore) {
            return false;
        }
        return exp_ok(exponent);
    }
    // Branch 2: \.[0-9_]+([eE][-+]?[0-9]+)? (no sign)
    if !sign {
        if let Some(frac) = body.strip_prefix('.') {
            let (mantissa, exponent) = split_exp(frac);
            if !mantissa.is_empty() && mantissa.chars().all(digit_underscore) {
                return exp_ok(exponent);
            }
        }
    }
    false
}
fn resolve_timestamp(s: &str) -> bool {
    // ^[0-9]{4}-[0-9]{2}-[0-9]{2}$
    let date = |s: &str| -> bool {
        let b: Vec<char> = s.chars().collect();
        b.len() == 10
            && b[0..4].iter().all(|c| c.is_ascii_digit())
            && b[4] == '-'
            && b[5..7].iter().all(|c| c.is_ascii_digit())
            && b[7] == '-'
            && b[8..10].iter().all(|c| c.is_ascii_digit())
    };
    if date(s) {
        return true;
    }
    // The full timestamp form: date, [Tt] or spaces, h:m:s, optional fraction and zone.
    let b: Vec<char> = s.chars().collect();
    let n = b.len();
    let digits = |from: usize, count: usize| -> bool {
        from + count <= n && b[from..from + count].iter().all(|c| c.is_ascii_digit())
    };
    if !digits(0, 4) || b.get(4) != Some(&'-') {
        return false;
    }
    // month 1-2 digits, day 1-2 digits
    let mut i = 5;
    let mut month = 0;
    while i < n && month < 2 && b[i].is_ascii_digit() {
        i += 1;
        month += 1;
    }
    if month == 0 || b.get(i) != Some(&'-') {
        return false;
    }
    i += 1;
    let mut day = 0;
    while i < n && day < 2 && b[i].is_ascii_digit() {
        i += 1;
        day += 1;
    }
    if day == 0 {
        return false;
    }
    // separator [Tt] or [ \t]+
    if i < n && (b[i] == 'T' || b[i] == 't') {
        i += 1;
    } else {
        let start = i;
        while i < n && (b[i] == ' ' || b[i] == '\t') {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    // hour 1-2, ':' minute 2, ':' second 2
    let mut hour = 0;
    while i < n && hour < 2 && b[i].is_ascii_digit() {
        i += 1;
        hour += 1;
    }
    if hour == 0 || b.get(i) != Some(&':') || !digits(i + 1, 2) {
        return false;
    }
    i += 3;
    if b.get(i) != Some(&':') || !digits(i + 1, 2) {
        return false;
    }
    i += 3;
    // optional fraction
    if b.get(i) == Some(&'.') {
        i += 1;
        while i < n && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    // optional zone
    if i < n {
        while i < n && (b[i] == ' ' || b[i] == '\t') {
            i += 1;
        }
        if i < n {
            if b[i] == 'Z' {
                i += 1;
            } else if (b[i] == '-' || b[i] == '+') && i + 1 < n {
                i += 1;
                let mut zh = 0;
                while i < n && zh < 2 && b[i].is_ascii_digit() {
                    i += 1;
                    zh += 1;
                }
                if zh == 0 {
                    return false;
                }
                if b.get(i) == Some(&':') {
                    if !digits(i + 1, 2) {
                        return false;
                    }
                    i += 3;
                }
            } else {
                return false;
            }
        }
    }
    i == n
}

/// Whether the core schema resolves a plain scalar as a non-string type (`testImplicitResolving`).
pub fn ambiguous(s: &str) -> bool {
    resolve_null(s)
        || resolve_bool(s)
        || resolve_int(s)
        || resolve_float(s)
        || resolve_timestamp(s)
        || resolve_merge(s)
}

const DEPRECATED_BOOLEANS: [&str; 16] = [
    "y", "Y", "yes", "Yes", "YES", "on", "On", "ON", "n", "N", "no", "No", "NO", "off", "Off",
    "OFF",
];

fn deprecated_base60(s: &str) -> bool {
    // ^[-+]?[0-9_]+(?::[0-9_]+)+(?:\.[0-9_]*)?$
    let body = s.strip_prefix(['-', '+']).unwrap_or(s);
    let digit_underscore = |c: char| c.is_ascii_digit() || c == '_';
    let (int_part, frac) = match body.rsplit_once('.') {
        Some((int_part, frac)) => (int_part, Some(frac)),
        None => (body, None),
    };
    if let Some(frac) = frac {
        if !frac.chars().all(digit_underscore) {
            return false;
        }
    }
    let mut groups = int_part.split(':');
    let first = groups.next().unwrap_or("");
    if first.is_empty() || !first.chars().all(digit_underscore) {
        return false;
    }
    let mut count = 0;
    for group in groups {
        if group.is_empty() || !group.chars().all(digit_underscore) {
            return false;
        }
        count += 1;
    }
    count >= 1
}
/// Render one string scalar exactly as `js-yaml` dump does in block context (single-line).
/// `None` when the value needs a block or double-quoted style this module does not reproduce.
pub fn scalar(value: &str) -> Option<String> {
    if value.is_empty() {
        return Some("''".to_owned());
    }
    if value.contains('\n') || value.contains('\r') {
        return None;
    }
    if DEPRECATED_BOOLEANS.contains(&value) || deprecated_base60(value) {
        return Some(format!("'{}'", value.replace('\'', "''")));
    }
    let chars: Vec<char> = value.chars().collect();
    if !chars.iter().all(|c| is_printable(*c)) {
        return None;
    }
    let mut plain = is_plain_safe_first(chars[0]) && is_plain_safe_last(chars[chars.len() - 1]);
    let mut prev = None;
    for c in &chars {
        plain = plain && is_plain_safe(*c, prev, true);
        prev = Some(*c);
    }
    if plain && !ambiguous(value) {
        Some(value.to_owned())
    } else {
        Some(format!("'{}'", value.replace('\'', "''")))
    }
}

/// Emit the registry as `yaml.dump(registry, {indent: 2, lineWidth: -1})`.
pub fn emit(registry: &Registry) -> Option<String> {
    let mut out = String::new();
    out.push_str("version: ");
    out.push_str(&scalar(&registry.version)?);
    out.push('\n');
    out.push_str("default: ");
    match &registry.default {
        None => out.push_str("null"),
        Some(name) => out.push_str(&scalar(name)?),
    }
    out.push('\n');
    if registry.profiles.is_empty() {
        out.push_str("profiles: {}\n");
        return Some(out);
    }
    out.push_str("profiles:\n");
    for (name, meta) in &registry.profiles {
        out.push_str("  ");
        out.push_str(&scalar(name)?);
        out.push_str(":\n");
        for (key, field) in meta {
            out.push_str("    ");
            out.push_str(&scalar(key)?);
            out.push_str(": ");
            match field {
                Field::Null => out.push_str("null"),
                Field::Str(value) => out.push_str(&scalar(value)?),
            }
            out.push('\n');
        }
    }
    Some(out)
}

// ── parsing the canonical block-mapping form ─────────────────────────────────────────────────

#[derive(Clone, Debug)]
enum Style {
    Plain,
    Single,
    Double,
}

#[derive(Clone, Debug)]
enum Node {
    Map(Vec<(String, Node)>),
    EmptyMap,
    Null,
    Scalar(String, Style),
}

/// A parsed scalar's resolved kind: only string and null survive validation for known fields.
fn scalar_is_string(node: &Node) -> Option<String> {
    match node {
        Node::Scalar(value, Style::Single | Style::Double) => Some(value.clone()),
        Node::Scalar(value, Style::Plain) if !ambiguous(value) => Some(value.clone()),
        _ => None,
    }
}

fn scalar_is_null(node: &Node) -> bool {
    matches!(node, Node::Null)
        || matches!(node, Node::Scalar(value, Style::Plain) if resolve_null(value))
}

struct Parser<'a> {
    lines: Vec<&'a str>,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a str> {
        self.lines.get(self.pos).copied()
    }

    fn indent(line: &str) -> usize {
        line.len() - line.trim_start_matches(' ').len()
    }

    /// Parse a block mapping whose entries sit at `indent`.
    fn map(&mut self, indent: usize) -> Option<Node> {
        let mut entries: Vec<(String, Node)> = Vec::new();
        while let Some(line) = self.peek() {
            if line.trim().is_empty() {
                self.pos += 1;
                continue;
            }
            if Self::indent(line) != indent {
                break;
            }
            let content = &line[indent..];
            let (key, rest) = split_key(content)?;
            self.pos += 1;
            let value = if rest.is_empty() {
                // Nested mapping at indent + 2, or an empty mapping written inline elsewhere.
                match self.peek() {
                    Some(next) if Self::indent(next) > indent => self.map(indent + 2)?,
                    _ => Node::EmptyMap,
                }
            } else {
                let rest = rest.strip_prefix(' ')?;
                self.value(rest)?
            };
            entries.push((key, value));
        }
        Some(Node::Map(entries))
    }

    fn value(&self, text: &str) -> Option<Node> {
        let text = text.trim_end();
        if text == "{}" {
            return Some(Node::EmptyMap);
        }
        if text == "null" || text == "~" {
            return Some(Node::Null);
        }
        if let Some(inner) = text.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')) {
            return Some(Node::Scalar(inner.replace("''", "'"), Style::Single));
        }
        if let Some(inner) = text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
            return Some(Node::Scalar(unescape_double(inner)?, Style::Double));
        }
        if text.is_empty() {
            return Some(Node::Null);
        }
        Some(Node::Scalar(text.to_owned(), Style::Plain))
    }
}

fn unescape_double(s: &str) -> Option<String> {
    // The registry never needs escapes beyond \\ and \"; decline anything else.
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next()? {
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                _ => return None,
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// Split `key: rest` honoring quoted keys; returns the key and the remainder after the colon.
fn split_key(content: &str) -> Option<(String, &str)> {
    if let Some(rest) = content.strip_prefix('\'') {
        let end = rest.find('\'')?;
        let key = rest[..end].replace("''", "'");
        let after = &rest[end + 1..];
        let after = after.strip_prefix(':')?;
        return Some((key, after));
    }
    if let Some(rest) = content.strip_prefix('"') {
        let end = rest.find('"')?;
        let key = unescape_double(&rest[..end])?;
        let after = &rest[end + 1..];
        let after = after.strip_prefix(':')?;
        return Some((key, after));
    }
    let colon = content.find(':')?;
    let key = &content[..colon];
    if key.is_empty() || key.contains(' ') {
        return None;
    }
    Some((key.to_owned(), &content[colon + 1..]))
}

fn field(node: &Node) -> Option<Field> {
    if scalar_is_null(node) {
        Some(Field::Null)
    } else {
        scalar_is_string(node).map(Field::Str)
    }
}

fn validate_meta(node: &Node) -> Option<Meta> {
    let Node::Map(entries) = node else {
        return None;
    };
    let get = |key: &str| entries.iter().find(|(k, _)| k == key).map(|(_, v)| v);
    // type must be the string "codex".
    if scalar_is_string(get("type")?)?.as_str() != "codex" {
        return None;
    }
    // created must be a string.
    scalar_is_string(get("created")?)?;
    // last_used string or null.
    let last = get("last_used")?;
    if !scalar_is_null(last) && scalar_is_string(last).is_none() {
        return None;
    }
    for key in ["email", "account_id"] {
        if let Some(value) = get(key) {
            scalar_is_string(value)?;
        }
    }
    if let Some(value) = get("plan_type") {
        if !scalar_is_null(value) && scalar_is_string(value).is_none() {
            return None;
        }
    }
    let mut meta = Meta::new();
    for (key, value) in entries {
        meta.push((key.clone(), field(value)?));
    }
    Some(meta)
}

/// Parse and validate a registry, declining anything not provably round-trippable.
pub fn parse(bytes: &[u8]) -> Option<Registry> {
    let text = std::str::from_utf8(bytes).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    let mut parser = Parser { lines, pos: 0 };
    let root = parser.map(0)?;
    if parser.pos != parser.lines.len() {
        return None;
    }
    let Node::Map(entries) = &root else {
        return None;
    };
    let get = |key: &str| entries.iter().find(|(k, _)| k == key).map(|(_, v)| v);
    let version = match get("version") {
        Some(node) => scalar_is_string(node).unwrap_or_else(|| SCHEMA_VERSION.to_owned()),
        None => SCHEMA_VERSION.to_owned(),
    };
    let default = match get("default") {
        None | Some(Node::Null) => None,
        Some(node) if scalar_is_null(node) => None,
        Some(node) => {
            let name = scalar_is_string(node)?;
            profile_name_error(&name).ok()?;
            Some(name)
        }
    };
    let profiles_node = get("profiles")?;
    let mut profiles: Vec<(String, Meta)> = Vec::new();
    match profiles_node {
        Node::EmptyMap => {}
        Node::Map(entries) => {
            for (name, meta_node) in entries {
                profile_name_error(name).ok()?;
                profiles.push((name.clone(), validate_meta(meta_node)?));
            }
        }
        _ => return None,
    }
    if let Some(default) = &default {
        if !profiles.iter().any(|(n, _)| n == default) {
            return None;
        }
    }
    let registry = Registry {
        version,
        default,
        profiles,
    };
    // Only accept a registry whose canonical re-emission is the exact input bytes.
    if emit(&registry)?.as_bytes() != bytes {
        return None;
    }
    Some(registry)
}

/// Read, parse and validate the registry file; `None` when absent (an empty registry) or declined.
/// An absent file is `Some(empty)`; an unreadable or invalid one is `None` so TypeScript answers.
pub fn read(path: &Path) -> Option<Registry> {
    match crate::read_bounded(path, 1024 * 1024) {
        Ok(bytes) => parse(&bytes),
        Err(error) if error.cause == aac_core::Cause::NotInstalled => Some(Registry::empty()),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const EMPTY: &str = "version: '1.0'\ndefault: null\nprofiles: {}\n";
    const ONE: &str = "version: '1.0'\ndefault: null\nprofiles:\n  gmail:\n    type: codex\n    created: '2026-11-19T00:00:00.000Z'\n    last_used: null\n    email: a@b.c\n";

    #[test]
    fn round_trips_canonical_registries() {
        for text in [EMPTY, ONE] {
            let registry = parse(text.as_bytes()).unwrap();
            assert_eq!(emit(&registry).unwrap(), text);
        }
    }

    #[test]
    fn quotes_ambiguous_scalars() {
        assert_eq!(scalar("1.0").unwrap(), "'1.0'");
        assert_eq!(scalar("codex").unwrap(), "codex");
        assert_eq!(scalar("a@b.c").unwrap(), "a@b.c");
        assert_eq!(scalar("123@x.com").unwrap(), "123@x.com");
        assert_eq!(scalar("0123").unwrap(), "'0123'");
        assert_eq!(scalar("null").unwrap(), "'null'");
        assert_eq!(scalar("no").unwrap(), "'no'");
        assert_eq!(scalar("a: b@c.com").unwrap(), "'a: b@c.com'");
        assert_eq!(
            scalar("2026-11-19T00:00:00.000Z").unwrap(),
            "'2026-11-19T00:00:00.000Z'"
        );
        assert_eq!(scalar("").unwrap(), "''");
    }

    #[test]
    fn create_and_update_preserve_order() {
        let mut registry = parse(ONE.as_bytes()).unwrap();
        registry
            .create(
                "work",
                "2026-11-19T02:00:00.000Z",
                vec![
                    ("email".to_owned(), Field::Str("w@x.y".to_owned())),
                    ("plan_type".to_owned(), Field::Str("pro".to_owned())),
                    ("account_id".to_owned(), Field::Str("acct-2".to_owned())),
                ],
            )
            .unwrap();
        let text = emit(&registry).unwrap();
        assert!(text.contains("  work:\n    type: codex\n    created: '2026-11-19T02:00:00.000Z'\n    last_used: null\n    email: w@x.y\n    plan_type: pro\n    account_id: acct-2\n"));
        assert_eq!(parse(text.as_bytes()).unwrap(), registry);
        registry
            .update(
                "work",
                vec![(
                    "last_used".to_owned(),
                    Field::Str("2026-11-19T03:00:00.000Z".to_owned()),
                )],
            )
            .unwrap();
        assert!(
            emit(&registry)
                .unwrap()
                .contains("    last_used: '2026-11-19T03:00:00.000Z'\n")
        );
    }

    #[test]
    fn declines_non_canonical_input() {
        // A quoted plain-safe scalar would re-emit unquoted: declined.
        assert!(parse(b"version: '1.0'\ndefault: null\nprofiles:\n  p:\n    type: 'codex'\n    created: '2026-11-19T00:00:00.000Z'\n    last_used: null\n").is_none());
        // An unquoted timestamp resolves to a date, not a string: declined.
        assert!(parse(b"version: '1.0'\ndefault: null\nprofiles:\n  p:\n    type: codex\n    created: 2026-11-19T00:00:00.000Z\n    last_used: null\n").is_none());
    }

    #[test]
    fn name_rule() {
        assert!(profile_name_error("gmail").is_ok());
        assert!(profile_name_error("work_2-x").is_ok());
        assert!(profile_name_error("default").is_err());
        assert!(profile_name_error("Bad").is_err());
        assert!(profile_name_error("-lead").is_err());
        assert!(profile_name_error("a/b").is_err());
    }
}
