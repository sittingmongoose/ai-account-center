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
    // event, one with no tokenUsage at all (outside), one outside the window, and two whose
    // tokenUsage is empty or incomplete ({}, and inputTokens only). The last two must read as
    // not logged (counted separately), never as an hour of zeros.
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
    assert_eq!(unmeasured, 2);
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
