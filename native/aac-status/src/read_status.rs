//! Status view composition over collected snapshots. Collectors and guarded write protocols keep
//! their owners; every value and sentence in the read response is composed here.
use crate::read_context::{
    Context, Envelope, age, age_of, array, js_whitespace, number, plural, rows, s,
};
use aac_num::{ecma, view};
use serde_json::{Value as V, json};
use std::collections::{HashMap, HashSet};
#[path = "read_status_cards.rs"]
mod cards;
#[path = "read_status_panels.rs"]
mod panels;
#[path = "read_status_spend.rs"]
mod spend;
#[path = "read_status_strip.rs"]
mod strip;
/// The composer's input object: the presentation envelope's `data` (owned), or R2's fed generations, each a JSON
/// object of some of its top-level members, borrowed from the generations a request names (no copy per request).
/// Indexing by a top-level key reads like `Value` indexing: an absent member is `null`.
pub(crate) enum Data<'a> {
    Whole(V),
    Parts(Vec<(&'a str, &'a V)>),
}
static NULL: V = V::Null;
impl Data<'_> {
    pub(crate) fn get(&self, key: &str) -> Option<&V> {
        match self {
            Data::Whole(v) => v.get(key),
            Data::Parts(parts) => parts.iter().find(|(k, _)| *k == key).map(|(_, v)| *v),
        }
    }
}
impl std::ops::Index<&str> for Data<'_> {
    type Output = V;
    fn index(&self, key: &str) -> &V {
        self.get(key).unwrap_or(&NULL)
    }
}
struct Inputs<'a> {
    data: Data<'a>,
    ctx: Context,
    now: i64,
    /// The writers of open question, judgement and action cards, found once per view (`stale_thread` asks for
    /// every thread and every card's asker).
    owner_cards: std::cell::OnceCell<OwnerCards>,
}
/// Writer values, compared as `Value`s compare: strings by text, anything else by value.
#[derive(Default)]
struct OwnerCards {
    strings: HashSet<String>,
    other: Vec<V>,
}
impl OwnerCards {
    fn has(&self, writer: &V) -> bool {
        match writer.as_str() {
            Some(text) => self.strings.contains(text),
            None => self.other.contains(writer),
        }
    }
}
impl Inputs<'_> {
    fn at(&self, v: &V) -> Option<i64> {
        self.ctx.epoch(v)
    }
    fn seen(&self, row: &V) -> Option<i64> {
        let seen = self.at(&row["firstSeenAt"])?;
        Some(self.at(&row["writtenAtMs"]).map(|t| t.min(seen)).unwrap_or(seen))
    }
    fn clock(&self, at: i64) -> String {
        self.ctx.clock(at)
    }
    fn iso(&self, at: Option<i64>) -> V {
        self.ctx.iso(at)
    }
    fn age(&self, at: Option<i64>) -> V {
        age(self.now, at)
    }
    fn stale_thread(&self, row: &V) -> Option<bool> {
        if !row["problem"].is_null() {
            return Some(true);
        }
        let owner = row["doc"]["blockedOn"]["kind"] == "owner"
            || self
                .owner_cards
                .get_or_init(|| {
                    let mut found = OwnerCards::default();
                    for c in rows(&self.data["reports"]["cards"]) {
                        if c["doc"]["state"] == "open"
                            && ["question", "judgement", "action"].contains(&s(&c["doc"]["kind"]))
                        {
                            match c["writer"].as_str() {
                                Some(text) => {
                                    found.strings.insert(text.to_owned());
                                }
                                None => found.other.push(c["writer"].clone()),
                            }
                        }
                    }
                    found
                })
                .has(&row["writer"]);
        if owner {
            return Some(false);
        }
        let age = self.now - self.seen(row)?;
        if row["doc"]["ttlSeconds"].is_null() {
            Some(!["blocked", "waiting"].contains(&s(&row["doc"]["state"])) && age > 86_400_000)
        } else {
            Some(age > row["doc"]["ttlSeconds"].as_i64()? * 1000)
        }
    }
    fn board(&self, given: &V) -> V {
        if s(given).is_empty() {
            return V::Null;
        }
        let mtime = &self.data["reports"]["boards"][s(given)];
        // A board time is `fs.Stats.mtimeMs`, fractional on ext4, ZFS, NFS and SMB: age it as
        // status-view-words.ts `ageSeconds` does, in binary64, rather than as "no time".
        let age = match mtime.as_f64() {
            Some(ms) if !mtime.is_i64() && ms.is_finite() && ms.abs() <= 8.64e15 => age_of(self.now as f64 - ms),
            _ => self.age(self.at(mtime)),
        };
        json!({"path":given,"ageSeconds":age})
    }
}
fn sentence(text: &str) -> String {
    let t = text.trim_matches(js_whitespace);
    if t.ends_with(['.', '!', '?']) {
        t.to_owned()
    } else {
        format!("{t}.")
    }
}
fn trim_period(text: &str) -> &str {
    text.strip_suffix('.').unwrap_or(text)
}
fn round1(v: &V) -> V {
    number(v)
        .map(|n| json!(ecma::math_round(n * 10.0) / 10.0))
        .unwrap_or(V::Null)
}
fn gb(v: &V) -> V {
    number(v)
        .map(|n| json!(ecma::math_round((n / 1_073_741_824.0) * 10.0) / 10.0))
        .unwrap_or(V::Null)
}
fn span(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "now".into();
    }
    let d = (seconds / 86400.0).floor();
    let h = ((seconds % 86400.0) / 3600.0).floor();
    let m = ((seconds % 3600.0) / 60.0).floor();
    if d > 0.0 {
        format!("{d}d {h}h")
    } else if h > 0.0 {
        format!("{h}h {m}m")
    } else if m > 0.0 {
        format!("{m}m")
    } else {
        format!("{}s", seconds.floor())
    }
}
/// status-view-panels.ts `partProblemText`: `${item}: ${reason}`. TS writes any `item` through `String()`
/// ("undefined" when absent) and throws on a reason that is not a string, so anything but two strings declines
/// (R2 fuzz: a problem without an item was served as ": ..." where TS writes "undefined: ...").
fn part_problem(i: &Inputs, p: &V) -> Option<String> {
    Some(format!(
        "{}: {} (since {}).",
        p["item"].as_str()?,
        trim_period(p["reason"].as_str()?),
        i.clock(i.at(&p["since"])?)
    ))
}
fn source_ref(v: &V) -> V {
    json!({"id":v["id"],"label":v["label"],"state":v["state"],"asOf":v["asOf"],"ageSeconds":v["ageSeconds"],"detail":v["detail"]})
}
fn part_ref(i: &Inputs, source: &V, collected: &V, part: Option<&V>, absent: &str) -> Option<V> {
    if !collected["error"].is_null() || collected["asOf"].is_null() || source["state"] == "stale" {
        return Some(source_ref(source));
    }
    let mut out = json!({"id":source["id"],"label":source["label"]});
    let Some(part) = part else {
        out["state"] = json!("missing");
        out["asOf"] = V::Null;
        out["ageSeconds"] = V::Null;
        out["detail"] = json!(absent);
        return Some(out);
    };
    let read = i.at(&part["readAt"]);
    if !part["problem"].is_null() {
        out["state"] = json!(if source["state"] == "unreadable" {
            "unreadable"
        } else {
            "error"
        });
        out["asOf"] = i.iso(read);
        out["ageSeconds"] = i.age(read);
        let shown = read
            .map(|t| format!(" Showing its reading from {}.", i.clock(t)))
            .unwrap_or_else(|| " No reading yet.".into());
        out["detail"] = json!(format!("{}{shown}", part_problem(i, &part["problem"])?));
    } else {
        let at = read.or_else(|| i.at(&collected["asOf"]));
        out["state"] = json!("ok");
        out["asOf"] = i.iso(at);
        out["ageSeconds"] = i.age(at);
        out["detail"] = json!(format!("Read {}.", i.clock(at?)));
    }
    Some(out)
}
fn collected_source(i: &Inputs, id: &str, label: &str, c: &V, stale: i64, registry: bool) -> Option<V> {
    let at = i.at(&c["asOf"]);
    let mut out = json!({"id":id,"label":label,"asOf":i.iso(at),"ageSeconds":i.age(at),"staleAfterSeconds":stale});
    let (state, detail) = if c["error"].is_object() {
        let shown = at
            .map(|t| format!(" Showing the reading from {}.", i.clock(t)))
            .unwrap_or_default();
        (
            if c["missing"] == true { "missing" } else { "error" },
            format!(
                "{} (since {}).{shown}",
                trim_period(s(&c["error"]["reason"])),
                i.clock(i.at(&c["error"]["since"])?)
            ),
        )
    } else if at.is_none() {
        ("missing", "Waiting for the first reading.".into())
    } else if number(&out["ageSeconds"])? > stale as f64 {
        ("stale", format!("No new reading since {}.", i.clock(at?)))
    } else {
        let problems = array(&c["problems"]);
        if problems.is_empty() {
            ("ok", format!("Read {}.", i.clock(at?)))
        } else {
            let text = if registry {
                let since = problems.iter().filter_map(|p| i.at(&p["since"])).min()?;
                format!(
                    "{} unreadable since {}: {}{}. Their threads show the last good reading.",
                    plural(problems.len(), "registry file"),
                    i.clock(since),
                    problems
                        .iter()
                        .take(5)
                        .map(|p| format!("{} ({})", s(&p["item"]), s(&p["reason"])))
                        .collect::<Vec<_>>()
                        .join(", "),
                    if problems.len() > 5 {
                        format!(", and {} more", problems.len() - 5)
                    } else {
                        String::new()
                    }
                )
            } else {
                problems
                    .iter()
                    .map(|p| part_problem(i, p))
                    .collect::<Option<Vec<_>>>()?
                    .join(" ")
            };
            (
                if registry { "unreadable" } else { "error" },
                format!("{text} The rest read {}.", i.clock(at?)),
            )
        }
    };
    out["state"] = json!(state);
    out["detail"] = json!(detail);
    Some(out)
}
fn reports_source(i: &Inputs) -> Option<V> {
    let r = &i.data["reports"];
    let at = i.at(&r["scannedAt"]);
    let mut out = json!({"id":"agent-reports","label":"Agent reports","asOf":i.iso(at),"ageSeconds":i.age(at),"staleAfterSeconds":60});
    let (state, detail) = if at.is_none() {
        ("missing", "Waiting for the first scan.".into())
    } else if r["state"] == "missing" {
        ("missing", format!("No agent reports folder at {}.", s(&r["root"])))
    } else if r["state"] == "error" {
        ("error", "The agent reports folder could not be listed.".into())
    } else if number(&out["ageSeconds"])? > 60.0 {
        (
            "stale",
            format!("The agent reports folder has not answered since {}.", i.clock(at?)),
        )
    } else {
        let problems = array(&r["problems"]);
        let files: Vec<_> = problems
            .iter()
            .filter(|p| !s(&p["file"]).starts_with("limits/"))
            .collect();
        let limits: Vec<_> = problems
            .iter()
            .filter(|p| s(&p["file"]).starts_with("limits/"))
            .map(|p| s(&p["reason"]))
            .collect();
        let limits = if limits.is_empty() {
            String::new()
        } else {
            format!(" {}", limits.join(" "))
        };
        if !files.is_empty() {
            let since = files.iter().filter_map(|p| i.at(&p["since"])).min()?;
            let shown = files
                .iter()
                .take(5)
                .map(|p| {
                    format!(
                        "{} ({})",
                        s(&p["file"]).strip_prefix("writers/").unwrap_or(s(&p["file"])),
                        s(&p["reason"])
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let more = if files.len() > 5 {
                format!(", and {} more", files.len() - 5)
            } else {
                String::new()
            };
            (
                "unreadable",
                format!(
                    "{} unreadable since {}: {shown}{more}.{limits}",
                    plural(files.len(), "file"),
                    i.clock(since)
                ),
            )
        } else {
            (
                "ok",
                format!(
                    "{} and {} read {}.{limits}",
                    plural(rows(&r["threads"]).len(), "thread"),
                    plural(rows(&r["cards"]).len(), "card"),
                    i.clock(at?)
                ),
            )
        }
    };
    out["state"] = json!(state);
    out["detail"] = json!(detail);
    Some(out)
}
fn thread_name(row: &V) -> String {
    row["doc"]["agent"]["name"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| {
            s(&row["writer"])
                .split_once('.')
                .map(|(_, s)| s)
                .unwrap_or("")
                .to_owned()
        })
}
fn threads(i: &Inputs, registry: &V) -> Option<Vec<V>> {
    let mut rows = Vec::new();
    let mut reporting = HashSet::new();
    for row in crate::read_context::rows(&i.data["reports"]["threads"]) {
        let name = thread_name(row);
        reporting.insert(name.clone());
        reporting.insert(s(&row["writer"]).split_once('.')?.1.to_owned());
        let at = i.seen(&row)?;
        if i.now - at > 86_400_000 && row["doc"]["state"] != "blocked" {
            continue;
        }
        let ahead = (number(&row["writtenAtMs"]).unwrap_or(number(&row["firstSeenAt"])?)
            - number(&row["firstSeenAt"])?)
            / 1000.0;
        let d = &row["doc"];
        rows.push(json!({"name":name,"writer":row["writer"],"machine":s(&row["writer"]).split_once('.')?.0,"project":d["project"],"stream":d["stream"],"state":d["state"],"doing":d["doing"],"blockedOn":d["blockedOn"],"progress":d["progress"],"model":d["agent"]["model"],"effort":d["agent"]["effort"],"account":d["agent"]["account"],"source":"report","lastSeen":i.iso(Some(at)),"ageSeconds":i.age(Some(at)),"stale":i.stale_thread(&row)?,"clockAheadSeconds":if ahead>120.0{json!(ecma::math_round(ahead))}else{V::Null},"problem":row["problem"]["reason"],"registrySource":null}));
    }
    let hidden = crate::read_context::rows(&i.data["hiddenThreadNames"]);
    for thread in crate::read_context::rows(&i.data["registry"]["data"]) {
        if reporting.contains(s(&thread["name"])) || hidden.contains(&thread["name"]) {
            continue;
        }
        let Some(at) = i.at(&thread["lastSeen"]) else {
            continue;
        };
        if i.now - at > 86_400_000 {
            continue;
        }
        let file_problem = if thread["problem"].is_object() {
            let shown = i
                .at(&thread["readAt"])
                .map(|t| format!(" Showing its reading from {}.", i.clock(t)))
                .unwrap_or_default();
            json!(format!("{}{shown}", part_problem(i, &thread["problem"])?))
        } else {
            V::Null
        };
        rows.push(json!({"name":thread["name"],"writer":null,"machine":thread["machine"],"project":null,"stream":null,"state":null,"doing":thread["about"],"blockedOn":null,"progress":null,"model":null,"effort":null,"account":null,"source":"pm-mail","lastSeen":thread["lastSeen"],"ageSeconds":i.age(Some(at)),"stale":!file_problem.is_null()||i.now-at>1_800_000,"clockAheadSeconds":null,"problem":file_problem,"registrySource":part_ref(i,registry,&i.data["registry"],Some(&thread),"Not in the pm-mail registry.")?}));
    }
    let rank = |r: &V| {
        ["blocked", "waiting", "working", "paused", "idle", "done"]
            .iter()
            .position(|w| r["state"] == *w)
            .unwrap_or(9)
    };
    rows.sort_by(|a, b| {
        (a["source"] == "pm-mail")
            .cmp(&(b["source"] == "pm-mail"))
            .then_with(|| (a["stale"] == true).cmp(&(b["stale"] == true)))
            .then_with(|| rank(a).cmp(&rank(b)))
            .then_with(|| {
                number(&a["ageSeconds"])
                    .unwrap_or(f64::INFINITY)
                    .total_cmp(&number(&b["ageSeconds"]).unwrap_or(f64::INFINITY))
            })
            .then_with(|| i.ctx.compare(s(&a["name"]), s(&b["name"])))
    });
    Some(rows)
}
const STATES: [&str; 9] = [
    "todo", "working", "review", "fixing", "blocked", "ready", "landed", "done", "dropped",
];
fn empty_counts() -> V {
    let mut c = json!({"total":0});
    for state in STATES {
        c[state] = json!(0);
    }
    c
}
fn add_counts(into: &mut V, from: &V) -> Option<()> {
    for key in std::iter::once("total").chain(STATES) {
        into[key] = json!(into[key].as_u64()? + from[key].as_u64()?);
    }
    Some(())
}
fn completion(c: &V) -> Option<V> {
    let of = c["total"].as_u64()?.checked_sub(c["dropped"].as_u64()?)?;
    Some(if of > 0 {
        json!({"done":c["landed"].as_u64()?+c["done"].as_u64()?,"of":of})
    } else {
        V::Null
    })
}
fn projects(i: &Inputs) -> Option<Vec<V>> {
    let mut groups: Vec<(String, V, Vec<V>)> = Vec::new();
    let mut tasks: Vec<&V> = rows(&i.data["reports"]["tasks"]).iter().collect();
    tasks.sort_by(|a, b| {
        i.seen(b)
            .cmp(&i.seen(a))
            .then_with(|| i.ctx.compare(s(&a["writer"]), s(&b["writer"])))
    });
    for row in tasks {
        let d = &row["doc"];
        let id = s(&d["project"]);
        let at = groups.iter().position(|(s, _, _)| s == id).unwrap_or_else(|| {
            groups.push((id.to_owned(), V::Null, Vec::new()));
            groups.len() - 1
        });
        if groups[at].1.is_null() {
            groups[at].1 = d["projectLabel"].clone();
        }
        if groups[at].2.iter().any(|s| s["id"] == d["stream"]) {
            continue;
        }
        let mut counts = empty_counts();
        let mut items = array(&d["items"]);
        counts["total"] = json!(items.len());
        for item in &mut items {
            let state = s(&item["state"]).to_owned();
            if STATES.contains(&state.as_str()) {
                counts[&state] = json!(counts[&state].as_u64()? + 1);
            }
            item["ageSeconds"] = i.age(i.at(&item["updatedAt"]));
        }
        let seen = i.seen(&row)?;
        groups[at].2.push(json!({"id":d["stream"],"label":d.get("streamLabel").filter(|v|!v.is_null()).unwrap_or(&d["stream"]),"owner":d["owner"],"goal":d["goal"],"writer":row["writer"],"board":i.board(&d["board"]),"source":"tasks","asOf":i.iso(Some(seen)),"ageSeconds":i.age(Some(seen)),"stale":!row["problem"].is_null()||i.now-seen>86_400_000,"problem":row["problem"]["reason"],"counts":counts,"completion":completion(&counts)?,"items":items}));
    }
    let mut threads: Vec<&V> = rows(&i.data["reports"]["threads"]).iter().collect();
    threads.sort_by_key(|r| std::cmp::Reverse(i.seen(r)));
    for row in threads {
        let d = &row["doc"];
        if s(&d["project"]).is_empty() || s(&d["stream"]).is_empty() {
            continue;
        }
        let links = array(&d["links"]);
        let Some(link) = links.iter().find(|l| s(&l["label"]).to_lowercase().contains("board")) else {
            continue;
        };
        let id = s(&d["project"]);
        let at = groups.iter().position(|(s, _, _)| s == id).unwrap_or_else(|| {
            groups.push((id.to_owned(), V::Null, Vec::new()));
            groups.len() - 1
        });
        if groups[at].2.iter().any(|s| s["id"] == d["stream"]) {
            continue;
        }
        let seen = i.seen(&row)?;
        groups[at].2.push(json!({"id":d["stream"],"label":d["stream"],"owner":thread_name(&row),"goal":null,"writer":row["writer"],"board":i.board(&link["path"]),"source":"board-only","asOf":i.iso(Some(seen)),"ageSeconds":i.age(Some(seen)),"stale":i.stale_thread(&row)?,"problem":row["problem"]["reason"],"counts":null,"completion":null,"items":[]}));
    }
    let mut out = Vec::new();
    for (id, label, mut streams) in groups {
        streams.sort_by(|a, b| i.ctx.compare(s(&a["label"]), s(&b["label"])));
        let mut counts = empty_counts();
        for stream in &streams {
            if stream["counts"].is_object() {
                add_counts(&mut counts, &stream["counts"])?;
            }
        }
        out.push(json!({"id":id,"label":if label.is_null(){json!(id)}else{label},"streams":streams,"counts":counts,"completion":completion(&counts)?}));
    }
    out.sort_by(|a, b| i.ctx.compare(s(&a["label"]), s(&b["label"])));
    Some(out)
}
pub fn compose(envelope: Envelope) -> Option<V> {
    if crate::read_context::proto_named(&envelope.data) {
        return None;
    }
    let now = envelope.data["now"].as_i64()?;
    let i = Inputs {
        data: Data::Whole(envelope.data),
        ctx: Context::new(envelope.presentation),
        now,
        owner_cards: std::cell::OnceCell::new(),
    };
    let view = view(&i)?;
    i.ctx.valid().then_some(view)
}
/// Why a fed composition did not build: a fact it read is missing (a time's words, an epoch, a rank), or an input
/// it cannot prove (a shape, a number outside the proved domain).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unproven {
    Facts,
    Input,
}
/// R2: the same view from fed generations. `parts` are the generations' top-level members (each generation
/// already passed [`fed_part`]); `facts` the facts generation's presentation; `now` the request's clock and its
/// date facts. Byte-equal to [`compose`] of the envelope those parts and facts make.
pub fn compose_fed(
    parts: Vec<(&str, &V)>,
    facts: std::sync::Arc<crate::read_context::Presentation>,
    now: (i64, crate::read_context::Date),
) -> Result<V, Unproven> {
    let at = now.0;
    let i = Inputs {
        data: Data::Parts(parts),
        ctx: Context::shared(facts, Some(now)),
        now: at,
        owner_cards: std::cell::OnceCell::new(),
    };
    let view = view(&i);
    match view {
        Some(view) if i.ctx.valid() => Ok(view),
        _ if !i.ctx.valid() => Err(Unproven::Facts),
        _ => Err(Unproven::Input),
    }
}
/// R2: one fed generation's data as the composer reads it: a JSON object of top-level members, none of which holds
/// an `Object.prototype` member name (the envelope's own gate, applied once per generation). None declines it.
pub fn fed_part(data: &V) -> Option<()> {
    (data.is_object() && !crate::read_context::proto_named(data)).then_some(())
}
fn view(i: &Inputs) -> Option<V> {
    for key in ["threads", "tasks", "cards", "problems"] {
        i.data["reports"][key].as_array()?;
    }
    let registry = collected_source(i, "pm-mail", "pm-mail registry", &i.data["registry"], 120, true)?;
    let browsers = collected_source(
        i,
        "browsers",
        "Browsers and recordings",
        &i.data["browsers"],
        180,
        false,
    )?;
    let machines = panels::machines(i, &browsers)?;
    let reports = reports_source(i)?;
    let quota = spend::source(i)?;
    let usage_source = panels::usage_source(i)?;
    let sources = vec![
        reports,
        registry.clone(),
        panels::machines_source(i, &machines)?,
        browsers,
        collected_source(i, "simlock", "Simlock", &i.data["simlock"], 180, false)?,
        usage_source.clone(),
        quota,
    ];
    let mut view = json!({"version":1,"generatedAt":i.iso(Some(i.now)),"refresh":{"pollSeconds":30},"headline":{"strip":null},"sources":sources,"cards":cards::compose(i)?});
    if let Some(p) = i.data.get("pinned") {
        view["pinned"] = p.clone();
    }
    if let Some(s) = i.data.get("spendByStream") {
        view["spendByStream"] = s.clone();
    }
    view["projects"] = json!(projects(i)?);
    view["threads"] = json!({"source":"agent-reports","also":["pm-mail"],"rows":threads(i,&registry)?});
    view["machines"] = json!({"source":"machines","also":["browsers"],"rows":machines});
    view["simlock"] = json!({"source":"simlock","capacity":i.data["simlock"]["data"].get("capacity").cloned().unwrap_or(json!([])),"leases":i.data["simlock"]["data"].get("leases").cloned().unwrap_or(json!([]))});
    view["usage"] = panels::usage(i, &usage_source)?;
    view["spend"] = spend::compose(i)?;
    let mut view = crate::http_views::status_owned(view)?;
    view["headline"]["strip"] = strip::compose(&view)?;
    Some(view)
}
