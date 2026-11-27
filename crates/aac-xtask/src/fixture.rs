//! Native synthetic Status and Scoreboard fixtures. Seed documents preserve the JS oracle's
//! insertion order and bytes; only clock values and the requested root are interpolated.
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, SecondsFormat, Utc};
use ed25519_dalek::{
    Signer, SigningKey,
    pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey},
};
use rand_core::OsRng;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const BASE: i64 = 1_793_534_400_000;
const ROOT: &str = "/__AAC_FIXTURE_ROOT__";

/// Options shared by the browser and memory fixture generators.
#[derive(Default)]
pub struct StatusOptions {
    /// Optional private answer store (no store is accessed by default).
    pub ccs_dir: Option<PathBuf>,
    /// Add intentionally malformed documents.
    pub with_problems: bool,
    /// Add objection, action, judgement and sample cards.
    pub with_kinds: bool,
    /// Try ffmpeg for the short video and poster.
    pub with_video: bool,
    /// Suppress the quota-watch document.
    pub no_quota: bool,
    /// Unix milliseconds; the current clock is used when omitted.
    pub now: Option<i64>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn stamp(ms: i64, millis: bool) -> Result<String> {
    Ok(DateTime::from_timestamp_millis(ms)
        .ok_or("fixture time out of range")?
        .to_rfc3339_opts(
            if millis {
                SecondsFormat::Millis
            } else {
                SecondsFormat::Secs
            },
            true,
        ))
}
fn render(text: &str, root: &Path, now: i64) -> Result<String> {
    let encoded_root = serde_json::to_string(&root.to_string_lossy())?;
    let text = text.replace(ROOT, &encoded_root[1..encoded_root.len() - 1]);
    let re = regex::Regex::new(
        r"2026-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9](?:\.[0-9]{3})?Z",
    )?;
    let mut output = String::new();
    let mut end = 0;
    for m in re.find_iter(&text) {
        output.push_str(&text[end..m.start()]);
        let old = DateTime::parse_from_rfc3339(m.as_str())?.timestamp_millis();
        output.push_str(&stamp(
            now.checked_add(old - BASE).ok_or("fixture time overflow")?,
            m.as_str().contains('.'),
        )?);
        end = m.end();
    }
    output.push_str(&text[end..]);
    Ok(output)
}
fn write(path: &Path, body: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, body)?;
    Ok(())
}
fn json_bytes(doc: &Value) -> Result<Vec<u8>> {
    let mut out = serde_json::to_vec_pretty(doc)?;
    out.push(b'\n');
    Ok(out)
}
fn materialize(seed: &str, root: &Path, now: i64, no_quota: bool) -> Result<Value> {
    let seed: Value = serde_json::from_str(seed)?;
    for (name, body) in seed["files"].as_object().ok_or("missing fixture files")? {
        if no_quota && name == "v1/quota/report.json" {
            continue;
        }
        let bytes = if let Some(text) = body["text"].as_str() {
            text.as_bytes().to_vec()
        } else {
            STANDARD.decode(body["base64"].as_str().ok_or("invalid fixture body")?)?
        };
        let mut name = name.clone();
        let body = if let Ok(text) = std::str::from_utf8(&bytes) {
            let text = if name.starts_with("v1/catalog/") {
                text.to_string()
            } else {
                render(text, root, now)?
            };
            // Scoreboard folders are derived from the shifted run end, including older copies.
            if name.contains("/runs/") {
                let doc: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                if let Some(ended) = doc["endedAt"].as_str() {
                    let parts: Vec<_> = name.split('/').collect();
                    name = format!("{}/{}/{}", parts[..4].join("/"), &ended[..10], parts[5]);
                } else {
                    let ended = stamp(now - 3 * 3_600_000, false)?;
                    let parts: Vec<_> = name.split('/').collect();
                    name = format!("{}/{}/{}", parts[..4].join("/"), &ended[..10], parts[5]);
                }
            }
            text.into_bytes()
        } else {
            bytes
        };
        write(&root.join(name), &body)?;
    }
    for (name, offset) in seed["mtimes"].as_object().ok_or("missing mtimes")? {
        let ms = now
            .checked_add(offset.as_i64().ok_or("bad mtime")?)
            .ok_or("mtime overflow")?;
        let time = if ms >= 0 {
            std::time::UNIX_EPOCH + Duration::from_millis(ms as u64)
        } else {
            std::time::UNIX_EPOCH - Duration::from_millis(ms.unsigned_abs())
        };
        fs::File::open(root.join(name))?
            .set_times(fs::FileTimes::new().set_accessed(time).set_modified(time))?;
    }
    Ok(serde_json::from_str(&render(
        &seed["result"].to_string(),
        root,
        now,
    )?)?)
}
fn canonical(v: &Value) -> Value {
    match v {
        Value::Object(o) => {
            let mut keys: Vec<_> = o.keys().collect();
            keys.sort();
            let mut map = serde_json::Map::new();
            for k in keys {
                map.insert(k.clone(), canonical(&o[k]));
            }
            Value::Object(map)
        }
        Value::Array(a) => Value::Array(a.iter().map(canonical).collect()),
        _ => v.clone(),
    }
}
fn private_file(path: &Path, bytes: &[u8], append: bool) -> Result<()> {
    let mut options = OpenOptions::new();
    options
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)?;
    Ok(())
}
fn answers(root: &Path, ccs: &Path, now: i64) -> Result<PathBuf> {
    let store = ccs.join("status");
    fs::create_dir_all(&store)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&store, fs::Permissions::from_mode(0o700))?;
    }
    let key_path = store.join("answer-key.pem");
    let key = if key_path.exists() {
        SigningKey::from_pkcs8_pem(&fs::read_to_string(&key_path)?)?
    } else {
        let key = SigningKey::generate(&mut OsRng);
        private_file(
            &key_path,
            key.to_pkcs8_pem(pkcs8::LineEnding::LF)?.as_bytes(),
            false,
        )?;
        key
    };
    let public = key.verifying_key();
    let key_id = hex(&Sha256::digest(public.to_public_key_der()?.as_bytes()))[..16].to_string();
    let pem = public.to_public_key_pem(pkcs8::LineEnding::LF)?;
    for name in ["aac-answers".to_string(), key_id.clone()] {
        write(&root.join(format!("v1/keys/{name}.pub")), pem.as_bytes())?;
    }
    let mut lines = Vec::new();
    for (id, choice, note, via, ago) in [
        (
            "mac.aac-claude-relogin--Q7",
            vec!["B"],
            "Windows gets its own runner; keep the Mac one simple.",
            "deck",
            20 * 3_600_000,
        ),
        (
            "vm.pm-closure-lead--Q1",
            vec!["A"],
            "",
            "recommended",
            3 * 3_600_000,
        ),
        (
            "vm.aac-status-lead--Q2",
            vec![],
            "Could you show both side by side first?",
            "deck",
            30 * 60_000,
        ),
    ] {
        let writer = id.split("--").next().ok_or("invalid card id")?;
        let bytes = fs::read(root.join(format!("v1/writers/{writer}/cards/{id}.json")))?;
        let card: Value = serde_json::from_slice(&bytes)?;
        let mut answer = json!({"schema":"agent-reports/answer@1","cardId":id,"cardSeq":card["seq"],"cardSha256":hex(&Sha256::digest(&bytes)),"kind":if choice.is_empty(){"note"}else{"answer"},"choice":choice,"note":note,"via":via,"answeredAt":stamp(now-ago,true)?,"answeredBy":"owner","device":"iPhone"});
        let signature = key.sign(&serde_json::to_vec(&canonical(&answer))?);
        answer["signature"] =
            json!({"alg":"ed25519","keyId":key_id,"value":STANDARD.encode(signature.to_bytes())});
        let uuid = uuid::Uuid::new_v4();
        let name = if choice.is_empty() {
            format!("{id}.note-{}.json", &uuid.simple().to_string()[..12])
        } else {
            format!("{id}.json")
        };
        write(&root.join("v1/answers").join(name), &json_bytes(&answer)?)?;
        lines.push(json!({"type":"answer","id":uuid.to_string(),"at":answer["answeredAt"],"answer":answer,"card":card}));
        for (step, detail) in [
            ("share", "Signed copy written to the share.".to_string()),
            (
                "doorbell",
                format!(
                    "Rang {} through pm-mail.",
                    card["replyTo"]["pmMail"].as_str().ok_or("missing reply")?
                ),
            ),
        ] {
            lines.push(json!({"type":"delivery","id":uuid.to_string(),"step":step,"state":"done","at":stamp(now-ago+60_000,true)?,"detail":detail}));
        }
    }
    let mut bytes = Vec::new();
    for line in lines {
        bytes.extend(serde_json::to_vec(&line)?);
        bytes.push(b'\n');
    }
    private_file(&store.join("answers.jsonl"), &bytes, true)?;
    Ok(store)
}
fn ffmpeg(args: &[&str]) -> bool {
    let Ok(mut child) = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => {}
        }
        if start.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
/// Write Status reports and optionally signed answer records.
pub fn write_status_fixture(root: &Path, options: &StatusOptions) -> Result<Value> {
    if root.join("v1/writers").exists() {
        return Err(format!(
            "{} already holds v1/writers; use an empty folder.",
            root.display()
        )
        .into());
    }
    let now = options.now.unwrap_or_else(|| Utc::now().timestamp_millis());
    let mut result = materialize(
        if options.with_kinds {
            include_str!("../fixtures/status-kinds.json")
        } else {
            include_str!("../fixtures/status.json")
        },
        root,
        now,
        options.no_quota,
    )?;
    if options.with_video {
        let images = root.join("v1/writers/vm.pm-fo-lead/images");
        let video = images.join("pip-resize.mp4");
        let poster = images.join("pip-resize.png");
        if ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=3:size=320x240:rate=15",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
            &video.to_string_lossy(),
        ]) && ffmpeg(&[
            "-i",
            &video.to_string_lossy(),
            "-frames:v",
            "1",
            &poster.to_string_lossy(),
        ]) {
            let file = root.join("v1/writers/vm.pm-fo-lead/cards/vm.pm-fo-lead--Q3.json");
            let mut card: Value = serde_json::from_slice(&fs::read(&file)?)?;
            card["sections"].as_array_mut().ok_or("missing sections")?.insert(2, json!({"type":"media","title":"The glitch today","fold":false,"items":[{"kind":"video","path":"images/pip-resize.mp4","caption":"Resizing the window (test pattern)","label":"built","poster":"images/pip-resize.png"}]}));
            write(&file, &json_bytes(&card)?)?;
        }
    }
    if options.with_problems {
        write(
            &root.join("v1/writers/vm.fo-welcome/tasks.json"),
            b"{\"schema\": \"agent-reports/tasks@1\", \"writer\": ",
        )?;
        let mut broken = b"{\"schema\":".to_vec();
        broken.extend([0; 32]);
        write(
            &root.join("v1/writers/vm.fo-welcome/cards/vm.fo-welcome--Q1.json"),
            &broken,
        )?;
    }
    if let Some(ccs) = &options.ccs_dir {
        result["storeDir"] = json!(answers(root, ccs, now)?);
        result["answered"] = json!(["vm.pm-closure-lead--Q1", "mac.aac-claude-relogin--Q7"]);
        result["noted"] = json!(["vm.aac-status-lead--Q2"]);
    }
    Ok(result)
}
/// Write Scoreboard catalog and runs, including damaged and duplicate records.
pub fn write_scoreboard_fixture(root: &Path, now: Option<i64>) -> Result<Value> {
    if root.join("v1/catalog/models.json").exists() {
        return Err(format!("{} already has a catalog", root.display()).into());
    }
    let result = materialize(
        include_str!("../fixtures/scoreboard.json"),
        root,
        now.unwrap_or_else(|| Utc::now().timestamp_millis()),
        false,
    )?;
    // Keep catalog data tied to the shipped source, rather than a captured historical copy.
    for (name, bytes) in [
        (
            "benchmarks.json",
            include_bytes!("../../../docs/agent-reports/catalog/benchmarks.json").as_slice(),
        ),
        (
            "measures.json",
            include_bytes!("../../../docs/agent-reports/catalog/measures.json").as_slice(),
        ),
        (
            "model-aliases.json",
            include_bytes!("../../../docs/agent-reports/catalog/model-aliases.json").as_slice(),
        ),
        (
            "models.json",
            include_bytes!("../../../docs/agent-reports/catalog/models.json").as_slice(),
        ),
        (
            "task-types.json",
            include_bytes!("../../../docs/agent-reports/catalog/task-types.json").as_slice(),
        ),
        (
            "uses.json",
            include_bytes!("../../../docs/agent-reports/catalog/uses.json").as_slice(),
        ),
    ] {
        write(&root.join("v1/catalog").join(name), bytes)?;
    }
    Ok(result)
}
/// Run `fixture status|scoreboard <reports-dir>`; `--json` returns module-compatible metadata.
pub fn run(args: &[String]) -> Result<()> {
    let kind = args
        .first()
        .ok_or("Usage: aac-xtask fixture status|scoreboard <reports-dir> [options]")?;
    let mut options = StatusOptions {
        with_video: true,
        ..Default::default()
    };
    let mut root = None;
    let mut json_output = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--ccs-dir" => {
                i += 1;
                options.ccs_dir =
                    Some(PathBuf::from(args.get(i).ok_or("--ccs-dir needs a value")?));
            }
            "--now" => {
                i += 1;
                options.now = Some(args.get(i).ok_or("--now needs milliseconds")?.parse()?);
            }
            "--with-problems" => options.with_problems = true,
            "--with-kinds" => options.with_kinds = true,
            "--no-video" => options.with_video = false,
            "--no-quota" => options.no_quota = true,
            "--json" => json_output = true,
            arg if !arg.starts_with('-') && root.is_none() => root = Some(PathBuf::from(arg)),
            _ => return Err(format!("Unknown fixture argument: {}", args[i]).into()),
        }
        i += 1;
    }
    let root = root.ok_or("reports-dir is required")?;
    let root = std::path::absolute(root)?;
    if options.ccs_dir.is_none() {
        options.ccs_dir = std::env::var_os("CCS_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("CCS_HOME")
                    .filter(|v| !v.is_empty())
                    .map(|p| PathBuf::from(p).join(".ccs"))
            });
    }
    if let Some(ccs) = &mut options.ccs_dir {
        *ccs = std::path::absolute(&*ccs)?;
    }
    let result = match kind.as_str() {
        "status" => write_status_fixture(&root, &options)?,
        "scoreboard" => write_scoreboard_fixture(&root, options.now)?,
        _ => return Err(format!("Unknown fixture: {kind}").into()),
    };
    if json_output {
        println!("{}", serde_json::to_string(&result)?);
    } else if kind == "scoreboard" {
        println!(
            "Wrote {} runs and the catalog to {}.\nServer environment:\n  AAC_AGENT_REPORTS_DIR={}",
            result["runs"].as_array().ok_or("missing runs")?.len(),
            root.display(),
            root.display()
        );
    } else {
        println!(
            "Wrote {} cards to {}.",
            result["cards"].as_array().ok_or("missing cards")?.len(),
            root.display()
        );
        if let Some(store) = result["storeDir"].as_str() {
            println!(
                "Answered {} of them in {}.",
                result["answered"]
                    .as_array()
                    .ok_or("missing answers")?
                    .len(),
                store
            );
        } else {
            println!("No ccs folder given: the 2 answered cards show as open (pass --ccs-dir).");
        }
        println!(
            "Server environment:\n  AAC_AGENT_REPORTS_DIR={}\n  AAC_PM_MAIL_REGISTRY_DIR={}\n  AAC_STATUS_DOORBELL=off",
            root.display(),
            root.join("pm-mail-registry").display()
        );
        if let Some(ccs) = options.ccs_dir {
            println!("  CCS_DIR={}", ccs.display());
        } else {
            println!();
        }
    }
    Ok(())
}
