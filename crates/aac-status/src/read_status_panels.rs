use super::*;
fn warnings(sample: &V) -> Option<Vec<String>> {
    if sample.is_null() {
        return Some(vec![]);
    }
    let mut out = Vec::new();
    if let Some(free) = number(&gb(&sample["disk"]["freeBytes"]))
        && free < 50.0
    {
        out.push(format!("Disk {} GB free, under the 50 GB floor", view::fixed(free, 1)?));
    }
    if let Some(free) = number(&gb(&sample["memAvailableBytes"]))
        && free < 6.0
    {
        out.push(format!(
            "Memory {} GB available, under the 6 GB floor",
            view::fixed(free, 1)?
        ));
    }
    Some(out)
}
pub(super) fn machines(i: &Inputs, browsers: &V) -> Option<Vec<V>> {
    let mut rows = Vec::new();
    for id in i.data["machineIds"].as_array()? {
        let id = s(id);
        let c = &i.data["machines"][id];
        c.as_object()?;
        let sample = &c["data"];
        let at = i.at(&c["asOf"]);
        let age = i.age(at);
        let (mut state, mut detail) = if c["error"].is_object() {
            let shown = at
                .map(|t| format!(" Showing the reading from {}.", i.clock(t)))
                .unwrap_or_default();
            (
                "unreachable",
                Some(format!(
                    "{} (since {}).{shown}",
                    trim_period(s(&c["error"]["reason"])),
                    i.clock(i.at(&c["error"]["since"])?)
                )),
            )
        } else if at.is_none() {
            ("stale", Some("Waiting for the first reading.".to_owned()))
        } else if number(&age)? > 180.0 {
            ("stale", Some(format!("No new reading since {}.", i.clock(at?))))
        } else {
            ("ok", None)
        };
        for (key, label, noun, at, held) in [
            (
                "cpu",
                "CPU",
                "CPU",
                i.at(&sample["cpuReadAt"]),
                number(&sample["busyPct"]).is_some(),
            ),
            (
                "disk",
                "Disk",
                "disk",
                i.at(&sample["diskReadAt"]),
                sample["disk"].is_object(),
            ),
            (
                "memory",
                "Memory",
                "memory",
                i.at(&sample["memoryReadAt"]),
                number(&sample["memAvailableBytes"]).is_some(),
            ),
        ] {
            let ps = array(&c["problems"]);
            let Some(p) = ps.iter().find(|p| p["item"] == key) else {
                continue;
            };
            if state == "ok" {
                state = "stale";
            }
            let kept = if held && at.is_some() {
                format!(" Showing the {noun} reading from {}.", i.clock(at?))
            } else {
                format!(" No earlier {noun} reading.")
            };
            let words = format!(
                "{label}: {} (since {}).{kept}",
                trim_period(s(&p["reason"])),
                i.clock(i.at(&p["since"])?)
            );
            detail = Some(detail.map(|d| format!("{d} {words}")).unwrap_or(words));
        }
        let bs = array(&i.data["browsers"]["data"]);
        let browser = bs.iter().find(|b| b["machine"] == id);
        let b = browser.unwrap_or(&V::Null);
        rows.push(json!({"id":id,"label":i.data["machineLabels"][id],"state":state,"asOf":i.iso(at),"ageSeconds":age,"detail":detail,"cpu":if sample.is_object(){json!({"cores":sample["cores"],"load1":round1(&sample["load1"]),"busyPct":sample["busyPct"]})}else{V::Null},"memory":if sample.is_object(){json!({"availableGb":gb(&sample["memAvailableBytes"]),"totalGb":gb(&sample["memTotalBytes"])})}else{V::Null},"disk":if sample["disk"].is_object(){json!([{"mount":sample["disk"]["mount"],"freeGb":number(&gb(&sample["disk"]["freeBytes"])).unwrap_or(0.0),"totalGb":number(&gb(&sample["disk"]["totalBytes"])).unwrap_or(0.0)}])}else{json!([])},"browsers":b["browsers"],"recordings":b["recordings"],"gpuPct":b["gpuPct"],"sessions":b.get("sessions").cloned().unwrap_or(json!([])),"slotsSource":part_ref(i,browsers,&i.data["browsers"],browser,"browser-check has no row for this machine.")?,"warnings":warnings(sample)?}));
    }
    Some(rows)
}
pub(super) fn machines_source(i: &Inputs, rows: &[V]) -> Option<V> {
    let at = i.data["machineIds"]
        .as_array()?
        .iter()
        .filter_map(|id| i.at(&i.data["machines"][s(id)]["asOf"]))
        .max();
    let mut out =
        json!({"id":"machines","label":"Machines","asOf":i.iso(at),"ageSeconds":i.age(at),"staleAfterSeconds":180});
    let (state, detail) = if at.is_none() && rows.iter().all(|r| r["state"] != "unreachable") {
        ("missing", "Waiting for the first reading.".into())
    } else {
        let off: Vec<_> = rows.iter().filter(|r| r["state"] != "ok").collect();
        if off.is_empty() {
            (
                "ok",
                format!("All {} machines read by {}.", rows.len(), i.clock_or_now(at)),
            )
        } else {
            (
                "stale",
                off.iter()
                    .map(|r| {
                        format!("{} {}: {}", s(&r["label"]), s(&r["state"]), s(&r["detail"]))
                            .trim_matches(js_whitespace)
                            .to_owned()
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        }
    };
    out["state"] = json!(state);
    out["detail"] = json!(detail);
    Some(out)
}
pub(super) fn usage_source(i: &Inputs) -> Option<V> {
    let usage = &i.data["usage"];
    if usage.is_null() {
        return Some(
            json!({"id":"usage","label":"Account usage","state":"missing","asOf":null,"ageSeconds":null,"staleAfterSeconds":null,"detail":"Waiting for the dashboard’s first account sample."}),
        );
    }
    let at = i.at(&usage["sampledAt"])?;
    let age = i.age(Some(at));
    let stale = number(&age)? > number(&usage["staleAfterSeconds"])?;
    Some(
        json!({"id":"usage","label":"Account usage","state":if stale{"stale"}else{"ok"},"asOf":i.iso(Some(at)),"ageSeconds":age,"staleAfterSeconds":usage["staleAfterSeconds"],"detail":if stale{format!("No account sample since {}.",i.clock(at))}else{format!("Sampled {}.",i.clock(at))}}),
    )
}
pub(super) fn usage(i: &Inputs, source: &V) -> Option<V> {
    let usage = &i.data["usage"];
    if usage.is_null() {
        return Some(json!({"source":"usage","warnings":[],"accounts":[]}));
    }
    let mut accounts = Vec::new();
    let mut warnings: Vec<(f64, V)> = Vec::new();
    for account in usage["accounts"].as_array()? {
        if account["hidden"] == true {
            continue;
        }
        for w in account["windows"].as_array()? {
            if !["rate_limit", "balance"].contains(&s(&w["kind"])) {
                continue;
            }
            let reset = i.at(&w["resetAt"]);
            let sampled = i.at(w
                .get("sampledAt")
                .filter(|v| !v.is_null())
                .unwrap_or(&account["sampledAt"]));
            let reset_passed =
                w["resetPassed"] == true || reset.is_some_and(|r| i.due(r) && sampled.is_none_or(|t| t < r));
            let reading_at = sampled.or_else(|| i.at(&usage["sampledAt"]));
            let reading_stale = number(&i.age(reading_at))
                .zip(number(&usage["staleAfterSeconds"]))
                .is_some_and(|(age, limit)| age > limit);
            let stale = source["state"] != "ok"
                || account["status"] != "ok"
                || w["cached"] == true
                || reset_passed
                || reading_stale;
            let at_limit = number(&w["usedPercent"]).is_some_and(|n| n >= 100.0) && !reset_passed && !stale;
            let burn = if reset_passed { &V::Null } else { &w["burn"] };
            let full = i.at(&burn["projectedFullAt"]);
            accounts.push(json!({"id":account["id"],"provider":account["provider"],"label":account["label"],"host":account["host"],"active":account["active"],"window":w["label"],"kind":w["kind"],"usedPct":round1(&w["usedPercent"]),"remaining":w["remaining"],"limit":w["limit"],"unit":w["unit"],"resetsAt":w["resetAt"],"projectedFullAt":burn["projectedFullAt"],"runsOutBeforeReset":burn["runsOutBeforeReset"],"atLimit":at_limit,"stale":stale,"resetPassed":reset_passed}));
            if stale {
                continue;
            }
            let who = format!(
                "{} {} ({})",
                s(&account["provider"]),
                s(&account["label"]),
                s(&w["label"])
            );
            if at_limit {
                let text = reset
                    .map(|r| format!("{who}: at its limit until its {} {} reset", i.day(r), i.clock(r)))
                    .unwrap_or_else(|| format!("{who}: at its limit"));
                warnings.push((0.0, json!({"tone":"bad","text":text})));
                continue;
            }
            if burn["runsOutBeforeReset"] == true && full.is_some_and(|f| i.ahead(f)) {
                let full = full?;
                let text = reset
                    .map(|r| format!("{who}: full in {}, before its {} reset", i.span_until(full), i.day(r)))
                    .unwrap_or_else(|| format!("{who}: full in {}", i.span_until(full)));
                // Every rank moves by the same `now`, so the order does not. `ahead` and `span_until`
                // are the decisions that change the bytes.
                warnings.push(((full - i.now) as f64, json!({"tone":"warn","text":text})));
            }
        }
    }
    accounts.sort_by(|a, b| {
        i.ctx
            .compare(s(&a["provider"]), s(&b["provider"]))
            .then_with(|| i.ctx.compare(s(&a["label"]), s(&b["label"])))
            .then_with(|| i.ctx.compare(s(&a["window"]), s(&b["window"])))
    });
    warnings.sort_by(|a, b| a.0.total_cmp(&b.0));
    Some(
        json!({"source":"usage","warnings":warnings.into_iter().map(|(_,w)|w).collect::<Vec<_>>(),"accounts":accounts}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn inputs(usage: V) -> Inputs<'static> {
        let facts = serde_json::from_value(json!({"dates": {}, "epochs": {}, "order": {}})).unwrap();
        Inputs {
            native_reports: None,
            scan_stamps: [None, None],
            data: Data::Whole(json!({ "usage": usage })),
            ctx: Context::new(facts),
            now: 1_700_000_000_000,
            owner_cards: std::cell::OnceCell::new(),
            trace: RefCell::new(trace::Recorder::default()),
        }
    }

    fn account(window: V) -> V {
        json!({
            "id": "acct-1",
            "provider": "claude",
            "label": "Work",
            "host": "vm",
            "active": true,
            "status": "ok",
            "sampledAt": null,
            "windows": [window]
        })
    }

    fn usage_of(window: V) -> V {
        let i = inputs(json!({
            "sampledAt": 1_700_000_000_000i64,
            "staleAfterSeconds": 180,
            "accounts": [account(window)]
        }));
        usage(&i, &json!({"state": "ok"})).expect("usage composes")
    }

    #[test]
    fn a_cached_window_with_no_reading_time_is_not_at_limit() {
        let view = usage_of(json!({
            "kind": "rate_limit",
            "label": "session",
            "usedPercent": 100,
            "cached": true,
            "sampledAt": null
        }));
        let row = &view["accounts"][0];
        assert_eq!(
            row["atLimit"], false,
            "cached 100% with no sampledAt is not an at-limit claim"
        );
        assert_eq!(row["stale"], true, "the cached window stays visible as stale");
    }

    #[test]
    fn a_fresh_full_window_stays_at_limit() {
        let view = usage_of(json!({
            "kind": "rate_limit",
            "label": "session",
            "usedPercent": 100,
            "cached": false,
            "sampledAt": 1_700_000_000_000i64
        }));
        let row = &view["accounts"][0];
        assert_eq!(row["atLimit"], true);
        assert_eq!(row["stale"], false);
    }

    #[test]
    fn a_window_older_than_the_stale_limit_is_not_at_limit_inside_a_current_feed() {
        const NOW: i64 = 1_700_000_000_000;
        let mut acc = account(json!({
            "kind": "rate_limit",
            "label": "session",
            "usedPercent": 100,
            "cached": false,
            "sampledAt": NOW - 310_000,
            "resetAt": NOW + 3_600_000,
            "resetPassed": false
        }));
        acc["sampledAt"] = json!(NOW - 250_000);
        let i = inputs(json!({
            "sampledAt": NOW - 250_000,
            "staleAfterSeconds": 300,
            "accounts": [acc]
        }));
        let view = usage(&i, &json!({"state": "ok"})).expect("usage composes");
        let row = &view["accounts"][0];
        assert_eq!(row["usedPct"], 100.0, "the measured 100% stays as history");
        assert_eq!(row["stale"], true, "a 310s-old window past a 300s limit is stale");
        assert_eq!(row["atLimit"], false, "a stale window is not an at-limit claim");
        assert_eq!(view["warnings"], json!([]));
    }
}
