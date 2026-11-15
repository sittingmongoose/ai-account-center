use super::*;
fn listed(names: &[String]) -> String {
    let mut out = names.iter().take(4).cloned().collect::<Vec<_>>().join(", ");
    if names.len() > 4 {
        out.push_str(&format!(" and {} more", names.len() - 4));
    }
    out
}
fn one(n: usize, singular: &str, many: &str) -> String {
    if n == 1 { singular.into() } else { many.into() }
}
fn owner(update: &V) -> String {
    let b = &update["board"];
    if b["shared"] == true {
        format!(
            "the {} board kept by {}",
            b["streamLabel"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or(s(&b["stream"])),
            s(&b["writer"])
        )
    } else {
        s(&update["title"]).into()
    }
}
pub(super) fn compose(view: &V) -> Option<V> {
    let counts = &view["headline"]["counts"];
    let n = counts["questions"].as_u64()? as usize;
    let decisions = counts["decisions"].as_u64()? as usize;
    let sources = view["sources"].as_array()?;
    let reports = sources.iter().find(|s| s["id"] == "agent-reports")?;
    let unread = ["missing", "error"].contains(&s(&reports["state"]));
    let mut items = vec![if n == 0 && unread {
        json!({"key":"questions","section":"questions","value":"–","label":"questions for you","short":"questions","tone":"quiet","detail":format!("Questions for you are unknown: {}",sentence(s(&reports["detail"])))})
    } else {
        json!({"key":"questions","section":"questions","value":n.to_string(),"label":one(n,"question for you","questions for you"),"short":one(n,"question","questions"),"tone":if n>0{"warn"}else{"quiet"},"detail":if n>0{format!("{n} {} for you.",one(n,"question","questions"))}else{"No questions for you.".into()}})
    }];
    if decisions > 0 {
        items.push(json!({"key":"decisions","value":decisions.to_string(),"label":one(decisions,"decision to look at","decisions to look at"),"short":one(decisions,"decision","decisions"),"tone":"warn","section":"questions","detail":format!("{decisions} {} to look at.",one(decisions,"decision","decisions"))}));
    }
    let mut loops: Vec<V> = Vec::new();
    let mut loop_keys = Vec::new();
    let mut waiting = Vec::new();
    let pinned = &view["pinned"];
    if let Some(updates) = pinned["updates"].as_array() {
        for (index, u) in updates.iter().enumerate() {
            if u["blocked"]["waitingOnYou"] == true {
                waiting.push(u.clone());
            }
            if u["looping"]["answer"] != "yes" {
                continue;
            }
            let key = if u["board"].is_object() && !s(&u["looping"]["task"]).is_empty() {
                format!("board\n{}\n{}", s(&u["board"]["writer"]), s(&u["looping"]["task"]))
            } else {
                format!("pin\n{index}")
            };
            if let Some(at) = loop_keys.iter().position(|k| k == &key) {
                if loops[at]["board"]["shared"] == true && u["board"]["shared"] != true {
                    loops[at] = u.clone();
                }
            } else {
                loop_keys.push(key);
                loops.push(u.clone());
            }
        }
    } else {
        let why = if pinned.is_null() {
            "this server doesn't send them yet"
        } else {
            s(&pinned["unavailable"])
        };
        items.push(json!({"key":"pinned","value":"–","label":"pinned threads","short":"pinned","tone":"quiet","section":"pinned","detail":format!("Pinned threads are unavailable: {}",sentence(why))}));
    }
    if !loops.is_empty() {
        let n = loops.len();
        let names = loops
            .iter()
            .map(|u| {
                if !s(&u["looping"]["task"]).is_empty() {
                    format!("{} (task {})", owner(u), s(&u["looping"]["task"]))
                } else {
                    owner(u)
                }
            })
            .collect::<Vec<_>>();
        items.push(json!({"key":"looping","value":n.to_string(),"label":one(n,"thread looping","threads looping"),"short":"looping","tone":"bad","section":"pinned","detail":format!("{n} pinned {} looping: {}.",one(n,"thread is","threads are"),listed(&names))}));
    }
    if !waiting.is_empty() {
        let n = waiting.len();
        items.push(json!({"key":"waiting","value":n.to_string(),"label":"waiting on you","short":one(n,"waits on you","wait on you"),"tone":"warn","section":"pinned","detail":format!("{n} pinned {} on you: {}.",one(n,"thread waits","threads wait"),listed(&waiting.iter().map(|u|s(&u["title"]).to_owned()).collect::<Vec<_>>()))}));
    }
    let blocked = counts["blockedItems"].as_u64()? as usize;
    if blocked > 0 {
        let mut names = Vec::new();
        for p in view["projects"].as_array()? {
            for st in p["streams"].as_array()? {
                for item in st["items"].as_array()? {
                    if item["state"] == "blocked" {
                        names.push(
                            format!("{} {}", s(&item["id"]), s(&item["title"]))
                                .trim_matches(js_whitespace)
                                .to_owned(),
                        );
                    }
                }
            }
        }
        items.push(json!({"key":"blocked","value":blocked.to_string(),"label":"blocked","short":"blocked","tone":"warn","section":null,"detail":if !names.is_empty(){format!("Blocked: {}.",listed(&names))}else{format!("{blocked} {} blocked.",one(blocked,"item is","items are"))}}));
    }
    let over = if sources.iter().any(|s| s["id"] == "quota-watch" && s["state"] == "ok") {
        view["spend"]["overLimitBudgets"].as_u64()? as usize
    } else {
        0
    };
    if over > 0 {
        items.push(json!({"key":"budgets","value":over.to_string(),"label":"over budget (estimate)","short":"over budget","tone":"bad","section":"spend","detail":format!("{over} {} at or over {} limit (estimate).",one(over,"budget is","budgets are"),one(over,"its","their"))}));
    }
    let mut total = empty_counts();
    for p in view["projects"].as_array()? {
        add_counts(&mut total, &p["counts"])?;
    }
    let done = completion(&total)?;
    if done.is_object() {
        let n = done["done"].as_u64()?;
        let of = done["of"].as_u64()?;
        items.push(json!({"key":"done","value":format!("{n}/{of}"),"label":"items done","short":"done","tone":null,"section":null,"detail":format!("{n} of {of} items landed or done (dropped items are not counted).") }));
    }
    let behind: Vec<_> = sources
        .iter()
        .filter(|s| s["state"] != "ok" && s["state"] != "unconfigured")
        .collect();
    if !behind.is_empty() {
        let n = behind.len();
        let parts = behind
            .iter()
            .map(|s| {
                format!(
                    "{}: {}",
                    super::s(&s["label"]),
                    trim_period(super::s(&s["detail"]).trim_matches(js_whitespace))
                )
            })
            .collect::<Vec<_>>();
        items.push(json!({"key":"sources","value":n.to_string(),"label":one(n,"source not current","sources not current"),"short":"not current","tone":"warn","section":null,"detail":format!("Not current: {}.",parts.join("; "))}));
    }
    let mut worst: Option<&V> = None;
    for u in &loops {
        if worst.is_none_or(|w| {
            number(&u["looping"]["rounds"]).unwrap_or(0.0) > number(&w["looping"]["rounds"]).unwrap_or(0.0)
        }) {
            worst = Some(u);
        }
    }
    let worst = if let Some(u) = worst {
        json!({"state":"looping","word":"Looping","tone":"bad","threadId":u["id"],"title":owner(u),"task":u["looping"]["task"],"detail":sentence(u["looping"]["text"].as_str().filter(|s|!s.is_empty()).unwrap_or("Looping."))})
    } else if let Some(u) = waiting.first() {
        json!({"state":"waiting","word":"Waiting on you","tone":"warn","threadId":u["id"],"title":u["title"],"task":null,"detail":sentence(u["blocked"]["text"].as_str().filter(|s|!s.is_empty()).unwrap_or("Waiting on you."))})
    } else {
        V::Null
    };
    Some(json!({"items":items,"worst":worst}))
}
