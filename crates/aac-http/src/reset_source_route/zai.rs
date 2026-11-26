//! Z.ai dashboard-key reads and the single guarded send.
use super::{Res, SourceContext, Target, local_computer};
use crate::account_usage::sources::{Credential, Sources, read_registry};
use aac_files::read::{self, Ancestors, Mode, Owner, Read, ReadPolicy};
use aac_status::reset::{Identity, Store};
use aac_status::reset_rules::rules;
use aac_status::reset_zai::{CLAIM_PROVEN, METHOD, PackList, apply_quota};
use reqwest::blocking::{Client, Response};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue, RETRY_AFTER};
use serde_json::{Value, json};
use std::io::Read as _;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const ORIGIN: &str = "https://api.z.ai";
const LIST: &str = "/api/biz/customer-package-reset/list?targetType=PERSONAL";
const QUOTA: &str = "/api/monitor/usage/quota/limit";
const USE: &str = "/api/biz/customer-package-reset/use";
const UNAVAILABLE: &str = "inventory-unavailable";
const RESPONSE_CAP: u64 = 512 * 1024;
const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) fn resolve(ctx: &SourceContext, account: &str) -> Res<Target> {
    let Sources::V2(sources) = read_registry(&ctx.ccs).map_err(|_| UNAVAILABLE)? else {
        return Err("provider-read-only");
    };
    let source = sources
        .into_iter()
        .find(|source| {
            source.id == account
                && source.provider == "zai"
                && source.platform == "ubuntu"
                && source.ssh_host.is_none()
        })
        .ok_or("provider-read-only")?;
    let Credential::AacKey { key_id } = source.credential else {
        return Err("provider-read-only");
    };
    let row = aac_files::registry::row("account-keys").map_err(|_| UNAVAILABLE)?;
    let key_path = ctx.ccs.join(
        row.file(Some(&format!("zai-{key_id}.json")))
            .map_err(|_| UNAVAILABLE)?,
    );
    Ok(Target::Zai {
        key_id,
        key_path,
        label: source.label.unwrap_or_else(|| "Z.ai".into()),
    })
}

struct Key {
    secret: String,
    binding: String,
}

fn capture(ctx: &SourceContext, path: &Path, id: &str) -> Res<Key> {
    if !path.parent().is_some_and(read::private_directory) {
        return Err("credential-host-unavailable");
    }
    let row = aac_files::registry::row("account-keys").map_err(|_| UNAVAILABLE)?;
    let policy = ReadPolicy::capped(row.max_bytes)
        .mode(Mode::Private)
        .owner(Owner::Mine)
        .ancestors(Ancestors::NoLinkBelow(&ctx.ccs));
    let Read::Bytes(bytes, _) = read::read(path, &policy) else {
        return Err("credential-host-unavailable");
    };
    let record: Value =
        serde_json::from_slice(&bytes).map_err(|_| "credential-host-unavailable")?;
    let secret = record["secret"]
        .as_str()
        .filter(|s| (8..=512).contains(&s.len()) && s.bytes().all(|b| (0x21..=0x7e).contains(&b)))
        .ok_or("credential-host-unavailable")?;
    if record["version"] != 1 || record["provider"] != "zai" || record["keyId"] != id {
        return Err("credential-host-unavailable");
    }
    Ok(Key {
        secret: secret.to_owned(),
        binding: crate::antigravity_runtime::sha256_hex(&bytes),
    })
}

fn now_ms() -> Res<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|time| time.as_millis().try_into().ok())
        .ok_or("clock")
}

struct Api<'a> {
    origin: &'a str,
    client: Client,
}

impl<'a> Api<'a> {
    fn new(origin: &'a str) -> Res<Self> {
        let client = Client::builder()
            .timeout(super::CLAIM_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .build()
            .map_err(|_| UNAVAILABLE)?;
        Ok(Self { origin, client })
    }

    fn list(&self, key: &Key, timeout: Duration) -> Res<PackList> {
        let response = self
            .client
            .get(format!("{}{LIST}", self.origin))
            .bearer_auth(&key.secret)
            .timeout(timeout)
            .send()
            .map_err(|_| UNAVAILABLE)?;
        PackList::parse(&successful_body(response)?)
    }

    fn quota(&self, key: &Key) -> Res<Value> {
        let mut authorization = HeaderValue::from_str(&key.secret).map_err(|_| UNAVAILABLE)?;
        authorization.set_sensitive(true);
        let response = self
            .client
            .get(format!("{}{QUOTA}", self.origin))
            .header(AUTHORIZATION, authorization)
            .header(CONTENT_TYPE, "application/json")
            .send()
            .map_err(|_| UNAVAILABLE)?;
        successful_body(response)
    }

    fn claim(
        &self,
        key: &Key,
        organization: &str,
        project: &str,
        body: &Value,
        mark: impl FnOnce() -> Res<()>,
    ) -> Res<Value> {
        let request = self
            .client
            .post(format!("{}{USE}", self.origin))
            .bearer_auth(&key.secret)
            .header("bigmodel-organization", organization)
            .header("bigmodel-project", project)
            .header(CONTENT_TYPE, "application/json;charset=UTF-8")
            .body(body.to_string())
            .build()
            .map_err(|_| "not-sent")?;
        mark().map_err(|_| "ambiguous")?;
        let reply = self.client.execute(request).map_err(|_| "ambiguous")?;
        if reply.status() == 429 {
            return Ok(json!({"outcome":"rate-limited","retryAfterMs":retry_after(&reply)}));
        }
        let reset = successful_body(reply).is_ok_and(|body| body["success"] == true);
        Ok(json!({"outcome":if reset { "reset" } else { "ambiguous" }}))
    }
}

fn successful_body(response: Response) -> Res<Value> {
    if !response.status().is_success() {
        return Err(UNAVAILABLE);
    }
    let mut bytes = Vec::new();
    response
        .take(RESPONSE_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| UNAVAILABLE)?;
    if bytes.len() as u64 > RESPONSE_CAP {
        return Err(UNAVAILABLE);
    }
    serde_json::from_slice(&bytes).map_err(|_| UNAVAILABLE)
}

fn retry_after(reply: &Response) -> Option<i64> {
    let text = reply.headers().get(RETRY_AFTER)?.to_str().ok()?;
    if let Ok(seconds) = text.parse::<u64>() {
        return Some(seconds.clamp(1, 86_400) as i64 * 1000);
    }
    let at = chrono::DateTime::parse_from_rfc2822(text)
        .ok()?
        .timestamp_millis();
    Some(at.saturating_sub(now_ms().ok()?).clamp(1_000, 86_400_000))
}

pub(super) fn read(ctx: &SourceContext, db: &Path, account: &str, target: &Target) -> Res<Value> {
    read_from(ctx, db, account, target, &Api::new(ORIGIN)?)
}

fn read_from(
    ctx: &SourceContext,
    db: &Path,
    account: &str,
    target: &Target,
    api: &Api<'_>,
) -> Res<Value> {
    let Target::Zai {
        key_id,
        key_path,
        label,
    } = target
    else {
        return Err("shape");
    };
    let mut store = Store::open(db)?;
    let (key, ticket) = store.capture_read(account, || capture(ctx, key_path, key_id))?;
    let started = now_ms()?;
    let list = api.list(&key, super::CLAIM_TIMEOUT)?;
    let mut snapshot = list.snapshot(
        key_id,
        &local_computer(&ctx.hostname),
        key.binding.clone(),
        started,
    );
    snapshot.account_label.clone_from(label);
    let quota = api.quota(&key);
    if capture(ctx, key_path, key_id)?.binding != key.binding {
        return Err("identity-changed");
    }
    store.bind_zai_read(account, &ticket, &snapshot.identity)?;
    snapshot.read_ticket = Some(ticket);
    apply_quota(&mut snapshot, &quota.map_err(|_| "zai-quota-unavailable")?)
        .map_err(|_| "zai-quota-unavailable")?;
    serde_json::to_value(snapshot).map_err(|_| "shape")
}

pub(super) fn send(ctx: &SourceContext, db: &Path, account: &str, write: &Value) -> Value {
    if !CLAIM_PROVEN {
        return json!({"outcome":"not-sent","reason":"claim-unproven"});
    }
    let send = || {
        let target = resolve(ctx, account).map_err(|_| "not-sent")?;
        let api = Api::new(ORIGIN).map_err(|_| "not-sent")?;
        send_to(ctx, &target, write, &api, || {
            Store::open_with_clock(db, |_| now_ms())?.mark_zai_send(write, 0)
        })
    };
    send().unwrap_or_else(|outcome| json!({"outcome":outcome}))
}

fn send_to(
    ctx: &SourceContext,
    target: &Target,
    write: &Value,
    api: &Api<'_>,
    mark: impl FnOnce() -> Res<()>,
) -> Res<Value> {
    let Target::Zai {
        key_id, key_path, ..
    } = target
    else {
        return Err("not-sent");
    };
    let identity: Identity =
        serde_json::from_value(write["identity"].clone()).map_err(|_| "not-sent")?;
    let key = capture(ctx, key_path, key_id).map_err(|_| "not-sent")?;
    if key.binding != write["sourceBinding"] || write["method"] != METHOD {
        return Ok(json!({"outcome":"not-sent","reason":"identity-changed"}));
    }
    let list = api.list(&key, PREFLIGHT_TIMEOUT).map_err(|_| "not-sent")?;
    let now = now_ms().map_err(|_| "not-sent")?;
    let organization = list.organization.clone();
    let project = list.project.clone();
    let snapshot = list.snapshot(
        key_id,
        &local_computer(&ctx.hostname),
        key.binding.clone(),
        now,
    );
    if identity != snapshot.identity
        || capture(ctx, key_path, key_id)
            .map_err(|_| "not-sent")?
            .binding
            != key.binding
    {
        return Ok(json!({"outcome":"not-sent","reason":"identity-changed"}));
    }
    let id = write["body"]["recordId"]
        .as_u64()
        .ok_or("not-sent")?
        .to_string();
    let grant = snapshot.grants.iter().find(|g| {
        g.id.as_deref() == Some(&id)
            && g.usable
            && g.left == 1
            && g.kind == "weekly"
            && g.expires_at_ms.is_none_or(|expiry| expiry > now)
    });
    let attempt = write["attemptKey"].as_str().ok_or("not-sent")?;
    let fresh_body = grant
        .and_then(|g| rules("zai")?.write(&snapshot, g, attempt).ok())
        .map(|(_, body)| body);
    if fresh_body.as_ref() != Some(&write["body"]) {
        return Ok(json!({"outcome":"not-sent","reason":"grant-changed"}));
    }
    api.claim(&key, &organization, &project, &write["body"], mark)
}

#[cfg(all(unix, test))]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::{
        fs,
        io::{BufRead, BufReader, Write as _},
        net::TcpListener,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        thread::{self, JoinHandle},
    };

    const ACCOUNT: &str = "zai:acct:00000001";
    const SLOT: &str = "12345678";

    struct Fixture {
        root: tempfile::TempDir,
        ctx: SourceContext,
        path: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
            let directory = root.path().join("account-usage/keys");
            fs::create_dir_all(&directory).unwrap();
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
            let path = directory.join(format!("zai-{SLOT}.json"));
            let ctx = SourceContext::from_env(root.path(), None);
            let f = Self { root, ctx, path };
            f.registry(None);
            f.key("fixture-key-old");
            f
        }

        fn registry(&self, ssh_host: Option<&str>) {
            let path = self.root.path().join("account-usage-accounts.json");
            let value = json!({"version":2,"accounts":[{"id":ACCOUNT,"provider":"zai","platform":if ssh_host.is_some() {"windows"} else {"ubuntu"},
                "sshHost":ssh_host,"label":"Fixture plan","credential":{"kind":"aac-key","keyId":SLOT}}]});
            fs::write(&path, value.to_string()).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }

        fn key(&self, secret: &str) {
            let value = json!({"version":1,"provider":"zai","keyId":SLOT,"secret":secret});
            fs::write(&self.path, value.to_string()).unwrap();
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600)).unwrap();
        }

        fn write(&self) -> Value {
            let snap = PackList::parse(&packs()).unwrap().snapshot(
                SLOT,
                &local_computer(&self.ctx.hostname),
                capture(&self.ctx, &self.path, SLOT).unwrap().binding,
                now_ms().unwrap(),
            );
            let attempt = "b".repeat(64);
            let (method, body) = rules("zai")
                .unwrap()
                .write(&snap, &snap.grants[0], &attempt)
                .unwrap();
            json!({"routeAccount":ACCOUNT,"method":method,"body":body,"attemptKey":attempt,
                "sourceBinding":snap.source_binding,"identity":snap.identity})
        }
    }

    fn packs() -> Value {
        json!({"success":true,"data":{"targetType":"PERSONAL","customerId":17,
            "organizationId":"org-x","projectId":"project-x","fiveHourResets":[],
            "weekResets":[{"recordId":251,"grantType":"GLOBAL","available":true}]}})
    }

    fn mock(replies: Vec<(String, String)>) -> (String, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let worker = thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, body) in replies {
                let deadline = std::time::Instant::now() + Duration::from_secs(3);
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                && std::time::Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => return requests,
                    }
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut reader = BufReader::new(&mut socket);
                let mut request = String::new();
                let mut len = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = value.trim().parse::<usize>().unwrap();
                    }
                    request.push_str(&line);
                }
                let mut bytes = vec![0; len];
                reader.read_exact(&mut bytes).unwrap();
                request.push_str(&String::from_utf8(bytes).unwrap());
                requests.push(request);
                if status == "timeout" {
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
                if !status.is_empty() {
                    write!(socket, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                }
            }
            requests
        });
        (origin, worker)
    }

    fn ok(value: Value) -> (String, String) {
        ("200 OK".into(), value.to_string())
    }

    #[test]
    fn dashboard_read_captures_the_key_ticket_and_uses_both_gets() {
        let f = Fixture::new();
        let target = resolve(&f.ctx, ACCOUNT).unwrap();
        let (url, server) = mock(vec![
            ok(packs()),
            ok(
                json!({"success":true,"data":{"limits":[{"type":"TOKENS_LIMIT","unit":6,"number":1,"percentage":97}]}}),
            ),
        ]);
        let db = f.root.path().join("claims.sqlite");
        let api = Api::new(&url).unwrap();
        let value = read_from(&f.ctx, &db, ACCOUNT, &target, &api).unwrap();
        assert_eq!(value["identity"]["account"], "17");
        assert_eq!(value["windows"]["seven_day"].as_f64(), Some(97.));
        assert_eq!(value["readTicket"]["captureVersion"], 1);
        assert_eq!(value["accountLabel"], "Fixture plan");
        assert!(!value.to_string().contains("fixture-key-old"));
        let requests = server.join().unwrap();
        assert!(requests[0].starts_with(&format!("GET {LIST} ")));
        assert!(requests[0].contains("authorization: Bearer fixture-key-old"));
        assert!(requests[1].starts_with(&format!("GET {QUOTA} ")));
        assert!(requests[1].contains("authorization: fixture-key-old"));
        let (url, server) = mock(vec![ok(json!({"success":false}))]);
        assert_eq!(
            read_from(&f.ctx, &db, ACCOUNT, &target, &Api::new(&url).unwrap()),
            Err(UNAVAILABLE)
        );
        assert_eq!(server.join().unwrap().len(), 1);
        f.registry(Some("fixture-host"));
        assert_eq!(resolve(&f.ctx, ACCOUNT).err(), Some("provider-read-only"));
    }

    #[test]
    fn post_is_once_and_distinguishes_success_rate_limit_and_ambiguous() {
        let f = Fixture::new();
        let target = resolve(&f.ctx, ACCOUNT).unwrap();
        let write = f.write();
        for (reply, outcome, retry) in [
            (ok(json!({"success":true})), "reset", None),
            (
                (
                    "429 Too Many Requests\r\nRetry-After: 17".into(),
                    "{}".into(),
                ),
                "rate-limited",
                Some(17_000),
            ),
            (
                ("500 Internal Server Error".into(), "{}".into()),
                "ambiguous",
                None,
            ),
            (ok(json!({"success":false})), "ambiguous", None),
            (("timeout".into(), String::new()), "ambiguous", None),
        ] {
            let timeout = reply.0 == "timeout";
            let (url, server) = mock(vec![ok(packs()), reply]);
            let mut api = Api::new(&url).unwrap();
            if timeout {
                api.client = Client::builder()
                    .timeout(Duration::from_millis(250))
                    .no_proxy()
                    .build()
                    .unwrap();
            }
            let mut marked = false;
            let result = send_to(&f.ctx, &target, &write, &api, || {
                marked = true;
                Ok(())
            })
            .unwrap_or_else(|outcome| json!({"outcome":outcome}));
            assert!(marked);
            assert_eq!(result["outcome"], outcome);
            assert_eq!(result["retryAfterMs"].as_i64(), retry);
            let requests = server.join().unwrap();
            assert_eq!(requests.len(), 2);
            let post = &requests[1];
            assert!(post.starts_with(&format!("POST {USE} ")));
            assert!(post.contains("bigmodel-organization: org-x"));
            assert!(post.contains("bigmodel-project: project-x"));
            assert!(post.ends_with(&write["body"].to_string()));
        }
    }

    #[test]
    fn lost_grant_other_customer_and_swapped_key_refuse_before_post() {
        let f = Fixture::new();
        let target = resolve(&f.ctx, ACCOUNT).unwrap();
        let write = f.write();
        for (field, value) in [("weekResets", json!([])), ("customerId", json!(91))] {
            let mut reply = packs();
            reply["data"][field] = value;
            let (url, server) = mock(vec![ok(reply)]);
            let result = send_to(&f.ctx, &target, &write, &Api::new(&url).unwrap(), || {
                panic!("must not mark")
            })
            .unwrap();
            assert_eq!(result["outcome"], "not-sent");
            assert_eq!(server.join().unwrap().len(), 1);
        }
        f.key("fixture-key-new");
        let (url, server) = mock(vec![]);
        let result = send_to(&f.ctx, &target, &write, &Api::new(&url).unwrap(), || {
            panic!("must not mark")
        })
        .unwrap();
        assert_eq!(result["outcome"], "not-sent");
        assert_eq!(result["reason"], "identity-changed");
        assert!(server.join().unwrap().is_empty());
        let top = send(&f.ctx, &f.path, ACCOUNT, &write);
        assert_eq!(top["outcome"], "not-sent");
        assert_eq!(top["reason"], "identity-changed");
    }
}
