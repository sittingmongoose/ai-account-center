//! aac-num against its reference runtimes: every row of the corpora `tests/spec/num/make-node-corpus.mjs` and
//! `make-python-corpus.py` wrote from real Node and real CPython, plus the M3 corpora the views were proven on
//! (`tests/spec/read-numbers.json`, `tests/spec/agent/python-round8.json`).
// Exact binary ties are written out in full on purpose.
#![allow(clippy::excessive_precision)]
use aac_num::{civil, ecma, intl, py, view};
use serde_json::Value;
use std::cmp::Ordering;

type Result = std::result::Result<(), String>;

fn load(relative: &str) -> std::result::Result<Value, String> {
    let path = format!("{}/../../tests/spec/{relative}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))
}

fn rows<'a>(doc: &'a Value, key: &str) -> std::result::Result<&'a Vec<Value>, String> {
    doc[key].as_array().ok_or(format!("no {key}"))
}

fn bits(v: &Value) -> std::result::Result<f64, String> {
    let text = v.as_str().ok_or("bits")?;
    u64::from_str_radix(text, 16)
        .map(f64::from_bits)
        .map_err(|e| format!("{text}: {e}"))
}

fn text(v: &Value) -> Option<&str> {
    v.as_str()
}

/// Collects failures and reports the first few with the total.
struct Check {
    name: &'static str,
    failures: Vec<String>,
    count: usize,
}

impl Check {
    fn new(name: &'static str) -> Check {
        Check {
            name,
            failures: Vec::new(),
            count: 0,
        }
    }
    fn eq<T: PartialEq + std::fmt::Debug>(
        &mut self,
        got: T,
        want: T,
        what: impl FnOnce() -> String,
    ) {
        self.count += 1;
        if got != want {
            self.failures
                .push(format!("{}: got {got:?}, want {want:?}", what()));
        }
    }
    fn done(self) -> Result {
        println!(
            "{}: {} comparisons, {} failed",
            self.name,
            self.count,
            self.failures.len()
        );
        if self.failures.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "{} {} of {} failed:\n{}",
                self.name,
                self.failures.len(),
                self.count,
                self.failures
                    .iter()
                    .take(30)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n")
            ))
        }
    }
}

#[test]
fn ecmascript_number_text_and_rounding_match_node() -> Result {
    let doc = load("num/ecma.json")?;
    let places: Vec<u32> = rows(&doc, "fixedPlaces")?
        .iter()
        .filter_map(|v| v.as_u64())
        .map(|v| v as u32)
        .collect();
    let digits: Vec<u32> = rows(&doc, "precisionDigits")?
        .iter()
        .filter_map(|v| v.as_u64())
        .map(|v| v as u32)
        .collect();
    let mut c = Check::new("ecma");
    for row in rows(&doc, "rows")? {
        let x = bits(&row[0])?;
        c.eq(
            ecma::to_string(x),
            text(&row[1]).unwrap_or("").to_string(),
            || format!("String({x:e})"),
        );
        c.eq(
            format!("{:016x}", ecma::math_round(x).to_bits()),
            text(&row[2]).unwrap_or("").to_string(),
            || format!("Math.round({x:e})"),
        );
        for (i, f) in places.iter().enumerate() {
            c.eq(
                ecma::to_fixed(x, *f),
                text(&row[3][i]).map(String::from),
                || format!("({x:e}).toFixed({f})"),
            );
        }
        for (i, p) in digits.iter().enumerate() {
            c.eq(
                ecma::to_precision(x, *p),
                text(&row[4][i]).map(String::from),
                || format!("({x:e}).toPrecision({p})"),
            );
        }
        // The view domain: admitted forms are Node's, others decline.
        let shown = view::string(x);
        if let Some(word) = &shown {
            c.eq(Some(word.as_str()), text(&row[1]), || {
                format!("view String({x:e})")
            });
        }
        c.eq(shown.is_some(), view::string_admitted(x), || {
            format!("admitted {x:e}")
        });
    }
    for row in rows(&doc, "wide")? {
        let x = bits(&row[0])?;
        c.eq(
            ecma::to_fixed(x, 20),
            text(&row[1]).map(String::from),
            || format!("({x:e}).toFixed(20)"),
        );
        c.eq(
            ecma::to_fixed(x, 100),
            text(&row[2]).map(String::from),
            || format!("({x:e}).toFixed(100)"),
        );
        for (i, p) in [3, 21, 100].into_iter().enumerate() {
            c.eq(
                ecma::to_precision(x, p),
                text(&row[3 + i]).map(String::from),
                || format!("({x:e}).toPrecision({p})"),
            );
        }
    }
    for row in rows(&doc, "json")? {
        let x = bits(&row[0])?;
        c.eq(
            ecma::json(x),
            text(&row[1]).unwrap_or("").to_string(),
            || format!("JSON.stringify({x:e})"),
        );
    }
    for row in rows(&doc, "sums")? {
        let list = row[0]
            .as_array()
            .ok_or("sum list")?
            .iter()
            .map(bits)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        c.eq(
            format!("{:016x}", ecma::sum(list.iter().copied()).to_bits()),
            text(&row[1]).unwrap_or("").to_string(),
            || format!("reduce {list:?}"),
        );
    }
    c.done()
}

/// The M3 forms Status and Scoreboard were proven on (formerly aac-http `read_numbers.rs`): `toPrecision`,
/// `toFixed` and seven `Math.round` expressions, with their admitted domain.
#[test]
fn the_view_domain_matches_the_m3_reference_forms() -> Result {
    let doc = load("read-numbers.json")?;
    let mut c = Check::new("read-numbers");
    for row in doc.as_array().ok_or("rows")? {
        if row["kind"] == "math-round" {
            let value = bits(&row["valueBits"])?;
            let (operand, divisor) = match row["op"].as_str().unwrap_or("") {
                "integer" => (value, 1.0),
                "tenth" => (value * 10.0, 10.0),
                "hundredth" => (value * 100.0, 100.0),
                "percent" => (value * 100.0, 1.0),
                "gib" => ((value / 1_073_741_824.0) * 10.0, 10.0),
                "seconds" => (value / 1000.0, 1.0),
                "hours" => (value / 3_600_000.0, 1.0),
                other => return Err(format!("unknown operation {other}")),
            };
            c.eq(
                format!("{:016x}", operand.to_bits()),
                text(&row["operandBits"]).unwrap_or("").into(),
                || format!("operand {row}"),
            );
            c.eq(
                format!("{:016x}", (ecma::math_round(operand) / divisor).to_bits()),
                text(&row["expectedBits"]).unwrap_or("").into(),
                || format!("Math.round {row}"),
            );
            continue;
        }
        let value = row["value"].as_f64().ok_or("number")?;
        let admitted = row["admitted"] == true;
        match row["kind"].as_str() {
            Some("significant") => {
                let got = view::precision(value, row["digits"].as_u64().ok_or("digits")? as u32);
                c.eq(got.is_some(), admitted, || format!("admitted {row}"));
                if admitted {
                    c.eq(got, row["expected"].as_f64(), || format!("{row}"));
                }
            }
            Some("fixed") => {
                let got = view::fixed(value, row["places"].as_u64().ok_or("places")? as u32);
                c.eq(got.is_some(), admitted, || format!("admitted {row}"));
                if admitted {
                    c.eq(got.as_deref(), row["expected"].as_str(), || {
                        format!("{row}")
                    });
                }
            }
            _ => return Err(format!("unknown kind {row}")),
        }
    }
    c.done()
}

#[test]
fn intl_number_format_matches_node_on_its_domain() -> Result {
    let doc = load("num/intl.json")?;
    let mut c = Check::new("intl");
    let mut admitted = 0;
    for row in rows(&doc, "rows")? {
        let x = bits(&row[0])?;
        let shown = !x.is_finite() || view::string_admitted(x.abs());
        admitted += usize::from(shown);
        let want = |i: usize| shown.then(|| text(&row[i]).unwrap_or("").to_string());
        c.eq(intl::decimal2(x), want(1), || format!("nf2 {x:e}"));
        c.eq(intl::usd(x), want(2), || format!("usd {x:e}"));
        c.eq(intl::compact(x), want(3), || format!("compact {x:e}"));
    }
    println!("intl: {admitted} values admitted");
    c.done()
}

#[test]
fn dates_match_node() -> Result {
    let doc = load("num/dates.json")?;
    let mut c = Check::new("dates");
    let mut admitted = 0;
    for row in rows(&doc, "parses")? {
        let s = text(&row[0]).unwrap_or("");
        if let Some(ms) = ecma::parse_instant(s) {
            admitted += 1;
            c.eq(Some(ms), row[1].as_i64(), || format!("Date.parse({s:?})"));
        }
    }
    println!("dates: {admitted} strings admitted by parse_instant");
    let (low, high) = ecma::ISO_STRING_DOMAIN;
    for row in rows(&doc, "iso")? {
        let ms = row[0].as_i64().ok_or("ms")?;
        let want = (low..=high)
            .contains(&ms)
            .then(|| text(&row[1]).unwrap_or("").to_string());
        c.eq(ecma::to_iso_string(ms), want, || {
            format!("toISOString({ms})")
        });
    }
    for row in rows(&doc, "civil")? {
        let days = row[0].as_i64().ok_or("days")?;
        let want = (row[1].as_i64(), row[2].as_i64(), row[3].as_i64());
        let (y, m, d) = civil::civil_from_days(days);
        c.eq((Some(y), Some(m), Some(d)), want, || {
            format!("civil {days}")
        });
        c.eq(civil::days_from_civil(y, m, d), days, || {
            format!("days {days}")
        });
        c.eq(Some(civil::weekday(days) as i64), row[4].as_i64(), || {
            format!("weekday {days}")
        });
    }
    for row in rows(&doc, "ages")? {
        let now = row[0].as_i64().ok_or("now")?;
        let mtime = bits(&row[1])?;
        let seconds = ecma::math_round((now as f64 - mtime) / 1000.0);
        let age = if seconds > 0.0 { seconds } else { 0.0 };
        c.eq(
            format!("{:016x}", age.to_bits()),
            text(&row[2]).unwrap_or("").into(),
            || format!("ageSeconds({now}, {mtime})"),
        );
    }
    c.done()
}

#[test]
fn ascii_collation_matches_node() -> Result {
    let doc = load("num/collation.json")?;
    let mut c = Check::new("collation");
    c.eq(intl::ASCII_ORDER, text(&doc["order"]).unwrap_or(""), || {
        "single-character order".into()
    });
    let order: Vec<char> = intl::ASCII_ORDER.chars().collect();
    let primary_equal: String = order
        .windows(2)
        .filter(|w| {
            intl::ascii_compare(&w[0].to_string(), &w[1].to_string()) == Some(Ordering::Less)
                && w[0].eq_ignore_ascii_case(&w[1])
        })
        .map(|w| w[1])
        .collect();
    c.eq(
        primary_equal.as_str(),
        text(&doc["primaryEqual"]).unwrap_or(""),
        || "primary groups".into(),
    );
    for row in rows(&doc, "pairs")? {
        let (a, b) = (text(&row[0]).unwrap_or(""), text(&row[1]).unwrap_or(""));
        let want = match row[2].as_i64() {
            Some(-1) => Ordering::Less,
            Some(0) => Ordering::Equal,
            _ => Ordering::Greater,
        };
        c.eq(intl::ascii_compare(a, b), Some(want), || {
            format!("{a:?}.localeCompare({b:?})")
        });
    }
    c.done()
}

#[test]
fn python_round_repr_and_timestamps_match_cpython() -> Result {
    let doc = load("num/python.json")?;
    let places: Vec<i32> = rows(&doc, "places")?
        .iter()
        .filter_map(|v| v.as_i64())
        .map(|v| v as i32)
        .collect();
    let wide: Vec<i32> = rows(&doc, "widePlaces")?
        .iter()
        .filter_map(|v| v.as_i64())
        .map(|v| v as i32)
        .collect();
    let mut c = Check::new("python");
    for row in rows(&doc, "rows")? {
        let x = bits(&row[0])?;
        c.eq(py::repr(x), text(&row[1]).unwrap_or("").to_string(), || {
            format!("repr {x:e}")
        });
        c.eq(
            py::json_dumps(x),
            text(&row[2]).unwrap_or("").to_string(),
            || format!("json.dumps {x:e}"),
        );
        for (i, n) in places.iter().enumerate() {
            c.eq(
                py::round(x, *n).map(py::repr),
                text(&row[3][i]).map(String::from),
                || format!("round({}, {n})", py::repr(x)),
            );
        }
    }
    for row in rows(&doc, "wide")? {
        let x = bits(&row[0])?;
        for (i, n) in wide.iter().enumerate() {
            c.eq(
                py::round(x, *n).map(py::repr),
                text(&row[1][i]).map(String::from),
                || format!("round({}, {n})", py::repr(x)),
            );
        }
    }
    for row in rows(&doc, "millis")? {
        let ms = row[0].as_i64().ok_or("ms")?;
        c.eq(py::iso_millis(ms), text(&row[1]).map(String::from), || {
            format!("iso({ms})")
        });
    }
    for row in rows(&doc, "seconds")? {
        let t: f64 = text(&row[0])
            .unwrap_or("")
            .parse()
            .map_err(|e| format!("{row}: {e}"))?;
        c.eq(
            py::iso_seconds_of_timestamp(t).map(|(word, _)| word),
            text(&row[1]).map(String::from),
            || format!("fromtimestamp({t})"),
        );
    }
    // `None` declines (the collector falls back to Python): an impossible date, which Python reads as no time, and
    // `T24:00:00`, which CPython 3.14 reads as the next midnight but older Pythons refuse.
    let mut declined = Vec::new();
    for row in rows(&doc, "resets")? {
        let s = text(&row[0]).unwrap_or("");
        match py::parse_z_seconds(s).and_then(|(seconds, _)| py::iso_seconds(seconds)) {
            Some(word) => c.eq(Some(word), text(&row[1]).map(String::from), || {
                format!("fromisoformat({s:?})")
            }),
            None if row[1].is_null() => {}
            None => declined.push(s.to_string()),
        }
    }
    c.eq(
        declined.iter().all(|s| s.get(11..19) == Some("24:00:00")),
        true,
        || format!("declined strings Python reads: {declined:?}"),
    );
    let table = load("agent/python-round8.json")?;
    for row in rows(&table, "rows")? {
        let x: f64 = text(&row[0])
            .unwrap_or("")
            .parse()
            .map_err(|e| format!("{row}: {e}"))?;
        c.eq(
            py::round(x, 8).map(py::repr),
            text(&row[1]).map(String::from),
            || format!("round({x}, 8)"),
        );
    }
    c.done()
}

/// The copies L3 deleted, reproduced here only to show where each disagreed with its reference runtime while aac-num
/// gives the reference's answer (the rows above prove aac-num on every row).
mod deleted {
    /// aac-api `home::js_number_text` before L3: ECMAScript notation over Rust's shortest digits.
    pub fn js_number_text(x: f64) -> String {
        if x == 0.0 {
            return "0".into();
        }
        if !x.is_finite() {
            return if x.is_nan() {
                "NaN".into()
            } else if x > 0.0 {
                "Infinity".into()
            } else {
                "-Infinity".into()
            };
        }
        let sci = format!("{:e}", x.abs());
        let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
        let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
        let k = digits.len() as i64;
        let n = exp.parse::<i64>().unwrap_or(0) + 1;
        let mut out = String::new();
        if x < 0.0 {
            out.push('-');
        }
        if k <= n && n <= 21 {
            out.push_str(&digits);
            out.extend(std::iter::repeat_n('0', (n - k) as usize));
        } else if 0 < n && n <= 21 {
            out.push_str(&digits[..n as usize]);
            out.push('.');
            out.push_str(&digits[n as usize..]);
        } else if -6 < n && n <= 0 {
            out.push_str("0.");
            out.extend(std::iter::repeat_n('0', (-n) as usize));
            out.push_str(&digits);
        } else {
            let e = n - 1;
            out.push_str(&digits[..1]);
            if k > 1 {
                out.push('.');
                out.push_str(&digits[1..]);
            }
            out.push('e');
            out.push(if e < 0 { '-' } else { '+' });
            out.push_str(&e.abs().to_string());
        }
        out
    }

    /// signin.rs `iso()` before L3: the exact integer milliseconds.
    pub fn signin_iso(ms: i64) -> String {
        let (y, mo, d, h, mi, s, milli) = aac_num::civil::utc_fields(ms);
        format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{milli:03}Z")
    }

    /// quota `parse.rs` `python_round` before L3: exact for up to 17 places, the value itself beyond.
    pub fn python_round(value: f64, places: u32) -> f64 {
        if !value.is_finite() || value == 0.0 || places > 17 {
            return value;
        }
        aac_num::py::round(value, places as i32).unwrap_or(value)
    }

    /// quota `reset()` before L3 on a number: the second floored, never rounded to the microsecond.
    pub fn reset_floor(t: f64) -> String {
        let seconds = t.floor() as i64;
        let (y, mo, d, h, mi, s, _) = aac_num::civil::utc_fields(seconds * 1000);
        format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
    }
}

#[test]
fn each_deleted_copy_disagreed_with_its_reference_where_aac_num_agrees() -> Result {
    let ecma_doc = load("num/ecma.json")?;
    let mut node = Vec::new();
    for row in rows(&ecma_doc, "rows")? {
        let x = bits(&row[0])?;
        let want = text(&row[1]).unwrap_or("");
        if deleted::js_number_text(x) != want && ecma::to_string(x) == want {
            node.push(x);
        }
    }
    let python = load("num/python.json")?;
    let mut millis = Vec::new();
    for row in rows(&python, "millis")? {
        let ms = row[0].as_i64().ok_or("ms")?;
        if let Some(want) = text(&row[1])
            && deleted::signin_iso(ms) != want
            && py::iso_millis(ms).as_deref() == Some(want)
        {
            millis.push(ms);
        }
    }
    let places: Vec<i64> = rows(&python, "places")?
        .iter()
        .filter_map(Value::as_i64)
        .collect();
    let mut rounds = 0;
    for row in rows(&python, "rows")? {
        let x = bits(&row[0])?;
        for (i, n) in places.iter().enumerate() {
            if let Some(want) = text(&row[3][i])
                && *n >= 0
                && py::repr(deleted::python_round(x, *n as u32)) != want
                && py::round(x, *n as i32).map(py::repr).as_deref() == Some(want)
            {
                rounds += 1;
            }
        }
    }
    let mut seconds = 0;
    for row in rows(&python, "seconds")? {
        let t: f64 = text(&row[0])
            .unwrap_or("")
            .parse()
            .map_err(|e| format!("{row}: {e}"))?;
        if let Some(want) = text(&row[1])
            && deleted::reset_floor(t) != want
            && py::iso_seconds_of_timestamp(t).map(|(w, _)| w).as_deref() == Some(want)
        {
            seconds += 1;
        }
    }
    println!(
        "deleted copies wrong where aac-num is right: String {} values, signin iso {} ms, round {} forms, reset {} stamps",
        node.len(),
        millis.len(),
        rounds,
        seconds
    );
    let has = |list: &[f64], x: f64| list.iter().any(|v| v.to_bits() == x.to_bits());
    if !has(&node, 2251799813685247.25) || !has(&node, 140737488355327.125) {
        return Err(format!(
            "the reviewers' String witnesses are not among {} disagreements",
            node.len()
        ));
    }
    if !millis.contains(&8_796_093_022_210) || !millis.contains(&9_999_999_999_001) {
        return Err(format!("the Sol witnesses are not among {millis:?}"));
    }
    if rounds == 0 || seconds == 0 {
        return Err(format!(
            "expected disagreements: round {rounds}, reset {seconds}"
        ));
    }
    Ok(())
}
