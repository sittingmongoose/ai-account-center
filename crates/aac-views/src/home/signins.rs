//! claude-signins.mjs: the Claude rows' sign-in renewal line and the "needs you" count.
use super::Ctx;
use crate::js::*;
use serde_json::Value;

fn renewal(data: &Value) -> Option<&Value> {
    let v = data.get("claudeSignIns");
    matches!(get(v, "signIns"), Some(Value::Array(_))).then_some(v?)
}

/// `email.trim().toLowerCase()`, proven for ASCII only.
fn address(email: &str) -> R<String> {
    let t = trim(email);
    if !t.is_ascii() {
        return Err(Decline("case-map"));
    }
    Ok(t.to_ascii_lowercase())
}

/// claude-signins.mjs `needsFor(data, email)`; `None` is "for any account".
pub fn needs_count(data: &Value, email: Option<Option<&Value>>) -> R<usize> {
    let Some(r) = renewal(data) else { return Ok(0) };
    let Some(Value::Array(needs)) = r.get("needsYou") else {
        return Ok(0);
    };
    let want = match email {
        None => None,
        Some(Some(Value::String(e))) => Some(address(e)?),
        Some(_) => None,
    };
    Ok(needs
        .iter()
        .filter(|need| {
            truthy(Some(*need))
                && match &want {
                    None => true,
                    Some(a) => is_str(need.get("email"), a),
                }
        })
        .count())
}

/// claude-signins.mjs `claudeSignInLine(data, email, now)`.
pub fn line(cx: &Ctx, data: &Value, email: Option<&Value>) -> R<String> {
    let Some(r) = renewal(data) else {
        return Ok(String::new());
    };
    let addr = match email {
        Some(Value::String(e)) => address(e)?,
        _ => String::new(),
    };
    if addr.is_empty() {
        return Ok(String::new());
    }
    let rows: Vec<&Value> = array(r.get("signIns"))
        .iter()
        .filter(|row| is_str(get(Some(*row), "email"), &addr))
        .collect();
    if rows.is_empty() {
        return Ok(String::new());
    }
    if needs_count(data, Some(email))? > 0 {
        return Ok("Sign-in renewal needs you".into());
    }
    if truthy(r.get("stale")) {
        return Ok("Sign-in renewal stopped".into());
    }
    let mut out: Vec<&Value> = rows
        .iter()
        .copied()
        .filter(|row| {
            let state = get(Some(*row), "state");
            is_str(state, "logged_out") || is_str(state, "no_refresh_token")
        })
        .collect();
    if !out.is_empty() {
        // signInsFor's order: Mac, Windows, the other computers, then the profile name.
        let order: Vec<&str> = ["mac", "windows"]
            .into_iter()
            .chain(
                cx.computers
                    .iter()
                    .map(|(id, _)| id.as_str())
                    .filter(|id| *id != "mac" && *id != "windows"),
            )
            .collect();
        let rank = |row: &Value| match get(Some(row), "host") {
            Some(Value::String(h)) => order.iter().position(|id| id == h).unwrap_or(9),
            _ => 9,
        };
        let mut failed = None;
        let mut keyed = Vec::new();
        for row in &out {
            keyed.push((rank(row), string_of(get(Some(*row), "profile"))?, *row));
        }
        keyed.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| match cx.fmt.compare(&a.1, &b.1) {
                    Ok(o) => o,
                    Err(d) => {
                        failed = Some(d);
                        std::cmp::Ordering::Equal
                    }
                })
        });
        if let Some(d) = failed {
            return Err(d);
        }
        out = keyed.into_iter().map(|(_, _, row)| row).collect();
        let mut words: Vec<String> = Vec::new();
        for row in out {
            let host = match get(Some(row), "host") {
                Some(Value::String(h)) => h.as_str(),
                _ => return Err(Decline("type")),
            };
            let word = match lookup_owned(&cx.computers, host)? {
                Some(label) if !label.is_empty() => label.to_string(),
                _ => host.to_string(),
            };
            if !words.contains(&word) {
                words.push(word);
            }
        }
        return Ok(format!("Signed out on {}", words.join(", ")));
    }
    let mut ends = Vec::new();
    for row in rows {
        if let Some(t) = valid_date(get(Some(row), "signInEndsAt"))? {
            ends.push(t);
        }
    }
    Ok(match ends.iter().min() {
        None => "Auto-renews".into(),
        Some(first) => format!("Auto-renews · next end {}", cx.fmt.day(*first)?),
    })
}

/// `HOST_LABELS[host]`: the registry is an object literal, so `Object.prototype` names decline.
fn lookup_owned<'c>(computers: &'c [(String, String)], key: &str) -> R<Option<&'c str>> {
    lookup(&[], key)?;
    Ok(computers
        .iter()
        .find(|(id, _)| id == key)
        .map(|(_, label)| label.as_str()))
}
