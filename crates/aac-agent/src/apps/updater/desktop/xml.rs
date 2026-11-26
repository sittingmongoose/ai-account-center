//! The two small XML readers the desktop updater needs, without an XML crate:
//! property lists (Python `plistlib` for `Info.plist` and `hdiutil -plist`) and
//! the `Identity` element of an MSIX `AppxManifest.xml` (Python `ElementTree`).
//! Anything malformed is `None`: callers treat it as unreadable, never as an answer.
use serde_json::{Map, Number, Value};
use std::collections::HashMap;

#[derive(Debug, PartialEq)]
enum Token<'a> {
    Open {
        name: &'a str,
        attrs: Vec<(&'a str, String)>,
        empty: bool,
    },
    Close(&'a str),
    Text(String),
}

/// Character and the five predefined entity references.
fn decode(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 1..];
        let end = tail.find(';')?;
        let name = &tail[..end];
        let ch = match name {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let code = if let Some(hex) = name.strip_prefix("#x") {
                    u32::from_str_radix(hex, 16).ok()?
                } else {
                    name.strip_prefix('#')?.parse().ok()?
                };
                char::from_u32(code)?
            }
        };
        out.push(ch);
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

fn is_space(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\r' | '\n')
}

fn tokens(text: &str) -> Option<Vec<Token<'_>>> {
    let mut out = Vec::new();
    let mut rest = text.strip_prefix('\u{feff}').unwrap_or(text);
    while !rest.is_empty() {
        let Some(at) = rest.find('<') else {
            out.push(Token::Text(decode(rest)?));
            break;
        };
        if at > 0 {
            out.push(Token::Text(decode(&rest[..at])?));
        }
        rest = &rest[at..];
        if let Some(body) = rest.strip_prefix("<?") {
            rest = &body[body.find("?>")? + 2..];
        } else if let Some(body) = rest.strip_prefix("<!--") {
            rest = &body[body.find("-->")? + 3..];
        } else if let Some(body) = rest.strip_prefix("<![CDATA[") {
            let end = body.find("]]>")?;
            out.push(Token::Text(body[..end].to_owned()));
            rest = &body[end + 3..];
        } else if let Some(body) = rest.strip_prefix("<!") {
            // A DOCTYPE without an internal subset (a property list's).
            let end = body.find('>')?;
            if body[..end].contains('[') {
                return None;
            }
            rest = &body[end + 1..];
        } else if let Some(body) = rest.strip_prefix("</") {
            let end = body.find('>')?;
            out.push(Token::Close(body[..end].trim_end_matches(is_space)));
            rest = &body[end + 1..];
        } else {
            let (token, next) = open_tag(&rest[1..])?;
            out.push(token);
            rest = next;
        }
    }
    Some(out)
}

/// One start tag after its `<`: the token and the text after its `>`.
fn open_tag(body: &str) -> Option<(Token<'_>, &str)> {
    let name_end = body.find(|ch: char| is_space(ch) || ch == '/' || ch == '>')?;
    let name = &body[..name_end];
    if name.is_empty() {
        return None;
    }
    let mut rest = &body[name_end..];
    let mut attrs = Vec::new();
    loop {
        rest = rest.trim_start_matches(is_space);
        if let Some(next) = rest.strip_prefix("/>") {
            return Some((
                Token::Open {
                    name,
                    attrs,
                    empty: true,
                },
                next,
            ));
        }
        if let Some(next) = rest.strip_prefix('>') {
            return Some((
                Token::Open {
                    name,
                    attrs,
                    empty: false,
                },
                next,
            ));
        }
        let equals = rest.find('=')?;
        let key = rest[..equals].trim_end_matches(is_space);
        if key.is_empty() || key.contains(|ch: char| is_space(ch) || ch == '<' || ch == '>') {
            return None;
        }
        rest = rest[equals + 1..].trim_start_matches(is_space);
        let quote = rest.chars().next().filter(|ch| *ch == '"' || *ch == '\'')?;
        let value_end = rest[1..].find(quote)? + 1;
        let value = &rest[1..value_end];
        if value.contains('<') {
            return None;
        }
        attrs.push((key, decode(value)?));
        rest = &rest[value_end + 1..];
    }
}

fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// Python `ElementTree.fromstring(manifest)`: the attributes of the root's first
/// direct child named `Identity` (any namespace prefix). `None` when the document is
/// malformed or has no such element.
pub(super) fn appx_identity(text: &str) -> Option<HashMap<String, String>> {
    let tokens = tokens(text)?;
    let mut stack: Vec<&str> = Vec::new();
    let mut roots = 0;
    let mut found = None;
    for token in &tokens {
        match token {
            Token::Open { name, attrs, empty } => {
                if stack.len() == 1 && found.is_none() && local(name) == "Identity" {
                    found = Some(
                        attrs
                            .iter()
                            .map(|(key, value)| ((*key).to_owned(), value.clone()))
                            .collect(),
                    );
                }
                if stack.is_empty() {
                    roots += 1;
                    if roots > 1 {
                        return None;
                    }
                }
                if !*empty {
                    stack.push(name);
                }
            }
            Token::Close(name) => {
                if stack.pop() != Some(*name) {
                    return None;
                }
            }
            Token::Text(text) => {
                if stack.is_empty() && !text.chars().all(is_space) {
                    return None;
                }
            }
        }
    }
    if !stack.is_empty() || roots != 1 {
        return None;
    }
    found
}

/// Python `plistlib.loads` for an XML property list, as JSON. `None` when malformed.
pub(super) fn plist(text: &str) -> Option<Value> {
    let tokens = tokens(text)?;
    let mut items = tokens
        .into_iter()
        .filter(|token| !matches!(token, Token::Text(text) if text.chars().all(is_space)))
        .peekable();
    match items.next()? {
        Token::Open {
            name: "plist",
            empty: false,
            ..
        } => {}
        _ => return None,
    }
    let value = plist_value(&mut items, 0)?;
    if items.next()? != Token::Close("plist") || items.next().is_some() {
        return None;
    }
    Some(value)
}

fn plist_text<'a>(items: &mut impl Iterator<Item = Token<'a>>, name: &str) -> Option<String> {
    let mut text = String::new();
    loop {
        match items.next()? {
            Token::Text(part) => text.push_str(&part),
            Token::Close(close) if close == name => return Some(text),
            _ => return None,
        }
    }
}

fn plist_value<'a>(
    items: &mut std::iter::Peekable<impl Iterator<Item = Token<'a>>>,
    depth: usize,
) -> Option<Value> {
    if depth > 64 {
        return None;
    }
    let Token::Open { name, empty, .. } = items.next()? else {
        return None;
    };
    Some(match (name, empty) {
        ("true", true) => Value::Bool(true),
        ("false", true) => Value::Bool(false),
        ("dict", true) => Value::Object(Map::new()),
        ("array", true) => Value::Array(Vec::new()),
        ("string" | "data" | "date", true) => Value::String(String::new()),
        ("string" | "data" | "date", false) => Value::String(plist_text(items, name)?),
        ("integer", false) => Value::Number(Number::from(
            plist_text(items, name)?.trim().parse::<i64>().ok()?,
        )),
        ("real", false) => Value::Number(Number::from_f64(
            plist_text(items, name)?.trim().parse::<f64>().ok()?,
        )?),
        ("array", false) => {
            let mut values = Vec::new();
            loop {
                if items.peek()? == &Token::Close("array") {
                    items.next();
                    break Value::Array(values);
                }
                values.push(plist_value(items, depth + 1)?);
            }
        }
        ("dict", false) => {
            let mut fields = Map::new();
            loop {
                match items.next()? {
                    Token::Close("dict") => break Value::Object(fields),
                    Token::Open {
                        name: "key",
                        empty: false,
                        ..
                    } => {
                        let key = plist_text(items, "key")?;
                        let value = plist_value(items, depth + 1)?;
                        fields.insert(key, value);
                    }
                    Token::Open {
                        name: "key",
                        empty: true,
                        ..
                    } => {
                        let value = plist_value(items, depth + 1)?;
                        fields.insert(String::new(), value);
                    }
                    _ => return None,
                }
            }
        }
        _ => return None,
    })
}

/// Python `plistlib.dumps` for the LaunchAgent property list: a dict of strings,
/// booleans, integers, string arrays and string dicts.
pub(super) fn plist_xml(value: &Value) -> String {
    fn escape(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }
    fn write(out: &mut String, value: &Value, indent: usize) {
        let pad = "\t".repeat(indent);
        match value {
            Value::Bool(true) => out.push_str(&format!("{pad}<true/>\n")),
            Value::Bool(false) => out.push_str(&format!("{pad}<false/>\n")),
            Value::Number(number) => out.push_str(&format!("{pad}<integer>{number}</integer>\n")),
            Value::String(text) => {
                out.push_str(&format!("{pad}<string>{}</string>\n", escape(text)));
            }
            Value::Array(values) => {
                out.push_str(&format!("{pad}<array>\n"));
                for item in values {
                    write(out, item, indent + 1);
                }
                out.push_str(&format!("{pad}</array>\n"));
            }
            Value::Object(fields) => {
                out.push_str(&format!("{pad}<dict>\n"));
                for (key, item) in fields {
                    out.push_str(&format!("{pad}\t<key>{}</key>\n", escape(key)));
                    write(out, item, indent + 1);
                }
                out.push_str(&format!("{pad}</dict>\n"));
            }
            Value::Null => out.push_str(&format!("{pad}<string></string>\n")),
        }
    }
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n",
    );
    write(&mut out, value, 0);
    out.push_str("</plist>\n");
    out
}
