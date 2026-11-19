use super::*;
fn closing(e: &V) -> bool {
    ["answer", "note", "done"].contains(&s(&e["answer"]["kind"]))
}
pub(super) fn is_open(row: &V, answers: &[V]) -> bool {
    let doc = &row["doc"];
    doc["state"] == "open"
        && !answers.iter().filter(|e| closing(e)).any(|e| {
            e["answer"]["cardId"] == doc["id"]
                && e["answer"]["cardSeq"] == doc["seq"]
                && e["answer"]["cardSha256"] == row["sha256"]
        })
}
fn delivery(i: &Inputs, entry: &V) -> Option<V> {
    let share = &entry["share"];
    let bell = &entry["doorbell"];
    let sends = array(&i.data["sends"]);
    let summary = sends
        .iter()
        .rev()
        .find(|s| !entry["sendId"].is_null() && s["sendId"] == entry["sendId"])
        .map(|s| s["summary"].clone())
        .unwrap_or(V::Null);
    let warning = if summary.is_null() || summary["state"] == "done" {
        V::Null
    } else if summary["state"] == "skipped" {
        summary["detail"].clone()
    } else if summary["state"] == "pending" || summary["attempts"] == 0 {
        json!("The signed send summary is not on the share yet.")
    } else {
        let n = summary["attempts"].as_u64()?;
        json!(if n >= 20 {
            format!("The signed send summary could not be written to the share after {n} attempts; no more retries.")
        } else {
            format!("The signed send summary could not be written to the share yet (attempt {n} of 20); retrying.")
        })
    };
    let attempt = |step: &V| -> Option<String> {
        let n = step["attempts"].as_u64()?;
        Some(if n > 0 {
            format!(" (attempt {n} of 20)")
        } else {
            String::new()
        })
    };
    let (state, at, detail) = if entry["sharedOnly"] == true {
        ("published", V::Null, json!("The signed answer is on the share."))
    } else if share["state"] == "skipped" {
        ("failed", share["at"].clone(), share["detail"].clone())
    } else if share["state"] == "failed" {
        let n = share["attempts"].as_u64()?;
        (
            "failed",
            share["at"].clone(),
            json!(if n >= 20 {
                format!("The signed copy could not be written to the share after {n} attempts; no more retries.")
            } else if share["detail"] == "The reports share is not available." {
                format!(
                    "The reports share is not available, so the signed copy is not written yet{}; retrying.",
                    attempt(share)?
                )
            } else {
                format!(
                    "The signed copy could not be written to the share yet{}; retrying.",
                    attempt(share)?
                )
            }),
        )
    } else if bell["state"] == "failed" {
        let n = bell["attempts"].as_u64()?;
        (
            "failed",
            bell["at"].clone(),
            json!(if n >= 20 {
                format!(
                    "The asker could not be rung after {n} attempts; no more retries. The signed answer is on the share."
                )
            } else {
                format!(
                    "The asker could not be rung yet{}; retrying. The signed answer is on the share.",
                    attempt(bell)?
                )
            }),
        )
    } else if share["state"] == "done"
        && bell["state"] == "skipped"
        && bell["detail"] == "No mail name; the agent reads its answers file."
    {
        ("rang", bell["at"].clone(), bell["detail"].clone())
    } else if bell["state"] == "skipped" {
        ("failed", bell["at"].clone(), bell["detail"].clone())
    } else if share["state"] == "done" && bell["state"] == "done" {
        ("rang", bell["at"].clone(), bell["detail"].clone())
    } else {
        ("pending", V::Null, json!("Delivering."))
    };
    Some(
        json!({"share":share,"doorbell":bell,"summary":summary,"summaryWarning":warning,"state":state,"at":at,"detail":detail}),
    )
}
fn encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
fn url(writer: &str, path: &str) -> String {
    format!(
        "/api/status/files/{}/{}",
        encode(writer),
        path.split('/').map(encode).collect::<Vec<_>>().join("/")
    )
}
fn media(writer: &str, item: &V) -> Option<V> {
    let mut out = item.clone();
    out.as_object()?;
    out["url"] = json!(url(writer, s(&item["path"])));
    out["posterUrl"] = if s(&item["poster"]).is_empty() {
        V::Null
    } else {
        json!(url(writer, s(&item["poster"])))
    };
    Some(out)
}
fn sections(writer: &str, items: &V) -> Option<Vec<V>> {
    array(items)
        .iter()
        .map(|item| {
            let mut v = item.clone();
            if item["type"] == "media" && item.get("unknown").is_none() {
                v["items"] = json!(
                    array(&item["items"])
                        .iter()
                        .map(|m| media(writer, m))
                        .collect::<Option<Vec<_>>>()?
                );
            }
            Some(v)
        })
        .collect()
}
fn card(i: &Inputs, doc: &V, hash: &V, problem: V, notes: Vec<V>) -> Option<V> {
    let anchored = doc["kind"] == "judgement" && !doc["recommendation"].is_null();
    let threads = rows(&i.data["reports"]["threads"]);
    let thread = threads.iter().find(|r| r["writer"] == doc["writer"]);
    let asker = if let Some(row) = thread {
        if i.stale_thread(row)? {
            json!("gone")
        } else {
            row["doc"]["state"].clone()
        }
    } else {
        json!("unknown")
    };
    let writer = s(&doc["writer"]);
    let mut options = Vec::new();
    for o in array(&doc["options"]) {
        options.push(json!({"key":o["key"],"label":o["label"],"gets":o["gets"],"costs":o["costs"],"media":array(&o["media"]).iter().map(|m|media(writer,m)).collect::<Option<Vec<_>>>()?,"sections":sections(writer,&o["sections"])?,"stance":o["stance"]}));
    }
    let fallback_label = json!(s(&doc["id"]).split_once("--").map(|(_, s)| s).unwrap_or(s(&doc["id"])));
    Some(
        json!({"id":doc["id"],"media":doc.get("media").cloned().unwrap_or(json!([])),"label":doc.get("label").filter(|v|!v.is_null()).unwrap_or(&fallback_label),"writer":doc["writer"],"seq":doc["seq"],"project":doc["project"],"stream":doc["stream"],"askedBy":doc["askedBy"],"title":doc["title"],"now":doc["now"],"nowAsOf":doc["nowAsOf"],"oldAnswer":doc["oldAnswer"],"whatChanged":doc["whatChanged"],"options":options,"recommendation":doc["recommendation"],"multiple":doc["multiple"]==true,"urgency":doc["urgency"],"blocks":doc["blocks"],"sections":sections(writer,&doc["sections"])?,"batch":doc["batch"],"evidence":doc.get("evidence").cloned().unwrap_or(json!([])),"replies":doc.get("replies").cloned().unwrap_or(json!([])),"kind":doc.get("kind").cloned().unwrap_or(json!("question")),"neededBy":doc["neededBy"],"exception":doc["exception"],"reversible":doc["reversible"],"verdict":doc["verdict"],"group":doc["group"],"agreement":doc["agreement"],"doubt":doc["doubt"],"appliesTo":doc["appliesTo"],"changesCanon":doc.get("changesCanon").cloned().unwrap_or(json!([])),"related":doc.get("related").cloned().unwrap_or(json!([])),"askedAt":doc["askedAt"],"ageSeconds":number(&i.age(i.at(&doc["askedAt"]))).unwrap_or(0.0),"askerState":asker,"cardSha256":hash,"notes":notes,"problem":if !problem.is_null(){problem.clone()}else if anchored{json!("This judgement card carries a recommendation, so it cannot be answered: a judgement must not anchor the answer.")}else{V::Null},"answerable":problem.is_null()&&!anchored&&doc["kind"]!="sample"}),
    )
}
fn quiet_notice(i: &Inputs, doc: &V) -> Option<String> {
    const DAY_MS: i64 = 86_400_000;
    const EST_MS: i64 = 5 * 3_600_000;
    let asked = i.at(&doc["askedAt"])?;
    if !i.after(asked, 3 * DAY_MS) {
        return None;
    }
    let thread = rows(&i.data["reports"]["threads"])
        .iter()
        .find(|row| row["writer"] == doc["writer"] && row["problem"].is_null())?;
    let last = i.seen(thread)?;
    let ttl = thread["doc"]["ttlSeconds"].as_i64().unwrap_or(86_400);
    if !i.after(last, ttl * 1000) {
        return None;
    }
    let date = aac_num::ecma::to_iso_string(last - EST_MS)?;
    Some(format!(
        "asked {} days ago; thread quiet since {}",
        (i.now - asked) / DAY_MS,
        &date[..10]
    ))
}

pub(super) fn compose(i: &Inputs) -> Option<V> {
    let answers = i.data["answers"].as_array()?;
    let mut open = Vec::new();
    for row in i.data["reports"]["cards"].as_array()? {
        let doc = &row["doc"];
        if !is_open(row, answers) {
            continue;
        }
        let notes=answers.iter().filter(|e|!closing(e)&&e["answer"]["cardId"]==doc["id"]).map(|e|Some(json!({"kind":if e["answer"]["kind"]=="object"{"object"}else{"note"},"note":e["answer"]["note"],"at":e["answer"]["answeredAt"],"delivery":delivery(i,e)?}))).collect::<Option<Vec<_>>>()?;
        let highest = answers
            .iter()
            .filter(|e| e["answer"]["cardId"] == doc["id"])
            .filter_map(|e| e["answer"]["cardSeq"].as_u64())
            .max()
            .unwrap_or(0);
        let problem = if !row["problem"].is_null() {
            row["problem"]["reason"].clone()
        } else if doc["seq"].as_u64()? < highest {
            json!("its seq is lower than one already read, so this copy is ignored")
        } else {
            V::Null
        };
        let mut c = card(i, doc, &row["sha256"], problem, notes)?;
        if let Some(notice) = quiet_notice(i, doc) {
            c["quietNotice"] = json!(notice);
        }
        if !s(&i.data["answerHistoryProblem"]).is_empty() {
            c["answerable"] = json!(false);
            c["problem"] = i.data["answerHistoryProblem"].clone();
        }
        open.push(c);
    }
    let urgency = |c: &V| {
        ["now", "today", "this-week", "whenever"]
            .iter()
            .position(|s| c["urgency"] == *s)
            .map(|n| n as i32)
            .unwrap_or(-1)
    };
    open.sort_by(|a, b| {
        urgency(a)
            .cmp(&urgency(b))
            .then_with(|| {
                number(&b["ageSeconds"])
                    .partial_cmp(&number(&a["ageSeconds"]))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| i.ctx.compare(s(&a["id"]), s(&b["id"])))
    });
    let mut batches: Vec<V> = Vec::new();
    for c in &open {
        if s(&c["batch"]).is_empty() {
            continue;
        }
        let at = batches.iter().position(|b| b["id"] == c["batch"]).unwrap_or_else(|| {
            batches.push(json!({"id":c["batch"],"label":c["batch"],"cardIds":[]}));
            batches.len() - 1
        });
        batches[at]["cardIds"].as_array_mut()?.push(c["id"].clone());
    }
    let mut history = Vec::new();
    for e in answers.iter().filter(|e| closing(e)).rev().take(50) {
        let a = &e["answer"];
        history.push(json!({"card":card(i,&e["card"],&a["cardSha256"],V::Null,vec![])?,"answer":{"kind":a["kind"],"choice":a["choice"],"note":a["note"],"via":a["via"],"answeredAt":a["answeredAt"],"device":a["device"],"delivery":delivery(i,e)?}}));
    }
    let reports = &i.data["reports"];
    let complete = s(&i.data["answerHistoryProblem"]).is_empty()
        && reports["state"] == "ok"
        && reports["complete"] == true
        && !reports["scannedAt"].is_null()
        && number(&i.age(i.at(&reports["scannedAt"]))).unwrap_or(0.0) <= 60.0;
    Some(
        json!({"source":"agent-reports","open":open,"answerHistoryProblem":i.data["answerHistoryProblem"],"answered":if !history.is_empty() || s(&i.data["answerHistoryProblem"]).is_empty(){json!(history)}else{V::Null},"batches":batches,"complete":complete}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_answer_closes_only_its_exact_version() {
        let mut doc = json!({
            "id": "vm.lead--SR1", "writer": "vm.lead", "seq": 2308, "state": "open",
            "kind": "question", "urgency": "today"
        });
        let asked = json!({"doc": doc, "sha256": "fixture-hash"});
        let entry = json!({
            "sharedOnly": true, "card": doc,
            "answer": {
                "kind": "answer", "cardId": "vm.lead--SR1", "cardSeq": 2308,
                "cardSha256": "fixture-hash", "choice": ["A"],
                "answeredAt": "2026-11-18T18:50:41.881Z"
            },
            "share": {"state": "done"}, "doorbell": {"state": "skipped"}
        });
        let facts = serde_json::from_value(json!({"dates":{}, "epochs":{}, "order":{}})).unwrap();
        let mut i = Inputs {
            data: Data::Whole(json!({
                "answers": [entry], "reports": {"cards": [asked], "threads": []}
            })),
            ctx: Context::new(facts),
            now: 0,
            owner_cards: std::cell::OnceCell::new(),
            trace: RefCell::new(trace::Recorder::default()),
        };
        let view = compose(&i).unwrap();
        assert_eq!(view["open"], json!([]));
        let answer = &view["answered"][0]["answer"];
        assert_eq!(answer["choice"], json!(["A"]));
        assert_eq!(answer["answeredAt"], "2026-11-18T18:50:41.881Z");
        assert_eq!(answer["delivery"]["state"], "published");

        doc["seq"] = json!(2309);
        doc["whatChanged"] = json!("New information.");
        let Data::Whole(ref mut data) = i.data else {
            unreachable!()
        };
        // Either a re-ask or different bytes at the same seq remain open.
        data["reports"]["cards"][0] = json!({"doc": doc, "sha256": "fixture-hash"});
        assert_eq!(compose(&i).unwrap()["open"][0]["seq"], 2309);
        let Data::Whole(ref mut data) = i.data else {
            unreachable!()
        };
        data["reports"]["cards"][0] = json!({"doc": asked["doc"], "sha256": "other-hash"});
        assert_eq!(compose(&i).unwrap()["open"][0]["seq"], 2308);
    }

    #[test]
    fn old_cards_get_a_notice_only_after_the_card_and_heartbeat_cutoffs() {
        const DAY: i64 = 86_400_000;
        let now = aac_num::ecma::parse_instant("2026-11-18T20:00:00Z").unwrap();
        let asked = now - 4 * DAY;
        let fixture = |last| Inputs {
            data: Data::Whole(json!({"reports":{"threads":[{
                "writer":"vm.fixture", "writtenAtMs":last, "firstSeenAt":now,
                "doc":{"state":"waiting","ttlSeconds":null}
            }]}})),
            ctx: Context::new(
                serde_json::from_value(json!({
                    "dates":{}, "epochs":{"2026-11-14T20:00:00Z":asked}, "order":{}
                }))
                .unwrap(),
            ),
            now,
            owner_cards: Default::default(),
            trace: Default::default(),
        };
        let card = json!({"writer":"vm.fixture","askedAt":"2026-11-14T20:00:00Z"});
        assert_eq!(
            quiet_notice(&fixture(now - 2 * DAY), &card).as_deref(),
            Some("asked 4 days ago; thread quiet since 2026-11-16")
        );
        assert_eq!(quiet_notice(&fixture(now - DAY), &card), None);
        assert_eq!(
            quiet_notice(
                &fixture(now - 2 * DAY),
                &json!({"writer":"vm.fixture","askedAt":now - 3 * DAY})
            ),
            None
        );
    }
}
