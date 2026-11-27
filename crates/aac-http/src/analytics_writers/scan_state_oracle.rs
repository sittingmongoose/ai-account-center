//! The JSON-value scanner state of the base commit (before typed prints and answers), kept as
//! the parity oracle: the typed cache must apply every answer and build every request as it did.
#![cfg(test)]
use super::{REQUEST_BYTES, Window, fold, json_len};
use aac_store::scan_row::{Interner, ScanRow};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub(super) struct Oracle {
    pub fingerprints: Map<String, Value>,
    pub rows: Vec<ScanRow>,
    pub srows: Vec<ScanRow>,
    pub states: Map<String, Value>,
    strings: Interner,
}

pub(super) fn requests(
    mut args: Value,
    kinds: &[String],
    fingerprints: &Map<String, Value>,
    settled: &HashMap<String, u64>,
    now: i64,
) -> Vec<(Vec<String>, Value)> {
    let slot = now.max(0) as u64 / aac_usage::collector::SETTLED_CHECK_MS;
    let settings = json!({ "settledBefore": settled, "settledSlot": slot });
    args["fingerprints"] = json!(fingerprints);
    args["fingerprints"]["_store"] = settings.clone();
    if json_len(&args) <= REQUEST_BYTES {
        return vec![(kinds.to_vec(), args)];
    }

    args["fingerprints"] = json!({"_store":settings});
    kinds
        .iter()
        .map(|kind| {
            let mut one = args.clone();
            one["kinds"] = json!([kind]);
            let mut selected = Map::new();
            if let Some(prints) = fingerprints.get(kind).and_then(Value::as_object) {
                let cutoff = settled.get(kind).copied().unwrap_or(0) as i64;
                let (settled, mut files): (Vec<_>, Vec<_>) = prints
                    .iter()
                    .partition(|(_, print)| aac_usage::collector::settled_print(print, cutoff));
                if !settled.is_empty() {
                    let settled = settled
                        .into_iter()
                        .map(|(key, print)| (key.clone(), print.clone()))
                        .collect();
                    let Some(packed) = aac_usage::protocol::pack_settled(&settled) else {
                        return (vec![kind.clone()], Value::Null);
                    };
                    one["fingerprints"]["_store"]["settledPacked"][kind] = json!(packed);
                }
                // Carry unfinished cursors first. Other files can be re-read to recover any
                // fingerprint omitted to fit the existing scanner frame.
                files.sort_by_key(|(_, print)| {
                    (
                        print["readThrough"].is_null(),
                        std::cmp::Reverse(print["mtimeMs"].as_i64()),
                    )
                });
                let mut remaining = REQUEST_BYTES.saturating_sub(json_len(&one) + kind.len() + 12);
                for (key, print) in files {
                    let bytes = json!(key).to_string().len() + json_len(print) + 2;
                    if bytes <= remaining {
                        selected.insert(key.clone(), print.clone());
                        remaining -= bytes;
                    }
                }
            }
            one["fingerprints"][kind] = Value::Object(selected);
            if json_len(&one) > REQUEST_BYTES {
                return (vec![kind.clone()], Value::Null);
            }
            (vec![kind.clone()], one)
        })
        .collect()
}

impl Oracle {
    fn apply(&mut self, kind: &str, answer: &Value, mut complete: bool, cutoff: i64) -> bool {
        let state = &answer["kinds"][kind];
        let Some(fresh) = state["fingerprints"].as_object() else {
            return false;
        };
        if matches!(
            state["state"].as_str(),
            Some("pending" | "error" | "unreadable")
        ) || kind == "antigravity" && state["unreadable"] == true
        {
            return false;
        }
        let empty = Map::new();
        let prior = self
            .fingerprints
            .get(kind)
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        let mut replace: HashSet<&str> = state["replacedFiles"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let mut accepted = HashSet::new();
        let mut rejected = Vec::new();
        let mut prints = Map::new();
        for (key, print) in fresh {
            let old = prior.get(key);
            if print["settledConfirmed"] == true {
                if let Some(mut saved) = old.and_then(Value::as_object).cloned() {
                    saved.insert(
                        "settledCheckedSlot".into(),
                        print["settledCheckedSlot"].clone(),
                    );
                    prints.insert(key.clone(), Value::Object(saved));
                } else {
                    complete = false;
                }
                continue;
            }
            if let Some(from) = print["readFrom"].as_u64() {
                let matches = old.is_some_and(|old| {
                    (old["readThrough"].as_u64() == Some(from) && same_file(old, print))
                        || (old["readThrough"].is_null()
                            && old.get("appendState").is_some()
                            && old["size"].as_u64() == Some(from)
                            && print["fileIdentity"].is_string()
                            && old["fileIdentity"] == print["fileIdentity"])
                });
                if !matches {
                    rejected.push(key);
                    complete = false;
                    replace.remove(key.as_str());
                    continue;
                }
                replace.remove(key.as_str());
            } else if !old.is_some_and(|old| same_file(old, print)) {
                replace.insert(key);
            }
            accepted.insert(key.as_str());
            let mut saved = print.clone();
            if let Some(fields) = saved.as_object_mut() {
                fields.remove("readFrom");
            }
            prints.insert(key.clone(), saved);
        }
        // Cursor deduplicates run IDs across every database it reads. With a full
        // inventory, only confirmed paths may contribute, even if some records in
        // those databases were unreadable. A limited inventory keeps missing paths.
        let enumerated = complete
            || kind == "cursor"
                && answer["truncated"] == false
                && state["state"] == "ok"
                && state["unreadableRecords"].as_u64().is_some_and(|n| n > 0)
                && ["readFailures", "unreadableFolders"]
                    .iter()
                    .all(|field| state[field].as_u64().unwrap_or(0) == 0)
                && state["searchLimit"].is_null()
                && state["walUnread"] != true;
        if !enumerated {
            for (key, old) in prior {
                if !replace.contains(key.as_str()) && !rejected.contains(&key) {
                    prints.entry(key.clone()).or_insert_with(|| old.clone());
                }
            }
        }
        let incoming = |key: &str| {
            answer[key].as_array().into_iter().flatten().filter(|row| {
                row["k"] == kind
                    && row["f"]
                        .as_str()
                        .is_some_and(|file| accepted.contains(file))
            })
        };
        for row in incoming("rows").chain(incoming("srows")) {
            if let Some(file) = row["f"].as_str()
                && fresh[file]["readFrom"].is_null()
            {
                replace.insert(file);
            }
        }
        let retain = |row: &ScanRow| {
            &*row.kind != kind
                || !replace.contains(&*row.file) && (!enumerated || prints.contains_key(&*row.file))
        };
        let mut window = Window::new(cutoff);
        self.rows
            .retain(|row| retain(row) && window.keeps(row, false));
        self.srows
            .retain(|row| retain(row) && window.keeps(row, true));
        let strings = &mut self.strings;
        for (key, rows, session) in [
            ("rows", &mut self.rows, false),
            ("srows", &mut self.srows, true),
        ] {
            rows.extend(
                incoming(key)
                    .map(|row| ScanRow::from_value(row, strings))
                    .filter(|row| window.keeps(row, session)),
            );
        }
        fold(&mut self.rows, false);
        fold(&mut self.srows, true);
        self.strings.prune();
        self.fingerprints
            .insert(kind.to_owned(), Value::Object(prints));
        let mut metadata = state.clone();
        if let Some(fields) = metadata.as_object_mut() {
            fields.remove("fingerprints");
            fields.remove("replacedFiles");
        }
        self.states.insert(kind.to_owned(), metadata);
        complete
    }
}

fn same_file(old: &Value, fresh: &Value) -> bool {
    ["size", "mtimeMs", "head", "tail", "walSize", "walMtimeMs"]
        .into_iter()
        .all(|key| old[key] == fresh[key])
        && (old["fileIdentity"].is_null() || old["fileIdentity"] == fresh["fileIdentity"])
}

mod parity {
    use super::super::Cache;
    use super::super::scan_print::Fingerprints;
    use super::*;
    use base64::Engine as _;

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
        fn pick<T: Clone>(&mut self, items: &[T]) -> T {
            items[self.below(items.len() as u64) as usize].clone()
        }
        fn chance(&mut self, percent: u64) -> bool {
            self.below(100) < percent
        }
    }

    const FILES: [&str; 6] = ["a", "b", "c", "d", "e", "f"];
    const NOW: i64 = 1_795_212_000_000;
    const HOUR: i64 = 3_600_000;

    fn print(rng: &mut Rng) -> Value {
        if rng.chance(3) {
            return json!(rng.below(3));
        }
        let mut print = Map::new();
        for (key, values) in [
            ("size", vec![json!(100), json!(150), json!(80)]),
            ("mtimeMs", vec![json!(1), json!(2), json!(NOW - 900 * HOUR)]),
            ("head", vec![json!("h1"), json!("h2")]),
            ("tail", vec![json!("t1"), json!("t2")]),
            (
                "fileIdentity",
                vec![json!("9:1"), json!("9:2"), Value::Null],
            ),
            ("readThrough", vec![json!(100), json!(150), Value::Null]),
            (
                "appendState",
                vec![json!({}), json!({"m":[1,{"x":"y"}]}), Value::Null],
            ),
            ("pathKey", vec![json!("k1"), json!(5)]),
            ("newestUsageMs", vec![json!(1), json!(NOW)]),
            ("walSize", vec![json!(1)]),
            ("usageSkipped", vec![json!(true)]),
        ] {
            if rng.chance(55) {
                print.insert(key.into(), rng.pick(&values));
            }
        }
        Value::Object(print)
    }

    fn row(rng: &mut Rng, kind: &str, session: bool) -> Value {
        if rng.chance(3) {
            return json!([1]);
        }
        let file = rng.pick(&FILES);
        let kind = if rng.chance(10) { "other" } else { kind };
        let hour = rng.pick(&[NOW - 2 * HOUR, NOW - 40 * 24 * HOUR]);
        let at = aac_num::ecma::to_iso_string(hour).unwrap_or_default();
        let mut row = json!({"k":kind,"f":file,"m":rng.pick(&["m1","m2"]),
            "i":rng.below(50),"o":rng.below(5),"c":rng.pick(&[0.0, 0.25]),"n":1});
        if session {
            row["s"] = json!(rng.pick(&["s1", "s2"]));
            row["a"] = json!(hour - HOUR);
            row["z"] = json!(hour);
        } else {
            row["h"] = json!(format!("{} {}", &at[..10], &at[11..13]));
        }
        row
    }

    fn fresh_print(rng: &mut Rng, prior: Option<&Value>) -> Value {
        let mut print = match prior {
            Some(prior) if rng.chance(50) => prior.clone(),
            _ => print(rng),
        };
        if let Some(fields) = print.as_object_mut() {
            match rng.below(6) {
                0 => {
                    fields.clear();
                    fields.insert("settledConfirmed".into(), json!(true));
                    if rng.chance(70) {
                        fields.insert("settledCheckedSlot".into(), json!(rng.pick(&[1, 2])));
                    }
                }
                1 => {
                    let from = prior
                        .and_then(|p| p.get(rng.pick(&["readThrough", "size"])).cloned())
                        .unwrap_or(json!(100));
                    fields.insert("readFrom".into(), from);
                }
                2 => {
                    fields.insert("readFrom".into(), rng.pick(&[Value::Null, json!("x")]));
                }
                3 => {
                    fields.insert("size".into(), json!(rng.below(300)));
                }
                _ => {}
            }
        }
        print
    }

    fn answer(rng: &mut Rng, kind: &str, prior: &Map<String, Value>) -> Value {
        let mut prints = Map::new();
        for file in FILES {
            if rng.chance(60) {
                prints.insert(file.into(), fresh_print(rng, prior.get(file)));
            }
        }
        let fingerprints = if rng.chance(4) {
            json!([1])
        } else {
            Value::Object(prints)
        };
        let mut read = json!({"state": rng.pick(&["ok","ok","ok","pending","error","empty"]),
            "fingerprints": fingerprints});
        if rng.chance(30) {
            read["replacedFiles"] = json!([rng.pick(&FILES), 7]);
        }
        for (key, values) in [
            ("unreadableRecords", vec![json!(1), json!(0)]),
            ("partial", vec![json!(true)]),
            ("readFailures", vec![json!(1)]),
            ("searchLimit", vec![json!(1)]),
            ("walUnread", vec![json!(true)]),
            ("unreadable", vec![json!(true)]),
        ] {
            if rng.chance(15) {
                read[key] = rng.pick(&values);
            }
        }
        let rows: Vec<Value> = (0..rng.below(6)).map(|_| row(rng, kind, false)).collect();
        let srows: Vec<Value> = (0..rng.below(4)).map(|_| row(rng, kind, true)).collect();
        json!({"truncated": rng.pick(&[json!(false), json!(true)]),
            "kinds": {(kind): read}, "rows": rows, "srows": srows})
    }

    fn sorted_rows(rows: &[ScanRow]) -> Vec<String> {
        let mut rows: Vec<String> = rows.iter().map(|row| row.to_value().to_string()).collect();
        rows.sort();
        rows
    }

    /// A request's arguments with `settledPacked` unpacked, for comparing content.
    fn unpacked(mut args: Value) -> Value {
        if let Some(packed) = args["fingerprints"]["_store"]["settledPacked"].as_object_mut() {
            for value in packed.values_mut() {
                let bytes = base64::engine::general_purpose::STANDARD_NO_PAD
                    .decode(value.as_str().expect("packed text"))
                    .expect("base64");
                let json = zstd::stream::decode_all(&bytes[..]).expect("zstd");
                *value = serde_json::from_slice(&json).expect("packed json");
            }
        }
        args
    }

    #[test]
    fn typed_scanner_state_applies_and_requests_as_the_json_oracle() {
        let kinds = [
            "codex".to_owned(),
            "cursor".to_owned(),
            "antigravity".to_owned(),
        ];
        let mut rng = Rng(0x5eed_1234_abcd_0001);
        for case in 0..400 {
            let mut oracle = Oracle::default();
            let mut typed = Cache::default();
            for step in 0..4 {
                let kind = rng.pick(&kinds);
                let prior = oracle
                    .fingerprints
                    .get(&kind)
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                let answer = answer(&mut rng, &kind, &prior);
                let complete = rng.chance(60);
                let cutoff = rng.pick(&[0, NOW - 31 * 24 * HOUR]);
                let left = oracle.apply(&kind, &answer, complete, cutoff);
                let right = typed.apply_value(&kind, &answer, complete, cutoff);
                let at = format!("case {case} step {step}: {answer}");
                assert_eq!(left, right, "complete, {at}");
                assert_eq!(
                    Value::Object(oracle.fingerprints.clone()),
                    typed.fingerprints.to_value(),
                    "prints, {at}"
                );
                assert_eq!(
                    sorted_rows(&oracle.rows),
                    sorted_rows(&typed.rows),
                    "rows, {at}"
                );
                assert_eq!(
                    sorted_rows(&oracle.srows),
                    sorted_rows(&typed.srows),
                    "srows, {at}"
                );
                assert_eq!(oracle.states, typed.states, "states, {at}");
            }
            // The requests carry the same prints, settled ones packed alike.
            let settled: HashMap<String, u64> = kinds
                .iter()
                .map(|kind| (kind.clone(), rng.pick(&[0, 3])))
                .collect();
            if case % 50 == 0 {
                let big = json!({"extra": "a".repeat(REQUEST_BYTES / 2)});
                oracle.fingerprints.insert(
                    "codex".into(),
                    json!({"big": big, "settled":
                    {"pathKey":"k","mtimeMs":1,"newestUsageMs":1,"appendState":{}}}),
                );
                oracle
                    .fingerprints
                    .insert("cursor".into(), json!({"big": big}));
                typed.fingerprints =
                    Fingerprints::from_value(&Value::Object(oracle.fingerprints.clone()));
            }
            let args = json!({"kinds": kinds, "minDateMs": 0});
            let left = requests(args.clone(), &kinds, &oracle.fingerprints, &settled, NOW);
            let right = super::super::requests(args, &kinds, &typed.fingerprints, &settled, NOW);
            assert_eq!(left.len(), right.len(), "case {case} batches");
            for ((left_kinds, left), (right_kinds, right)) in left.into_iter().zip(right) {
                assert_eq!(left_kinds, right_kinds, "case {case}");
                match right {
                    None => assert!(left.is_null(), "case {case}: {left}"),
                    Some(right) => {
                        if left["fingerprints"]["_store"]["settledPacked"].is_null() {
                            assert_eq!(
                                left.to_string().len(),
                                right.text.get().len(),
                                "case {case}"
                            );
                        }
                        assert_eq!(unpacked(left), unpacked(right.value()), "case {case}");
                    }
                }
            }
        }
    }
}
