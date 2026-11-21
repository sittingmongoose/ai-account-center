//! DESIGN 17.7: the Cursor account reader against a loopback fake of Cursor's calls (no real key,
//! no network), the key store, and the dedupe against the local runs.

use aac_cursor_account::{api, dedupe, keys, util};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

const KEY: &str = "key_TEST_0123456789abcdef_NOT_REAL";
const SUB: &str = "auth0|user_TEST_subject";

/// A JWT whose payload names `SUB` (unsigned: the reader only decodes it).
fn token() -> String {
    let encode = |bytes: &[u8]| {
        const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
            for i in 0..chunk.len() + 1 {
                out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
            }
        }
        out
    };
    format!(
        "{}.{}.sig",
        encode(br#"{"alg":"none"}"#),
        encode(json!({"sub": SUB}).to_string().as_bytes())
    )
}

/// The fake's answer: status, extra headers, body.
type Reply = (u16, Vec<(&'static str, String)>, String);

#[derive(Clone, Debug)]
struct Seen {
    path: String,
    authorization: String,
    cookie: String,
    connect: bool,
    body: String,
}

/// A one-thread HTTP/1.1 fake: `answer(path, body)` gives (status, extra headers, body).
fn fake<F>(answer: F) -> (String, Arc<Mutex<Vec<Seen>>>)
where
    F: Fn(&str, &Value) -> Reply + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() || line.is_empty() {
                continue;
            }
            let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
            let (mut length, mut authorization, mut connect) = (0usize, String::new(), false);
            let mut cookie = String::new();
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                let header = header.trim_end().to_string();
                if header.is_empty() {
                    break;
                }
                let (name, value) = header.split_once(':').unwrap();
                match name.to_ascii_lowercase().as_str() {
                    "content-length" => length = value.trim().parse().unwrap(),
                    "authorization" => authorization = value.trim().to_string(),
                    "cookie" => cookie = value.trim().to_string(),
                    "connect-protocol-version" => connect = value.trim() == "1",
                    _ => {}
                }
            }
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).unwrap();
            let body = String::from_utf8(body).unwrap();
            let request: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            log.lock().unwrap().push(Seen {
                path: path.clone(),
                authorization,
                cookie,
                connect,
                body: body.clone(),
            });
            let (status, headers, text) = answer(&path, &request);
            if status == 0 {
                // Hang past the client's 10 s timeout.
                std::thread::sleep(std::time::Duration::from_secs(12));
                continue;
            }
            let mut response = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                text.len()
            );
            for (name, value) in headers {
                response.push_str(&format!("{name}: {value}\r\n"));
            }
            response.push_str("\r\n");
            response.push_str(&text);
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (base, seen)
}

fn event(n: u64, model: &str, cents: Option<f64>) -> Value {
    let mut usage = json!({"inputTokens": 100 + n, "outputTokens": 10, "cacheReadTokens": 1000, "cacheWriteTokens": 5});
    if let Some(cents) = cents {
        usage["totalCents"] = json!(cents);
    }
    json!({"timestamp": (1_794_000_000_000u64 + n * 1000).to_string(), "model": model,
           "conversationId": format!("conv-{}", n % 3), "tokenUsage": usage})
}

fn exchange_ok(path: &str) -> Option<Reply> {
    (path == "/auth/exchange_user_api_key").then(|| (200, vec![], json!({"accessToken": token()}).to_string()))
}

const WINDOW: (i64, i64) = (1_793_000_000_000, 1_795_000_000_000);

#[test]
fn reads_every_page_and_drops_the_copies_a_boundary_repeats() {
    // 1,003 events; page 2 starts with the last two of page 1 again (the account gained two while
    // paging), so 1,005 arrive and the count proves two are copies.
    let all: Vec<Value> = (0..1003).map(|n| event(n, "grok-4.7", Some(0.5))).collect();
    let (base, seen) = fake(move |path, body| {
        if let Some(answer) = exchange_ok(path) {
            return answer;
        }
        let page = body["page"].as_u64().unwrap();
        let events: Vec<Value> = if page == 1 {
            all[..1000].to_vec()
        } else {
            all[998..].to_vec()
        };
        (
            200,
            vec![],
            json!({"totalUsageEventsCount": 1003, "usageEventsDisplay": events}).to_string(),
        )
    });
    let client = api::Client::new(Some(&base));
    let access = client.exchange(KEY).unwrap();
    let (events, counts, unmeasured) = api::events(&client, &access, WINDOW.0, WINDOW.1).unwrap();
    assert_eq!(counts.pages, 2);
    assert_eq!(counts.fetched, 1005);
    assert_eq!(counts.kept, 1003);
    assert_eq!(counts.dropped_duplicate, 2);
    assert_eq!(unmeasured, 0);
    assert_eq!(events.len(), 1003);
    assert_eq!(
        events.iter().map(|e| e.tokens[0]).sum::<u64>(),
        (0..1003u64).map(|n| 100 + n).sum::<u64>()
    );
    // The key went only to the exchange; the token, with the connect header, to the pages.
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen[0].authorization, format!("Bearer {KEY}"));
    assert!(
        seen[1..]
            .iter()
            .all(|s| s.authorization == format!("Bearer {}", token()) && s.connect)
    );
    let first: Value = serde_json::from_str(&seen[1].body).unwrap();
    assert_eq!(
        first,
        json!({"page": 1, "pageSize": 1000, "startDate": WINDOW.0.to_string(),
                             "endDate": WINDOW.1.to_string()})
    );
    // The account is a hash of the token's subject; conversations are session keys.
    let account = api::account_hash(&access).unwrap();
    assert_eq!(
        account,
        util::sha256_hex(format!("aac-cursor-account-v1:{SUB}").as_bytes())[..16]
    );
    assert!(events.iter().all(|e| e.session.as_ref().is_some_and(|s| s.len() == 16)));
    // Hourly rows carry Cursor's own cost.
    let rows = api::hourly(&events);
    let cents: f64 = rows.iter().map(|r| r["cents"].as_f64().unwrap()).sum();
    assert!((cents - 1003.0 * 0.5).abs() < 1e-6);
}

#[test]
fn an_event_without_measured_counts_is_unmeasured_never_measured_zeros() {
    // The review's M3 input, beside the neighbours it must not be confused with: a measured
    // event, one in range with no tokenUsage at all, one outside the window, and two whose
    // tokenUsage is empty or incomplete ({}, and inputTokens only). The in-range ones without
    // counts read as not logged (counted separately), never as an hour of zeros; the one without
    // tokenUsage is usage too (review wave 82, Low 1), so three are unmeasured.
    let (base, _) = fake(|path, _| {
        if let Some(answer) = exchange_ok(path) {
            return answer;
        }
        let mut late = event(1, "kimi-k3", None);
        late["timestamp"] = json!(WINDOW.1 + 5);
        let events = vec![
            event(0, "kimi-k3", None),
            json!({"timestamp": "1794000000500", "model": "kimi-k3"}),
            late,
            json!({"timestamp": "1794000000500", "model": "grok-4.7", "tokenUsage": {}}),
            json!({"timestamp": "1794000000500", "model": "grok-4.7",
                   "tokenUsage": {"inputTokens": 50}}),
        ];
        (
            200,
            vec![],
            json!({"totalUsageEventsCount": 5, "usageEventsDisplay": events}).to_string(),
        )
    });
    let client = api::Client::new(Some(&base));
    let access = client.exchange(KEY).unwrap();
    let (events, counts, unmeasured) = api::events(&client, &access, WINDOW.0, WINDOW.1).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(unmeasured, 3);
    assert_eq!(
        counts.value(),
        json!({"pages":1,"fetched":5,"kept":1,
        "droppedOutOfWindow":1,"droppedNoTokenUsage":1,"droppedDuplicate":0})
    );
    let rows = api::hourly(&events);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["m"], "kimi-k3");
    assert_eq!(rows[0]["uncosted"], 1);
}

#[test]
fn every_failure_is_a_fixed_cause_and_a_redirect_is_never_followed() {
    let cases: Vec<(u16, &str, api::Failure)> = vec![
        (401, "exchange", api::Failure::Rejected),
        (403, "page", api::Failure::Rejected),
        (500, "page", api::Failure::Server),
        (302, "page", api::Failure::BadResponse),
        (200, "inconsistent", api::Failure::BadResponse),
        (200, "short", api::Failure::BadResponse),
        (200, "error-body", api::Failure::BadResponse),
        (200, "bad-count", api::Failure::BadResponse),
    ];
    for (status, at, expected) in cases {
        let (base, seen) = fake(move |path, body| {
            if at != "exchange"
                && let Some(answer) = exchange_ok(path)
            {
                return answer;
            }
            let page = body["page"].as_u64().unwrap_or(0);
            match at {
                "exchange" | "page" => (
                    status,
                    vec![("Location", "http://127.0.0.1:9/elsewhere".to_string())],
                    "{\"message\":\"secret text Cursor wrote\"}".to_string(),
                ),
                "inconsistent" => {
                    let events: Vec<Value> = (0..1000).map(|n| event(n, "m", None)).collect();
                    (
                        200,
                        vec![],
                        json!({"totalUsageEventsCount": 1000 + page, "usageEventsDisplay": events}).to_string(),
                    )
                }
                "short" => (
                    200,
                    vec![],
                    json!({"totalUsageEventsCount": 5, "usageEventsDisplay": [event(0, "m", None)]}).to_string(),
                ),
                "bad-count" => {
                    let mut broken = event(0, "m", None);
                    broken["tokenUsage"]["inputTokens"] = json!(10.5);
                    (
                        200,
                        vec![],
                        json!({"totalUsageEventsCount": 1, "usageEventsDisplay": [broken]}).to_string(),
                    )
                }
                _ => (200, vec![], json!({"error": "x"}).to_string()),
            }
        });
        let client = api::Client::new(Some(&base));
        let result = client
            .exchange(KEY)
            .and_then(|token| api::events(&client, &token, WINDOW.0, WINDOW.1));
        assert_eq!(result.err(), Some(expected), "{status} {at}");
        assert!(seen.lock().unwrap().iter().all(|s| s.path != "/elsewhere"));
    }
}

#[test]
fn a_call_that_hangs_times_out() {
    let (base, _) = fake(|_, _| (0, vec![], String::new()));
    let client = api::Client::new(Some(&base));
    assert_eq!(client.exchange(KEY).err(), Some(api::Failure::Timeout));
}

#[test]
fn only_a_loopback_base_replaces_cursor() {
    // A non-loopback override is ignored: the key can only go to Cursor (here: unreachable, so the
    // test only checks the request never reached the would-be host).
    let (base, seen) = fake(|_, _| (200, vec![], "{}".to_string()));
    let elsewhere = base.replace("127.0.0.1", "localhost");
    let _ = api::Client::new(Some(&elsewhere));
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn quota_reads_the_plan_percentages() {
    let (base, _) = fake(|path, _| {
        if let Some(answer) = exchange_ok(path) {
            return answer;
        }
        (
            200,
            vec![],
            json!({"billingCycleStart": "1793000000000", "billingCycleEnd": 1795600000000u64,
                             "planUsage": {"totalPercentUsed": 42.5, "autoPercentUsed": 120.0}})
            .to_string(),
        )
    });
    let client = api::Client::new(Some(&base));
    let access = client.exchange(KEY).unwrap();
    let quota = api::quota(&client, &access).unwrap();
    assert_eq!(quota["billingCycleStartMs"], 1_793_000_000_000i64);
    assert_eq!(quota["planUsage"]["totalPercentUsed"], 42.5);
    // The reported percentage, unclamped: a plan over its quota reads above 100 (review L1).
    assert_eq!(quota["planUsage"]["autoPercentUsed"], 120.0);
}

#[test]
fn the_key_is_one_private_credential_never_shown() {
    let ccs = tempfile::tempdir().unwrap();
    assert!(keys::load(ccs.path()).unwrap().is_none());
    assert_eq!(keys::put(ccs.path(), "short").err(), Some(keys::KeyError::Invalid));
    assert_eq!(
        keys::put(ccs.path(), "has a space in it").err(),
        Some(keys::KeyError::Invalid)
    );
    let first = keys::put(ccs.path(), KEY).unwrap();
    assert_eq!(first.last4, "REAL");
    assert!(!format!("{first:?}").contains(KEY));
    let second = keys::put(ccs.path(), "key_SECOND_0123456789").unwrap();
    let dir = keys::directory(ccs.path());
    let files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files.len(), 1, "one key at a time: {files:?}");
    let loaded = keys::load(ccs.path()).unwrap().unwrap();
    assert_eq!(loaded.key_id, second.key_id);
    assert_eq!(loaded.secret, "key_SECOND_0123456789");
    let raw = std::fs::read(dir.join(&files[0])).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(dir.join(&files[0])).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(String::from_utf8_lossy(&raw).contains("\"provider\":\"cursor\""));
        // A key file others can read is never used.
        std::fs::set_permissions(dir.join(&files[0]), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(keys::load(ccs.path()).err(), Some(keys::KeyError::Unsafe));
    }
    #[cfg(windows)]
    {
        // DPAPI: the file holds no readable secret, and reads back for this user.
        assert!(files[0].ends_with(".dpapi"));
        assert!(!String::from_utf8_lossy(&raw).contains("key_SECOND"));
    }
    assert_eq!(keys::delete(ccs.path()), 1);
    assert!(keys::load(ccs.path()).unwrap().is_none());
}

#[test]
fn dedupe_reports_the_overlap_estimate_and_the_account_rows_as_read() {
    let row = |h: &str, m: &str, i: u64, n: u64, cents: f64| json!({"h": h, "m": m, "i": i, "o": 0, "cr": 0, "cw": 0, "n": n, "cents": cents});
    let local = |h: &str, m: &str, i: u64| json!({"h": h, "m": m, "i": i, "o": 0, "cr": 0, "cw": 0});
    let answer = dedupe::dedupe(&json!({
        "account": [
            row("2026-11-11 20:00", "grok-4.7-high", 300, 3, 30.0),   // a run's early requests
            row("2026-11-11 21:00", "grok-4.7-high", 700, 7, 70.0),   // its later ones
            row("2026-11-11 22:00", "claude-5-sonnet", 50, 1, 5.0),   // the Cursor app: outside
            row("2026-11-12 09:00", "kimi-k3", 100, 2, 4.0),          // partly a T3 run
        ],
        "local": [
            local("2026-11-11 21:00", "grok-4.7", 1000),              // the run, at its finish hour
            local("2026-11-12 09:00", "kimi-k3", 60),
            local("2026-11-12 15:00", "kimi-k3", 25),                 // no account event: unmatched
        ],
    }));
    // The rows are the account's own rows, unscaled: rows sum to accountTokens, never to the
    // local runs plus a residual.
    let rows = answer["rows"].as_array().unwrap();
    let shown: Vec<(String, u64, f64)> = rows
        .iter()
        .map(|r| {
            (
                format!("{} {}", r["h"].as_str().unwrap(), r["m"].as_str().unwrap()),
                r["i"].as_u64().unwrap(),
                r["cents"].as_f64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        vec![
            ("2026-11-11 22:00 claude-5-sonnet".to_string(), 50, 5.0),
            ("2026-11-11 20:00 grok-4.7-high".to_string(), 300, 30.0),
            ("2026-11-11 21:00 grok-4.7-high".to_string(), 700, 70.0),
            ("2026-11-12 09:00 kimi-k3".to_string(), 100, 4.0),
        ]
    );
    // The overlap estimate: 1,000 of the run's hour cover 300 + 700, and 60 of the 100 cover 60.
    assert_eq!(
        answer["summary"],
        json!({"accountTokens": 1150, "matchedTokens": 1060, "outsideTokens": 90, "localUnmatchedTokens": 25})
    );
}

#[test]
fn a_multi_hour_run_never_inflates_the_account_total() {
    // The review's witness: one T3 run over three hours, aggregated at its finish. The account
    // total is its 300 tokens; the old code also published the 100 residual the finish-hour
    // window cannot reach, counting 400 tokens for 300 tokens of work.
    let row = |h: &str| json!({"h": h, "m": "grok-4.7", "i": 100u64, "o": 0, "cr": 0, "cw": 0, "n": 1, "cents": 0.0});
    let answer = dedupe::dedupe(&json!({
        "account": [row("2026-11-11 10:00"), row("2026-11-11 11:00"), row("2026-11-11 12:00")],
        "local": [
            {"h": "2026-11-11 12:00", "m": "grok-4.7", "i": 300, "o": 0, "cr": 0, "cw": 0},
        ],
    }));
    // The account total is authoritative: 300, and the rows carry exactly those tokens.
    assert_eq!(answer["summary"]["accountTokens"], 300);
    let rows: u64 = answer["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["i"].as_u64().unwrap())
        .sum();
    assert_eq!(rows, 300);
    // The overlap estimate itself is reported as such: the finish-hour window covers the 11:00
    // and 12:00 hours, never the 10:00 one.
    assert_eq!(
        answer["summary"],
        json!({"accountTokens": 300, "matchedTokens": 200, "outsideTokens": 100, "localUnmatchedTokens": 100})
    );
}

#[test]
fn model_spellings_compare_without_prefix_and_effort() {
    for (a, b) in [
        ("cursor-grok-4.7-high", "grok-4.7"),
        ("gpt-6.1-thinking-max-fast", "gpt-6.1"),
        ("Kimi-K3", "kimi-k3"),
    ] {
        assert_eq!(util::match_model(a), util::match_model(b), "{a} {b}");
    }
    assert_eq!(
        util::hour_epoch("2026-11-11 21:00")
            .and_then(util::hour_label)
            .as_deref(),
        Some("2026-11-11 21:00")
    );
}

#[test]
fn the_binary_prints_no_key_token_or_account_id() {
    let ccs = tempfile::tempdir().unwrap();
    let (base, _) = fake(|path, _| {
        if let Some(answer) = exchange_ok(path) {
            return answer;
        }
        (
            200,
            vec![],
            json!({"totalUsageEventsCount": 1, "usageEventsDisplay": [event(0, "grok-4.7", Some(1.25))]}).to_string(),
        )
    });
    let run = |args: &[&str], input: &str| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_aac-cursor-account"))
            .args(args)
            .args(["--ccs-dir", ccs.path().to_str().unwrap()])
            .env("AAC_CURSOR_API_BASE", &base)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        serde_json::from_slice::<Value>(&out.stdout).unwrap()
    };
    assert_eq!(
        run(
            &["read"],
            &json!({"sinceMs": WINDOW.0, "untilMs": WINDOW.1}).to_string()
        )["state"],
        "no_key"
    );
    let put = run(&["key", "put"], &format!("{KEY}\n"));
    assert_eq!(put["ok"], true);
    assert_eq!(run(&["key", "status"], "")["last4"], "REAL");
    let read = run(
        &["read"],
        &json!({"sinceMs": WINDOW.0, "untilMs": WINDOW.1}).to_string(),
    );
    assert_eq!(read["state"], "read");
    assert_eq!(read["events"], 1);
    assert_eq!(read["unmeasured"], 0);
    assert_eq!(
        read["eventCounts"],
        json!({"pages":1,"fetched":1,"kept":1,
        "droppedOutOfWindow":0,"droppedNoTokenUsage":0,"droppedDuplicate":0})
    );
    let text = format!("{put}{read}");
    for secret in [KEY, SUB, token().as_str(), "conv-0"] {
        assert!(!text.contains(secret), "{secret} was printed");
    }
}

/// The binary with a fresh key store, asked to read [since, until]: (the answer, the calls seen).
fn read_with_fake<F>(since: i64, until: i64, answer: F) -> (Value, Vec<Seen>)
where
    F: Fn(&str, &Value) -> Reply + Send + 'static,
{
    let ccs = tempfile::tempdir().unwrap();
    let (base, seen) = fake(answer);
    let run = |args: &[&str], input: &str| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_aac-cursor-account"))
            .args(args)
            .args(["--ccs-dir", ccs.path().to_str().unwrap()])
            .env("AAC_CURSOR_API_BASE", &base)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        serde_json::from_slice::<Value>(&out.stdout).unwrap()
    };
    assert_eq!(run(&["key", "put"], &format!("{KEY}\n"))["ok"], true);
    let read = run(&["read"], &json!({"sinceMs": since, "untilMs": until}).to_string());
    let calls = seen.lock().unwrap().clone();
    (read, calls)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

const DAY: i64 = 86_400_000;

/// The plan answer as Cursor gave it on 2026-11-20 (field names recorded; values synthetic): a
/// cycle from `start` to `end`, `total` percent used.
fn recorded_plan(start: i64, end: i64, total: f64) -> String {
    json!({"billingCycleStart": start.to_string(), "billingCycleEnd": end.to_string(),
           "planUsage": {"totalPercentUsed": total, "autoPercentUsed": total / 2.0,
                         "remainingBonus": "0", "bonusTooltip": "synthetic"},
           "spendLimitUsage": {"limitType": "synthetic"}, "displayMessage": "synthetic",
           "displayThreshold": 50, "enabled": true, "autoBucketModels": [],
           "autoModelSelectedDisplayMessage": "synthetic", "namedModelSelectedDisplayMessage": "synthetic"})
    .to_string()
}

const EVENTS: &str = "/aiserver.v1.DashboardService/GetFilteredUsageEvents";
const PLAN: &str = "/aiserver.v1.DashboardService/GetCurrentPeriodUsage";

/// A fake that answers the events call with Cursor's recorded `{}` and the plan call with `plan`
/// (status, body).
fn empty_events_with_plan(plan: (u16, String)) -> impl Fn(&str, &Value) -> Reply + Send + 'static {
    move |path, _| {
        if let Some(answer) = exchange_ok(path) {
            return answer;
        }
        match path {
            EVENTS => (200, vec![], "{}".to_string()),
            PLAN => (plan.0, vec![], plan.1.clone()),
            _ => (404, vec![], "{}".to_string()),
        }
    }
}

fn assert_private(read: &Value) {
    let text = read.to_string();
    for secret in [KEY, SUB, token().as_str()] {
        assert!(!text.contains(secret), "{secret} was printed");
    }
}

#[test]
fn an_empty_events_answer_while_the_plan_shows_use_in_range_is_not_visible() {
    // The 2026-11-20 read: `{}` for the production window (31 days through now) while the
    // meter reads 49% for a cycle that started inside it.
    let t = now();
    let plan = recorded_plan(t - 10 * DAY, t + 20 * DAY, 49.36);
    let (read, calls) = read_with_fake(t - 31 * DAY, t, empty_events_with_plan((200, plan)));
    assert_eq!(read["state"], "unavailable", "{read}");
    assert_eq!(read["cause"], "events_not_visible");
    assert_eq!(read["key"]["last4"], "REAL");
    assert!(read.get("events").is_none() && read.get("rows").is_none(), "{read}");
    assert_eq!(calls.iter().filter(|c| c.path == PLAN).count(), 1);
    assert_private(&read);
}

#[test]
fn an_empty_events_answer_is_unverified_when_the_plan_cannot_be_read() {
    // Review wave 82, Medium 2: a failed meter never makes the empty answer "no usage". One call.
    let t = now();
    let (read, calls) = read_with_fake(t - 31 * DAY, t, empty_events_with_plan((503, "{}".into())));
    assert_eq!(read["state"], "unavailable", "{read}");
    assert_eq!(read["cause"], "events_unverified");
    assert_eq!(calls.iter().filter(|c| c.path == PLAN).count(), 1);
    assert_private(&read);
}

#[test]
fn an_unused_plan_proves_no_usage_only_for_a_range_inside_its_cycle() {
    let t = now();
    let plan = recorded_plan(t - 10 * DAY, t + 20 * DAY, 0.0);
    // The range lies inside the metered part of the cycle: the zero covers it, so no usage.
    let (read, calls) = read_with_fake(t - 5 * DAY, t, empty_events_with_plan((200, plan.clone())));
    assert_eq!(read["state"], "read", "{read}");
    assert_eq!(read["events"], 0);
    assert_eq!(read["unmeasured"], 0);
    assert_eq!(calls.iter().filter(|c| c.path == PLAN).count(), 1);
    // Review wave 82, Medium 2: the 31-day range starts before this cycle, whose zero says
    // nothing about the days before it: unverified, never "no usage in range".
    let (read, _) = read_with_fake(t - 31 * DAY, t, empty_events_with_plan((200, plan)));
    assert_eq!(read["state"], "unavailable", "{read}");
    assert_eq!(read["cause"], "events_unverified");
}

#[test]
fn a_used_plan_cannot_place_its_use_in_a_shorter_range() {
    // Review wave 82, Low 2: a range that ends before now holds only part of the cycle; the
    // meter's use may lie after it, so the empty answer stays unverified.
    let t = now();
    let plan = recorded_plan(t - 10 * DAY, t + 20 * DAY, 49.36);
    let (read, _) = read_with_fake(t - 31 * DAY, t - 5 * DAY, empty_events_with_plan((200, plan)));
    assert_eq!(read["state"], "unavailable", "{read}");
    assert_eq!(read["cause"], "events_unverified");
}

#[test]
fn an_in_range_event_without_token_usage_is_unmeasured_and_never_asks_the_plan() {
    // Review wave 82, Low 1: Cursor showed an event, so the meter is not asked; the event's
    // missing counts read as not logged.
    let t = now();
    let plan = recorded_plan(t - 10 * DAY, t + 20 * DAY, 49.36);
    let (read, calls) = read_with_fake(t - 31 * DAY, t, move |path, _| {
        if let Some(answer) = exchange_ok(path) {
            return answer;
        }
        match path {
            EVENTS => (
                200,
                vec![],
                json!({"totalUsageEventsCount": 1,
                       "usageEventsDisplay": [{"timestamp": (t - DAY).to_string(), "model": "grok-4.7"}]})
                .to_string(),
            ),
            PLAN => (200, vec![], plan.clone()),
            _ => (404, vec![], "{}".to_string()),
        }
    });
    assert_eq!(read["state"], "read", "{read}");
    assert_eq!(read["events"], 0);
    assert_eq!(read["unmeasured"], 1);
    assert_eq!(read["eventCounts"]["droppedNoTokenUsage"], 1);
    assert!(calls.iter().all(|c| c.path != PLAN));
}

#[test]
fn a_read_with_events_never_asks_the_plan() {
    let (read, calls) = read_with_fake(WINDOW.0, WINDOW.1, |path, _| {
        if let Some(answer) = exchange_ok(path) {
            return answer;
        }
        match path {
            EVENTS => (
                200,
                vec![],
                json!({"totalUsageEventsCount": 1, "usageEventsDisplay": [event(0, "grok-4.7", Some(1.0))]})
                    .to_string(),
            ),
            PLAN => (200, vec![], recorded_plan(WINDOW.0, WINDOW.1, 49.36)),
            _ => (404, vec![], "{}".to_string()),
        }
    });
    assert_eq!(read["state"], "read", "{read}");
    assert_eq!(read["events"], 1);
    assert!(calls.iter().all(|c| c.path != PLAN));
}

const BILLING_NOW: i64 = 1_794_067_200_000; // 2026-11-07 16:00 UTC
const BILLING_HOUR: i64 = 3_600_000;

fn billing_token(exp: u64) -> String {
    // Base64url-encode the synthetic JWT claims with the same test helper as the key fixture.
    let claims = json!({"sub": "auth0|user_TEST_billing", "exp": exp}).to_string();
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut payload = String::new();
    for chunk in claims.as_bytes().chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..chunk.len() + 1 {
            payload.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    format!("e30.{payload}.fake_billing_signature")
}

fn billing_signin(root: &std::path::Path, exp: u64) -> String {
    let token = billing_token(exp);
    std::fs::write(root.join("auth.json"), json!({"accessToken": token}).to_string()).unwrap();
    token
}

fn billing_read(root: &std::path::Path, base: &str, now: i64) -> (Value, String) {
    billing_read_enabled(root, base, now, Some("1"))
}

/// The dashboard's store for these fixtures: one billing document per root, kept and saved the
/// way the caller (aac-http's Cursor reader) keeps it between passes.
static STORE: Mutex<std::collections::BTreeMap<std::path::PathBuf, Value>> =
    Mutex::new(std::collections::BTreeMap::new());

fn billing_read_enabled(root: &std::path::Path, base: &str, now: i64, enabled: Option<&str>) -> (Value, String) {
    let saved = STORE
        .lock()
        .unwrap()
        .get(root)
        .cloned()
        .unwrap_or_else(|| json!({"version": 1, "accounts": {}}));
    let (mut answer, text) = billing_run(root, base, now, enabled, Some(saved));
    if let Some(state) = answer.as_object_mut().unwrap().remove("billingState") {
        let account = state["account"].as_str().unwrap().to_owned();
        let mut store = STORE.lock().unwrap();
        let document = store
            .entry(root.to_owned())
            .or_insert_with(|| json!({"version": 1, "accounts": {}}));
        document["accounts"][account] = state["checkpoint"].clone();
    }
    (answer, text)
}

/// One reader process. `saved` is the store's document; None is a caller without a store.
fn billing_run(
    root: &std::path::Path,
    base: &str,
    now: i64,
    enabled: Option<&str>,
    saved: Option<Value>,
) -> (Value, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aac-cursor-account"));
    if let Some(enabled) = enabled {
        command.env("AAC_CURSOR_BILLING_ENABLED", enabled);
    } else {
        command.env_remove("AAC_CURSOR_BILLING_ENABLED");
    }
    let mut child = command
        .args(["read", "--ccs-dir"])
        .arg(root)
        .env("CCS_HOME", root)
        .env("AAC_CURSOR_API_BASE", base)
        .env("AAC_CURSOR_BILLING_BASE", base)
        .env("AAC_CURSOR_TEST_SIGNIN_DIR", root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut request = json!({"sinceMs": now - 31 * 24 * BILLING_HOUR, "untilMs": now});
    if let Some(saved) = saved {
        request["billingState"] = saved;
    }
    write!(child.stdin.take().unwrap(), "{request}").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
    let text = String::from_utf8(out.stdout).unwrap();
    (serde_json::from_str(&text).unwrap(), text)
}

/// Every file the reader left under the root's cache folder (the sign-in fixture lives outside it).
fn cache_files(root: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(root.join("cache"))
        .map(|dir| {
            dir.map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn billing_item(id: &str) -> Value {
    json!({
        "billingItemId": id, "product": "cursor", "chargeCategory": "usage",
        "eventTime": "2026-11-07T08:30:00.123Z",
        "displayExtras": {"model": "claude-sonnet-4-5", "userEmail": "fixture@example.invalid",
                          "displayLabel": "private label"},
        "principal": "fixture-principal",
        "usage": {"prompt-text-tokens": "17", "completion-text-tokens": 5,
                  "cached-prompt-text-tokens": "90", "cache-write-text-tokens": "11"},
        "totalTokens": 123, "pricing": {"amount": 0}
    })
}

fn billing_page(items: Vec<Value>, cursor: Option<&str>) -> Reply {
    (
        200,
        vec![],
        json!({"data": items, "pagination": {"hasMore": cursor.is_some(), "nextCursor": cursor}}).to_string(),
    )
}

#[test]
fn billing_maps_disjoint_counters_dedupes_and_never_exports_private_fields() {
    let root = tempfile::tempdir().unwrap();
    let token = billing_signin(root.path(), 4_000_000_000);
    let (base, seen) = fake(|path, _| {
        if path.contains("cursor=fixture-next") {
            let mut broken = billing_item("fixture-malformed");
            broken["usage"]["prompt-text-tokens"] = json!("-1");
            let mut other = billing_item("not-usage");
            other["chargeCategory"] = json!("subscription");
            let mut alias = billing_item("fixture-alias");
            alias["displayExtras"]["model"] = json!("Claude 4.5 Sonnet (Thinking)");
            let mut other_product = billing_item("not-cursor");
            other_product["product"] = json!("other");
            return billing_page(
                vec![billing_item("fixture-id"), broken, other, alias, other_product],
                None,
            );
        }
        billing_page(vec![billing_item("fixture-id")], Some("fixture-next"))
    });
    let (answer, output) = billing_read(root.path(), &base, BILLING_NOW);
    assert_eq!(answer["state"], "read");
    assert_eq!(answer["events"], 2);
    assert_eq!(answer["unmeasured"], 1);
    assert_eq!(
        answer["rows"][0],
        json!({"h":"2026-11-07 08:00","m":"claude-sonnet-4-5",
        "i":34,"o":10,"cr":180,"cw":22,"n":2,"cents":0.0,"uncosted":2})
    );
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|s| s.path.contains("cursor=fixture-next"))
    );
    for private in [
        &token,
        "WorkosCursorSessionToken",
        "user_TEST_billing",
        "fixture-id",
        "fixture@example.invalid",
        "fixture-principal",
        "private label",
        "fixture-next",
    ] {
        assert!(!output.contains(private));
    }
}

#[test]
fn billing_expiry_never_calls_and_auth_refusal_never_reports_zero() {
    let root = tempfile::tempdir().unwrap();
    billing_signin(root.path(), 1);
    let (base, seen) = fake(|_, _| (401, vec![], "{}".into()));
    let (expired, _) = billing_read(root.path(), &base, BILLING_NOW);
    assert_eq!(expired, json!({"state": "unavailable", "cause": "signin_needed"}));
    assert!(seen.lock().unwrap().is_empty());
    billing_signin(root.path(), 4_000_000_000);
    let (refused, _) = billing_read(root.path(), &base, BILLING_NOW);
    assert_eq!(refused, expired);
    assert_eq!(seen.lock().unwrap().len(), 1);
    let (redirect, seen) = fake(|_, _| (302, vec![("Location", "/api/auth/login".into())], "{}".into()));
    assert_eq!(billing_read(root.path(), &redirect, BILLING_NOW).0, expired);
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn billing_repeated_cursor_keeps_the_answer_unavailable() {
    let root = tempfile::tempdir().unwrap();
    billing_signin(root.path(), 4_000_000_000);
    let (base, seen) = fake(|_, _| billing_page(vec![], Some("repeated")));
    assert_eq!(
        billing_read(root.path(), &base, BILLING_NOW).0,
        json!({"state": "unavailable", "cause": "bad_response"})
    );
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[test]
fn billing_first_fill_stops_at_twenty_pages_without_publishing_a_partial_window() {
    let root = tempfile::tempdir().unwrap();
    billing_signin(root.path(), 4_000_000_000);
    let page = std::sync::atomic::AtomicUsize::new(0);
    let (base, seen) = fake(move |_, _| {
        let next = page.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        billing_page(vec![billing_item("fixture-id")], Some(&next.to_string()))
    });
    let (answer, _) = billing_read(root.path(), &base, BILLING_NOW);
    assert_eq!(answer["state"], "unavailable");
    assert_eq!(answer["cause"], "backfill_pending");
    assert!(answer["rows"].is_null());
    assert_eq!(seen.lock().unwrap().len(), 20);
    let (resume, seen) = fake(|_, _| billing_page(vec![billing_item("fixture-id")], None));
    let resumed = billing_read(root.path(), &resume, BILLING_NOW + 600_000).0;
    assert_eq!(resumed["events"], 1);
    assert_eq!(resumed["coverageFrom"], "2026-11-07T04:00:00.000Z");
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn billing_resumes_history_and_replaces_recent_hours_wholesale() {
    let root = tempfile::tempdir().unwrap();
    billing_signin(root.path(), 4_000_000_000);
    let (base, seen) = fake(|path, _| {
        let mut item = billing_item("fixture-late");
        item["eventTime"] = json!("2026-11-07T15:30:00.123Z");
        if path.contains("startTime=2026-11-07T10") {
            item["usage"]["prompt-text-tokens"] = json!(30);
        }
        if path.contains("endTime=2026-11-07T16") {
            return billing_page(vec![item], None);
        }
        billing_page(vec![], None)
    });
    let (first, _) = billing_read(root.path(), &base, BILLING_NOW);
    let first_calls = seen.lock().unwrap().len();
    assert_eq!(first["rows"][0]["i"], 17);
    assert_eq!(first["newestItemAt"], 1_794_065_400_123_i64);
    let (next, _) = billing_read(root.path(), &base, BILLING_NOW + 600_000);
    assert_eq!(next["rows"][0]["i"], 30);
    assert_eq!(next["events"], 1);
    assert!(next["coverageFrom"].as_str() < first["coverageFrom"].as_str());
    assert!(
        seen.lock().unwrap()[first_calls]
            .path
            .contains("startTime=2026-11-07T10")
    );
}

#[test]
fn billing_caps_the_combined_recent_read_and_historical_attempt_at_forty_pages() {
    let root = tempfile::tempdir().unwrap();
    billing_signin(root.path(), 4_000_000_000);
    let (base, _) = fake(|_, _| billing_page(vec![], None));
    let first = billing_read(root.path(), &base, BILLING_NOW).0;
    let page = std::sync::atomic::AtomicUsize::new(0);
    let (base, seen) = fake(move |path, _| {
        let index = page.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if index == 24 {
            let mut item = billing_item("new-recent-item");
            item["eventTime"] = json!("2026-11-07T15:30:00.123Z");
            return billing_page(vec![item], None);
        }
        assert!(path.contains("query-billing-items"));
        billing_page(vec![], Some(&index.to_string()))
    });
    let read = billing_read(root.path(), &base, BILLING_NOW + 600_000).0;
    assert_eq!(read["state"], "read");
    assert_eq!(read["coverageFrom"], first["coverageFrom"]);
    assert_eq!(read["events"], 1);
    assert_eq!(read["pages"], 40);
    assert_eq!(seen.lock().unwrap().len(), 40);
}

#[test]
fn billing_uses_the_latest_signin_and_leaves_the_desktop_database_untouched() {
    let root = tempfile::tempdir().unwrap();
    let cli = billing_signin(root.path(), 4_000_000_000);
    let desktop = billing_token(4_100_000_000);
    let db_path = root.path().join("Cursor/User/globalStorage/state.vscdb");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute_batch("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)")
        .unwrap();
    db.execute("INSERT INTO ItemTable VALUES ('cursorAuth/accessToken', ?)", [&desktop])
        .unwrap();
    drop(db);
    let original = std::fs::read(&db_path).unwrap();
    let expected = desktop.clone();
    let (base, seen) = fake(move |path, _| {
        assert!(path.contains("scope=user"));
        billing_page(vec![], None)
    });
    let (answer, output) = billing_read(root.path(), &base, BILLING_NOW);
    assert_eq!(answer["state"], "read");
    assert_eq!(
        seen.lock().unwrap()[0].cookie,
        format!("WorkosCursorSessionToken=user_TEST_billing%3A%3A{expected}")
    );
    assert_eq!(std::fs::read(&db_path).unwrap(), original);
    assert!(!output.contains(&cli) && !output.contains(&desktop));
}

#[test]
fn billing_review_clock_ahead_then_corrected_does_not_skip_hours() {
    let root = tempfile::tempdir().unwrap();
    billing_signin(root.path(), 4_000_000_000);
    let (empty, _) = fake(|_, _| billing_page(vec![], None));
    assert_eq!(billing_read(root.path(), &empty, BILLING_NOW).0["state"], "read");

    // While the clock is six hours ahead, Cursor agrees that the future hours are empty.
    let ahead = BILLING_NOW + 6 * BILLING_HOUR;
    assert_eq!(
        billing_read(root.path(), &empty, ahead).0["coverageUntil"],
        "2026-11-07T22:00:00.000Z"
    );

    let (corrected, seen) = fake(|_, _| billing_page(vec![], None));
    let until = BILLING_NOW + 600_000;
    let read = billing_read(root.path(), &corrected, until).0;
    let calls_after_correction = seen.lock().unwrap().len();

    // 17:30 happened only after the clock was corrected; at 23:00 it must still be found.
    let (later, _) = fake(|_, _| {
        let mut item = billing_item("review-1730");
        item["eventTime"] = json!("2026-11-07T17:30:00.000Z");
        billing_page(vec![item], None)
    });
    let resumed = billing_read(root.path(), &later, BILLING_NOW + 7 * BILLING_HOUR).0;
    assert_eq!(resumed["events"], 1);
    assert_eq!(resumed["rows"][0]["h"], "2026-11-07 17:00");
    assert!(calls_after_correction > 0);
    assert_eq!(read["coverageUntil"], "2026-11-07T16:10:00.000Z");
    assert!(read["readAt"].as_i64().unwrap() <= until);
}

#[test]
fn billing_review_disabled_or_absent_enable_never_contacts_cursor() {
    let root = tempfile::tempdir().unwrap();
    billing_signin(root.path(), 4_000_000_000);
    let (base, seen) = fake(|_, _| billing_page(vec![], None));
    for enable in [None, Some("0")] {
        assert_eq!(
            billing_read_enabled(root.path(), &base, BILLING_NOW, enable).0,
            json!({"state": "no_key"})
        );
    }
    assert!(seen.lock().unwrap().is_empty());
    assert!(!root.path().join("cache/cursor-billing-v1.sqlite").exists());
}

#[test]
fn billing_review_a_late_item_five_hours_old_counts_once_across_pages_and_passes() {
    let root = tempfile::tempdir().unwrap();
    billing_signin(root.path(), 4_000_000_000);
    let (empty, _) = fake(|_, _| billing_page(vec![], None));
    assert_eq!(billing_read(root.path(), &empty, BILLING_NOW).0["events"], 0);
    let (late, seen) = fake(|path, _| {
        if !path.contains("endTime=2026-11-07T16") {
            return billing_page(vec![], None);
        }
        let mut item = billing_item("five-hours-late");
        item["eventTime"] = json!("2026-11-07T11:15:00.000Z");
        let next = (!path.contains("cursor=repeat-page")).then_some("repeat-page");
        billing_page(vec![item], next)
    });
    for offset in [600_000, 1_200_000] {
        let read = billing_read(root.path(), &late, BILLING_NOW + offset).0;
        assert_eq!(read["events"], 1);
        assert_eq!(read["rows"][0]["h"], "2026-11-07 11:00");
    }
    assert!(seen.lock().unwrap()[0].path.contains("startTime=2026-11-07T10"));
}

#[test]
fn billing_review_unmapped_display_model_keeps_its_tokens_and_sets_the_notice() {
    let root = tempfile::tempdir().unwrap();
    billing_signin(root.path(), 4_000_000_000);
    let (base, _) = fake(|_, _| {
        let mut item = billing_item("display-model");
        item["displayExtras"]["model"] = json!("Example Model 5.2 (Extended Thinking)");
        billing_page(vec![item], None)
    });
    let first = billing_read(root.path(), &base, BILLING_NOW).0;
    assert_eq!(first["events"], 1);
    assert_eq!(first["unmeasured"], 0);
    assert_eq!(first["unrecognizedModels"], 1);
    assert_eq!(first["rows"][0]["m"], "example-model-5-2-extended-thinking");
    assert_eq!(first["rows"][0]["i"], 17);
    assert_eq!(first["rows"][0]["cr"], 90);
    let (empty, _) = fake(|_, _| billing_page(vec![], None));
    let cached = billing_read(root.path(), &empty, BILLING_NOW + 600_000).0;
    assert_eq!(cached["unrecognizedModels"], 1); // Stored 08:30 lies outside the 6 h replay.
}

#[test]
fn billing_creates_no_sqlite_file_and_hands_its_state_to_the_store() {
    let root = tempfile::tempdir().unwrap();
    billing_signin(root.path(), 4_000_000_000);
    let (base, _) = fake(|_, _| billing_page(vec![billing_item("store-item")], None));
    let (answer, output) = billing_read(root.path(), &base, BILLING_NOW);
    assert_eq!(answer["state"], "read", "{answer}");
    assert!(!root.path().join("cache/cursor-billing-v1.sqlite").exists());
    assert_eq!(cache_files(root.path()), Vec::<String>::new());
    let store = STORE.lock().unwrap();
    let accounts = store[root.path()]["accounts"].as_object().unwrap();
    assert_eq!(accounts.len(), 1);
    let checkpoint = accounts.values().next().unwrap();
    assert_eq!(checkpoint["through"], BILLING_NOW);
    assert_eq!(checkpoint["read_at"], BILLING_NOW);
    // The saved cells hold aggregated hours only, never an item id, a cookie or an email.
    let text = checkpoint.to_string();
    for private in ["store-item", "WorkosCursorSessionToken", "fixture@example.invalid"] {
        assert!(!text.contains(private) && !output.contains(private));
    }
}

#[test]
fn billing_state_survives_a_restart_from_the_store() {
    let root = tempfile::tempdir().unwrap();
    billing_signin(root.path(), 4_000_000_000);
    let (empty, _) = fake(|_, _| billing_page(vec![], None));
    let first = billing_read(root.path(), &empty, BILLING_NOW).0;
    assert_eq!(first["coverageFrom"], "2026-11-06T16:00:00.000Z");
    // A restart keeps only the store: whatever the reader left in the cache folder is gone.
    let _ = std::fs::remove_dir_all(root.path().join("cache"));
    let (base, seen) = fake(|_, _| billing_page(vec![], None));
    let next = billing_read(root.path(), &base, BILLING_NOW + 600_000).0;
    assert_eq!(next["state"], "read", "{next}");
    // The resumed pass re-reads only the 6 h overlap, then extends history one more day.
    let calls = seen.lock().unwrap();
    assert!(calls[0].path.contains("startTime=2026-11-07T10"), "{}", calls[0].path);
    assert_eq!(next["coverageFrom"], "2026-11-05T16:00:00.000Z");
    assert_eq!(next["coverageUntil"], "2026-11-07T16:10:00.000Z");
}

#[test]
fn billing_without_a_store_reads_fresh_each_pass_and_keeps_no_file() {
    let root = tempfile::tempdir().unwrap();
    billing_signin(root.path(), 4_000_000_000);
    let (base, seen) = fake(|_, _| billing_page(vec![], None));
    for offset in [0, 600_000] {
        let (answer, _) = billing_run(root.path(), &base, BILLING_NOW + offset, Some("1"), None);
        assert_eq!(answer["state"], "read", "{answer}");
        assert_eq!(answer["coverageFrom"], "2026-11-06T16:00:00.000Z");
    }
    let calls = seen.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(calls[1].path.contains("startTime=2026-11-06T16"), "{}", calls[1].path);
    assert_eq!(cache_files(root.path()), Vec::<String>::new());
}
