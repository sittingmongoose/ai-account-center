use super::*;
fn seen(i: &Inputs, good: &V) -> Option<i64> {
    let first = i.at(&good["firstSeenAt"])?;
    Some(i.at(&good["writtenAtMs"]).map(|t| t.min(first)).unwrap_or(first))
}
pub(super) fn source(i: &Inputs) -> Option<V> {
    let q = &i.data["quota"];
    if q.is_null() {
        return Some(
            json!({"id":"quota-watch","label":"Quota watch (estimates)","state":"unconfigured","asOf":null,"ageSeconds":null,"staleAfterSeconds":null,"detail":"Per-stream quota attribution is not connected on this server."}),
        );
    }
    let at = if q["good"].is_object() {
        Some(seen(i, &q["good"])?)
    } else {
        None
    };
    let mut out = json!({"id":"quota-watch","label":"Quota watch (estimates)","asOf":i.iso(at),"ageSeconds":i.age(at),"staleAfterSeconds":1800});
    let showing = at
        .map(|t| format!(" Showing the report from {}.", i.clock(t)))
        .unwrap_or_default();
    let (state, detail) = if q["scannedAt"].is_null() {
        ("missing", "Waiting for the first scan.".into())
    } else if q["state"] == "error" {
        ("error", format!("The quota folder could not be listed.{showing}"))
    } else if q["problem"].is_object() {
        (
            "unreadable",
            format!(
                "quota/report.json unreadable since {} ({}).{showing}",
                i.clock(i.at(&q["problem"]["since"])?),
                s(&q["problem"]["reason"])
            ),
        )
    } else if q["good"].is_null() {
        ("missing", "No quota-watch report at v1/quota/report.json.".into())
    } else if number(&out["ageSeconds"])? > 1800.0 {
        (
            "stale",
            format!(
                "No new quota report since {} (quota-watch writes every 10 minutes).",
                i.clock(at.unwrap_or(i.now))
            ),
        )
    } else {
        (
            "ok",
            format!(
                "Report from {}. Every number is an estimate.",
                i.clock(at.unwrap_or(i.now))
            ),
        )
    };
    out["state"] = json!(state);
    out["detail"] = json!(detail);
    Some(out)
}
fn meter(meter: &str) -> String {
    match meter {
        "five_hour" => "5-hour window".into(),
        "seven_day" => "7-day window".into(),
        "seven_day_opus" => "7-day Opus window".into(),
        "seven_day_sonnet" => "7-day Sonnet window".into(),
        "credits_balance" => "Credits".into(),
        _ => meter.replace('_', " "),
    }
}
fn rounded_value(value: &V, unit: &V) -> V {
    number(value)
        .map(|n| {
            let scale = if unit == "percent" { 10.0 } else { 100.0 };
            json!(math_round(n * scale) / scale)
        })
        .unwrap_or(V::Null)
}
fn amount(value: &V, unit: &V, m: &str) -> Option<String> {
    let Some(n) = number(value) else {
        return Some("not read".into());
    };
    if unit.is_null() {
        return Some("not read".into());
    }
    let r = number(&rounded_value(value, unit))?;
    let tiny = n > 0.0 && r == 0.0;
    if unit == "credits" {
        Some(if tiny {
            "≈ under 0.01 credits".into()
        } else {
            format!("≈ {} credits", fixed(r, 2)?)
        })
    } else if unit == "percent" {
        let points = if tiny {
            "under 0.1 points".into()
        } else {
            format!("{} {}", fixed(r, 1)?, if r == 1.0 { "point" } else { "points" })
        };
        Some(format!("≈ {points} of the {}", meter(m)))
    } else {
        None
    }
}
fn coverage(value: &V, noun: &str) -> Option<V> {
    Some(match number(value) {
        None => json!(format!("coverage of the {noun} not known")),
        Some(n) if n >= 0.5 => V::Null,
        Some(n) => json!(format!("covers {}% of the {noun}", round_word(n * 100.0)?)),
    })
}
fn budget_text(b: &V) -> Option<String> {
    let Some(used) = number(&b["use"]) else {
        return Some("No reading yet.".into());
    };
    let lower = if b["known"] == true { "" } else { "at least " };
    let per = if s(&b["per"]).is_empty() {
        String::new()
    } else {
        format!(
            " {}",
            match s(&b["per"]) {
                "day" => "today".to_owned(),
                "week" => "this week".to_owned(),
                "window" => "in the window".to_owned(),
                word => format!("per {word}"),
            }
        )
    };
    let Some(limit) = number(&b["limit"]) else {
        return Some(format!(
            "{lower}{}{per}; no limit set.",
            amount(&b["use"], &b["unit"], s(&b["meter"]))?
        ));
    };
    let what = if b["unit"] == "credits" {
        "credits".into()
    } else {
        format!("points of the {}", meter(s(&b["meter"])))
    };
    let digits = if b["unit"] == "credits" { 2 } else { 1 };
    let pct = match number(&b["percent"]) {
        Some(p) => format!(" ({}%)", round_word(p)?),
        None => String::new(),
    };
    Some(format!(
        "{lower}≈ {} of {} {what}{per}{pct}.",
        fixed(used, digits)?,
        fixed(limit, digits)?
    ))
}
fn labels(i: &Inputs) -> HashMap<String, String> {
    let mut labels = HashMap::new();
    let mut clash = HashSet::new();
    for row in array(&i.data["reports"]["tasks"]) {
        let id = s(&row["doc"]["stream"]).to_owned();
        let label = row["doc"]["streamLabel"].as_str().unwrap_or(&id).to_owned();
        if labels.get(&id).is_some_and(|v| v != &label) {
            clash.insert(id.clone());
        }
        labels.insert(id, label);
    }
    for id in clash {
        labels.remove(&id);
    }
    labels
}
fn stream_label(stream: &V, labels: &HashMap<String, String>) -> V {
    let id = s(stream);
    if id.starts_with('(') {
        stream.clone()
    } else {
        labels.get(id).map(|s| json!(s)).unwrap_or(stream.clone())
    }
}
pub(super) fn compose(i: &Inputs) -> Option<V> {
    let good = &i.data["quota"]["good"];
    if good.is_null() {
        return Some(
            json!({"source":"quota-watch","estimate":true,"asOf":null,"generatedAt":null,"writer":null,"methodId":null,"method":null,"periods":[],"budgets":[],"unattributed":[],"machinesNotScanned":[],"overLimitBudgets":0}),
        );
    }
    let doc = &good["doc"];
    let labels = labels(i);
    let mut periods = Vec::new();
    for id in ["day", "week", "window"] {
        let p = &doc["periods"][id];
        if p.is_null() {
            continue;
        }
        let mut streams: Vec<V> = Vec::new();
        for row in p["rows"].as_array()? {
            let at=streams.iter().position(|s|s["stream"]==row["stream"]).unwrap_or_else(||{streams.push(json!({"stream":row["stream"],"label":stream_label(&row["stream"],&labels),"bucket":s(&row["stream"]).starts_with('('),"rows":[]}));streams.len()-1});
            streams[at]["rows"].as_array_mut()?.push(json!({"account":row["account"],"meter":row["meter"],"meterLabel":meter(s(&row["meter"])),"unit":row["unit"],"value":rounded_value(&row["value"],&row["unit"]),"valueText":amount(&row["value"],&row["unit"],s(&row["meter"]))?,"estimate":row["estimate"]}));
        }
        streams.sort_by(|a, b| {
            (a["bucket"] == true)
                .cmp(&(b["bucket"] == true))
                .then_with(|| i.ctx.compare(s(&a["label"]), s(&b["label"])))
        });
        let label = match id {
            "day" => "Today".to_owned(),
            "week" => "This week".to_owned(),
            _ => {
                let hours = match (i.at(&p["start"]), i.at(&p["end"])) {
                    (Some(a), Some(b)) => math_round((b - a) as f64 / 3_600_000.0),
                    _ => 0.0,
                };
                if hours != 0.0 {
                    format!(
                        "Last {} hour{}",
                        round_word(hours)?,
                        if hours == 1.0 { "" } else { "s" }
                    )
                } else {
                    "Window".into()
                }
            }
        };
        periods.push(json!({"id":id,"label":label,"period":p["period"],"start":p["start"],"end":p["end"],"coverage":p["coverage"],"coverageText":coverage(&p["coverage"],id)?,"streams":streams}));
    }
    let mut budgets = Vec::new();
    for b in doc["budgets"].as_array()? {
        let over = number(&b["percent"]).is_some_and(|v| v >= 100.0)
            || matches!((number(&b["limit"]),number(&b["use"])),(Some(l),Some(u))if u>=l);
        let per = b.get("per").filter(|v| !v.is_null()).unwrap_or(&b["period"]);
        let noun = if s(per).starts_with("day") {
            "day"
        } else if s(per).starts_with("week") {
            "week"
        } else {
            "window"
        };
        budgets.push(json!({"stream":b["stream"],"streamLabel":stream_label(&b["stream"],&labels),"desc":b["desc"],"accounts":b["accounts"],"unit":b["unit"],"meter":b["meter"],"meterLabel":meter(s(&b["meter"])),"per":b["per"],"limit":b["limit"],"use":b["use"],"known":b["known"],"percent":b["percent"],"period":b["period"],"coverage":b["coverage"],"coverageText":coverage(&b["coverage"],noun)?,"overLimit":over,"estimate":b["estimate"],"text":budget_text(b)?}));
    }
    let mut unattributed = Vec::new();
    for u in doc["unattributed"].as_array()? {
        unattributed.push(json!({"account":u["account"],"meter":u["meter"],"meterLabel":meter(s(&u["meter"])),"unit":u["unit"],"amount":rounded_value(&u["amount"],&u["unit"]),"amountText":amount(&u["amount"],&u["unit"],s(&u["meter"]))?,"reason":u["reason"],"why":u["why"],"estimate":u["estimate"]}));
    }
    Some(
        json!({"source":"quota-watch","estimate":true,"asOf":i.iso(Some(seen(i,good)?)),"generatedAt":doc["generatedAt"],"writer":doc["writer"],"methodId":doc["methodId"],"method":doc["method"],"periods":periods,"budgets":budgets,"unattributed":unattributed,"machinesNotScanned":doc["machinesNotScanned"],"overLimitBudgets":budgets.iter().filter(|b|b["overLimit"]==true).count()}),
    )
}
