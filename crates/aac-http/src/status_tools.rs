//! Direct, read-only Status readers for browser-check and Simlock state.
//!
//! browser-check keeps fleet metadata and session summaries on the Cursor share; each
//! agent-browser backend keeps its own JSON session registry. Simlock keeps a worker's
//! devices and leases in `state.db` and its limits in `config.json`. The gateway's live
//! aggregate is not persisted, so the fleet reader uses its loopback HTTP status route.

use serde_json::{Map, Value as Json, json};
use std::collections::{BTreeMap, HashSet};

/// The browser state files needed for one host.
#[derive(Debug, Clone)]
pub(crate) struct BrowserState {
    pub caps: Json,
    pub browsers: Vec<Json>,
    pub records: Vec<Json>,
    pub gpu_util: Option<i64>,
    pub mem_gb: Option<f64>,
    pub display_problem: Option<String>,
    pub p1000: bool,
    pub intel: bool,
}

/// Read one backend's state files without starting agent-browser or browser-check.
#[allow(clippy::too_many_arguments)]
pub(crate) fn read_browser_state(
    config_path: &std::path::Path,
    browser_dir: &std::path::Path,
    record_dir: Option<&std::path::Path>,
    browser_prefix: &str,
    record_prefix: Option<&str>,
    mem_gb: Option<f64>,
    gpu_util: Option<i64>,
    display_problem: Option<String>,
) -> Result<BrowserState, String> {
    let caps = read_json(config_path, "browser-check caps")?;
    let browsers = read_session_registry(browser_dir, "browser-check browsers", browser_prefix)?;
    let records = match (record_dir, record_prefix) {
        (Some(path), Some(prefix)) => {
            read_session_registry(path, "browser-check recordings", prefix)?
        }
        _ => Vec::new(),
    };
    Ok(BrowserState {
        caps,
        browsers,
        records,
        gpu_util,
        mem_gb,
        display_problem,
        p1000: config_path.ends_with("p1000-agent.json"),
        intel: config_path.ends_with("intel-agent.json"),
    })
}

fn read_json(path: &std::path::Path, what: &str) -> Result<Json, String> {
    let bytes = std::fs::read(path).map_err(|error| {
        format!(
            "{what} state file {} is unreadable: {error}",
            path.display()
        )
    })?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(format!(
            "{what} state file {} is too large.",
            path.display()
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("{what} state file {} is damaged: {error}", path.display()))
}

fn read_session_registry(
    dir: &std::path::Path,
    what: &str,
    prefix: &str,
) -> Result<Vec<Json>, String> {
    let entries = std::fs::read_dir(dir).map_err(|error| {
        format!(
            "{what} state directory {} is unreadable: {error}",
            dir.display()
        )
    })?;
    let mut rows = Vec::new();
    let mut bytes = 0usize;
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "{what} state directory {} is unreadable: {error}",
                dir.display()
            )
        })?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json")
            || !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix))
        {
            continue;
        }
        let mut row = read_json(&path, what)?;
        if row.is_object() {
            bytes += path.metadata().map_or(0, |meta| meta.len() as usize);
            if bytes > 16 * 1024 * 1024 {
                return Err(format!(
                    "{what} state directory {} is too large.",
                    dir.display()
                ));
            }
            if let Some(object) = row.as_object_mut() {
                object.insert(
                    "_mtime".to_owned(),
                    path.metadata()
                        .ok()
                        .and_then(|meta| meta.modified().ok())
                        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(Json::Null, |time| Json::from(time.as_secs_f64())),
                );
            }
            rows.push(row);
        }
    }
    rows.sort_by(|a, b| {
        let a = a["id"].as_str().unwrap_or("");
        let b = b["id"].as_str().unwrap_or("");
        a.cmp(b)
    });
    Ok(rows)
}
const TERMINAL: &[&str] = &[
    "complete",
    "completed",
    "stopped",
    "failed",
    "error",
    "expired",
    "crashed",
    "gone",
];

fn live(row: &Json) -> bool {
    row.get("status")
        .and_then(Json::as_str)
        .is_none_or(|status| !TERMINAL.contains(&status.to_ascii_lowercase().as_str()))
}

fn number(value: &Json) -> Option<f64> {
    value.as_f64().filter(|number| number.is_finite())
}

fn rounded(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn owner(row: &Json) -> Option<String> {
    match row.get("owner") {
        Some(Json::Object(owner)) => {
            let parts: Vec<&str> = ["thread", "tool"]
                .iter()
                .filter_map(|key| owner.get(*key).and_then(Json::as_str))
                .collect();
            (!parts.is_empty()).then(|| parts.join(" · "))
        }
        Some(Json::String(owner)) if !owner.is_empty() => Some(owner.clone()),
        _ => None,
    }
}

fn started_at(row: &Json) -> Option<f64> {
    [
        "created",
        "created_at",
        "started_at",
        "record_started",
        "record_started_at",
        "_mtime",
    ]
    .iter()
    .find_map(|key| {
        number(&row[*key]).and_then(|value| {
            (value > 0.0).then(|| if value > 1e12 { value / 1000.0 } else { value })
        })
    })
}

fn recorded(row: &Json) -> bool {
    row.get("record_output")
        .is_some_and(|value| !value.is_null())
        || row.get("recording_active") == Some(&Json::Bool(true))
        || row.get("recording").is_some_and(|recording| {
            recording
                .get("status")
                .and_then(Json::as_str)
                .is_none_or(|status| !TERMINAL.contains(&status))
        })
}

fn weight(row: &Json) -> i64 {
    ["recording_weight", "encoder_weight"]
        .iter()
        .find_map(|key| number(&row[*key]))
        .and_then(|value| value.is_finite().then(|| value.round() as i64))
        .filter(|value| *value > 0)
        .unwrap_or(1)
}

fn phone_rows(simlock: Option<&Json>, local: &str) -> BTreeMap<String, Json> {
    let mut rows = BTreeMap::new();
    let Some(simlock) = simlock else {
        return rows;
    };
    if let Some(workers) = simlock["workers"].as_array() {
        for worker in workers {
            let Some(label) = worker["label"].as_str() else {
                continue;
            };
            let Some(machine) = aac_status::computers::exact_computer(label) else {
                continue;
            };
            if let Some(phones) = phone_capacity(worker.get("capacity")) {
                rows.insert(machine.to_owned(), phones);
            }
        }
    }
    if let Some(phones) = phone_capacity(simlock.get("capacity")) {
        rows.insert(local.to_owned(), phones);
    }
    rows
}

fn phone_capacity(capacity: Option<&Json>) -> Option<Json> {
    let capacity = capacity?.as_object()?;
    let mut phones = Map::new();
    for platform in ["ios", "android"] {
        let value = capacity.get(platform)?;
        let running = int(value.get("running")?)?;
        let reserved = int(value.get("reserved")?)?;
        let max = int(value.get("maxRunning")?)?;
        phones.insert(
            platform.to_owned(),
            json!({"running": running + reserved, "max": max}),
        );
    }
    Some(Json::Object(phones))
}

fn browser_state_json(state: &BrowserState, now: f64, phones: Option<&Json>) -> Json {
    let browsers: Vec<&Json> = state.browsers.iter().filter(|row| live(row)).collect();
    let records: Vec<&Json> = state.records.iter().filter(|row| live(row)).collect();
    let attached: HashSet<&str> = records
        .iter()
        .filter_map(|row| row["browser_id"].as_str())
        .collect();
    let outputs: HashSet<&str> = records
        .iter()
        .filter_map(|row| row["output"].as_str())
        .collect();
    let recorded_browsers: Vec<&Json> = browsers
        .iter()
        .copied()
        .filter(|row| {
            recorded(row)
                && !row["id"].as_str().is_some_and(|id| attached.contains(id))
                && !row["record_output"]
                    .as_str()
                    .is_some_and(|output| outputs.contains(output))
        })
        .collect();
    let recording_count = records.len() + recorded_browsers.len();
    let recording_weight: i64 = records.iter().map(|row| weight(row)).sum::<i64>()
        + recorded_browsers.iter().map(|row| weight(row)).sum::<i64>();
    let sessions: Vec<Json> = browsers
        .iter()
        .map(|row| {
            let started = started_at(row);
            let age = started.map(|started| (now - started).max(0.0).round() as i64);
            let url = row["page"]["url"]
                .as_str()
                .or_else(|| row["navigation"]["url"].as_str())
                .or_else(|| row["url"].as_str())
                .unwrap_or("");
            let blank = url.is_empty()
                || url == "about:blank"
                || url == "chrome://newtab/"
                || url.starts_with("data:text/html,<title>agent-browser</title>");
            let is_recording = records.iter().any(|record| {
                record["browser_id"].as_str() == row["id"].as_str()
                    || record["output"].as_str() == row["record_output"].as_str()
            }) || recorded_browsers
                .iter()
                .any(|record| record["id"] == row["id"]);
            json!({
                "id": row["id"],
                "recording": is_recording,
                "visible": row["headless"] == Json::Bool(false)
                    || (row.get("headless").is_none() && is_recording),
                "weight": if is_recording { weight(row) } else { 0 },
                "url": url,
                "owner": owner(row),
                "started": started,
                "age_s": age,
                "idle": blank && age.is_some_and(|age| age > 600),
            })
        })
        .collect();
    let cap_keys = [
        "max_recordings",
        "max_browsers",
        "max_browsers_while_recording",
        "max_total_encoder_sessions",
        "recording_budget",
        "stream_weight",
        "reserved_sunshine_streams",
        "hevc_weight",
        "default_codec",
    ];
    let caps: Map<String, Json> = cap_keys
        .iter()
        .filter_map(|key| {
            state
                .caps
                .get(*key)
                .map(|value| ((*key).to_owned(), value.clone()))
        })
        .collect();
    let p1000 = state.p1000
        && (state.browsers.iter().any(|row| row["fps"] == 120)
            || state.records.iter().any(|row| row["fps"] == 120));
    json!({
        "caps": caps,
        "browsers": browsers.len(),
        "recordings": recording_count,
        "recording_weight": recording_weight,
        "inventory": {"state":"fresh","retries":0},
        "inventory_trace": [],
        "gpu_util": state.gpu_util,
        "mem_gb": state.mem_gb,
        "display_problem": state.display_problem,
        "snapshot": Json::Null,
        "exclusive": state.browsers.iter().chain(state.records.iter()).any(|row| row["exclusive_gpu"] == true) || p1000,
        "hevc_alone": state.intel && records.iter().any(|row| row["codec"] == "hevc"),
        "phones": phones,
        "sessions": sessions,
    })
}

/// The browser-check fleet file, honoring the tool's environment overrides.
pub(crate) fn browser_fleet_path() -> std::path::PathBuf {
    if let Some(path) = std::env::var_os("T3SP_OPS_FLEET") {
        return std::path::PathBuf::from(path);
    }
    let share = std::env::var_os("GPU_BROWSER_SHARE").map_or_else(
        || {
            if cfg!(windows) {
                std::path::PathBuf::from(r"\\TRUENAS\Cursor")
            } else if cfg!(target_os = "macos") {
                std::path::PathBuf::from("/Volumes/Cursor")
            } else {
                std::path::PathBuf::from("/mnt/Cursor")
            }
        },
        std::path::PathBuf::from,
    );
    share.join("Tools/t3sp-ops-fleet.json")
}
/// Read browser-check's fleet metadata without starting the tool.
pub(crate) fn read_browser_fleet() -> Result<Json, String> {
    read_json(&browser_fleet_path(), "browser-check fleet")
}

/// Registry-only fallback so a missing fleet file still yields one unavailable row per host.
pub(crate) fn fallback_browser_fleet() -> Json {
    let machines: Vec<Json> = aac_status::computers::machine_ids()
        .into_iter()
        .filter_map(|machine| {
            let row = aac_status::computers::computer(machine)?;
            Some(json!({
                "name": row["machineId"],
                "label": row["machineLabel"],
                "hostnames": row["names"],
            }))
        })
        .collect();
    json!({"machines": machines})
}

/// Build the same `{rows:[...]}` shape browser-check slots writes, from direct state reads.
pub(crate) fn browser_slots_json(
    fleet: &Json,
    local: &str,
    states: BTreeMap<String, Result<BrowserState, String>>,
    simlock: Option<&Json>,
    now: f64,
) -> Json {
    let phones = phone_rows(simlock, local);
    let rows: Vec<Json> = fleet["machines"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|machine| {
            let name = machine["name"].as_str()?;
            let computer = aac_status::computers::exact_computer(name)?;
            let state = match states.get(computer) {
                Some(Ok(state)) => browser_state_json(state, now, phones.get(computer)),
                Some(Err(reason)) => json!({"error": reason}),
                None => json!({"error": "browser-check state was not read for this machine."}),
            };
            Some(json!({
                "machine": machine,
                "self": computer == local,
                "state": state,
            }))
        })
        .collect();
    json!({"here": local, "rows": rows})
}

#[allow(clippy::type_complexity)]
fn local_backend(
    machine: &Json,
) -> Result<
    (
        std::path::PathBuf,
        std::path::PathBuf,
        Option<std::path::PathBuf>,
        &'static str,
        Option<&'static str>,
    ),
    String,
> {
    use std::path::{Path, PathBuf};
    let home = std::env::home_dir().ok_or("The home directory is unavailable.")?;
    let backend = machine["backend"].as_str().unwrap_or("desktop");
    if cfg!(target_os = "linux")
        && (backend == "p1000"
            || backend == "intel"
            || backend == "local"
            || Path::new("/etc/p1000-agent.json").exists()
            || Path::new("/etc/intel-agent.json").exists())
    {
        let kind = if backend == "intel"
            || (!Path::new("/etc/p1000-agent.json").exists()
                && Path::new("/etc/intel-agent.json").exists())
        {
            "intel"
        } else {
            "p1000"
        };
        let config = Path::new(&format!("/etc/{kind}-agent.json")).to_path_buf();
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(
            || Path::new("/run/user").join(aac_files::path::uid().to_string()),
            PathBuf::from,
        );
        let base = runtime.join(format!("{kind}-agent"));
        return Ok((
            config,
            base.clone(),
            Some(base),
            "browser-",
            Some("record-"),
        ));
    }
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map_or_else(
            || home.join("AppData/Local/agent-browser"),
            |path| PathBuf::from(path).join("agent-browser"),
        )
    } else {
        home.join(".agent-browser")
    };
    Ok((
        base.join("config.json"),
        base.join("sessions"),
        None,
        "",
        None,
    ))
}

/// Read the dashboard computer's backend state directly.
pub(crate) async fn read_local_browser_state(machine: &Json) -> Result<BrowserState, String> {
    let (config, browsers, records, browser_prefix, record_prefix) = local_backend(machine)?;
    let mem_gb = local_mem_gb().await;
    let gpu_util = local_gpu_util().await;
    let display_problem = local_display_problem().await;
    read_browser_state(
        &config,
        &browsers,
        records.as_deref(),
        browser_prefix,
        record_prefix,
        mem_gb,
        gpu_util,
        display_problem,
    )
}

async fn local_mem_gb() -> Option<f64> {
    if cfg!(windows) {
        let script = "$ErrorActionPreference='Stop'; $os=Get-CimInstance Win32_OperatingSystem; [math]::Round($os.FreePhysicalMemory/1MB,1)";
        let encoded = {
            use base64::Engine;
            let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
            base64::engine::general_purpose::STANDARD.encode(bytes)
        };
        let result = crate::status_run::run(
            std::path::Path::new("powershell.exe"),
            &[
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-EncodedCommand".to_owned(),
                encoded,
            ],
            8_000,
            4096,
        )
        .await;
        return result.stdout.trim().parse().ok();
    }
    if cfg!(target_os = "macos") {
        let result = crate::status_run::run(
            std::path::Path::new("/bin/sh"),
            &["-c".to_owned(), "vm_stat | awk '/page size of/{p=$4} /Pages free:/{f=$3} /Pages inactive:/{i=$3} /Pages speculative:/{s=$3} END{printf \"%.1f\\n\",(f+i+s)*p/1073741824}'".to_owned()],
            8_000,
            4096,
        ).await;
        return result.stdout.trim().parse().ok();
    }
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kib = text.lines().find_map(|line| {
        let rest = line.strip_prefix("MemAvailable:")?;
        rest.split_whitespace().next()?.parse::<f64>().ok()
    })?;
    Some(rounded(kib / 1024.0 / 1024.0))
}

async fn local_gpu_util() -> Option<i64> {
    if cfg!(target_os = "macos") {
        return None;
    }
    let result = crate::status_run::run(
        std::path::Path::new("nvidia-smi"),
        &[
            "--query-gpu=utilization.gpu".to_owned(),
            "--format=csv,noheader,nounits".to_owned(),
        ],
        4_000,
        4096,
    )
    .await;
    result.stdout.lines().next()?.trim().parse().ok()
}

async fn local_display_problem() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let script = "if [ ! -f /etc/p1000-agent.json ] && [ ! -f /etc/intel-agent.json ]; then exit 0; fi; export DISPLAY=:0 XAUTHORITY=\"${XAUTHORITY:-$HOME/.Xauthority}\"; if ! xset q >/dev/null 2>&1; then printf 'cannot open X display :0 using %s\\n' \"$XAUTHORITY\"; exit 0; fi; if ! xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null | grep -Eq 'window id # 0x[1-9a-f][0-9a-f]*'; then printf 'no window manager on :0; the X server may be at the lightdm greeter; restore its login session in an idle window\\n'; fi";
    let result = crate::status_run::run(
        std::path::Path::new("/bin/sh"),
        &["-c".to_owned(), script.to_owned()],
        8_000,
        4096,
    )
    .await;
    (!result.stdout.trim().is_empty()).then(|| result.stdout.trim().to_owned())
}

/// Read a remote backend's JSON state files over one read-only SSH command.
pub(crate) async fn read_remote_browser_state(
    host: aac_status::produce::machines::HostOs,
    alias: &str,
) -> Result<BrowserState, String> {
    let command = remote_browser_command(host);
    let result = crate::status_run::run(
        std::path::Path::new("ssh"),
        &crate::status_run::ssh_argv(alias, &command, Some((5, 2))),
        30_000,
        4 * 1024 * 1024,
    )
    .await;
    if result.code != Some(0) {
        return Err("browser-check state files could not be read over ssh.".to_owned());
    }
    let raw: Json = serde_json::from_str(&result.stdout)
        .map_err(|_| "browser-check state files answered in an unknown shape.".to_owned())?;
    Ok(BrowserState {
        caps: raw["caps"].clone(),
        browsers: raw["browsers"].as_array().cloned().unwrap_or_default(),
        records: raw["records"].as_array().cloned().unwrap_or_default(),
        gpu_util: raw["gpu_util"].as_i64(),
        mem_gb: raw["mem_gb"].as_f64(),
        display_problem: raw["display_problem"].as_str().map(str::to_owned),
        intel: raw["intel"] == true,
        p1000: raw["p1000"] == true,
    })
}

fn remote_browser_command(host: aac_status::produce::machines::HostOs) -> String {
    use aac_status::produce::machines::HostOs;
    if host == HostOs::Windows {
        let script = r#"$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; $base=Join-Path $env:LOCALAPPDATA 'agent-browser'; $cfg=Get-Content -LiteralPath (Join-Path $base 'config.json') -Raw | ConvertFrom-Json; $dead='complete','completed','stopped','failed','error','expired','crashed','gone'; $rows=@(Get-ChildItem -LiteralPath (Join-Path $base 'sessions') -Filter *.json | ForEach-Object { Get-Content -LiteralPath $_.FullName -Raw | ConvertFrom-Json } | Where-Object { $dead -notcontains $_.status }); $caps=@{}; foreach($k in 'max_recordings','max_browsers','max_browsers_while_recording','max_total_encoder_sessions','recording_budget','stream_weight','reserved_sunshine_streams','hevc_weight','default_codec'){ if($null -ne $cfg.$k){$caps[$k]=$cfg.$k} }; $gpu=$null; try{$gpu=[int](nvidia-smi --query-gpu=utilization.gpu --format=csv,noheader,nounits | Select-Object -First 1)}catch{}; $os=Get-CimInstance Win32_OperatingSystem; [ordered]@{caps=$caps;browsers=$rows;records=@();gpu_util=$gpu;mem_gb=[math]::Round($os.FreePhysicalMemory/1MB,1);display_problem=$null;intel=$false;p1000=$false} | ConvertTo-Json -Depth 8 -Compress"#;
        let encoded = {
            use base64::Engine;
            let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
            base64::engine::general_purpose::STANDARD.encode(bytes)
        };
        return format!("powershell.exe -NoProfile -NonInteractive -EncodedCommand {encoded}");
    }
    r#"set -eu
home=$HOME
uid=$(id -u)
if [ -f /etc/p1000-agent.json ]; then cfg=/etc/p1000-agent.json; base=/run/user/$uid/p1000-agent; intel=false; p1000=true; prefix=browser-; rprefix=record-
elif [ -f /etc/intel-agent.json ]; then cfg=/etc/intel-agent.json; base=/run/user/$uid/intel-agent; intel=true; p1000=false; prefix=browser-; rprefix=record-
else cfg=$home/.agent-browser/config.json; base=$home/.agent-browser/sessions; intel=false; p1000=false; prefix=; rprefix=
fi
caps=$(jq -c '{max_recordings,max_browsers,max_browsers_while_recording,max_total_encoder_sessions,recording_budget,stream_weight,reserved_sunshine_streams,hevc_weight,default_codec} | with_entries(select(.value != null))' "$cfg")
read_registry(){ p=$1; set -- "$base"/"$p"*.json; if [ ! -e "$1" ]; then printf '[]'; return; fi; jq -s -c 'map(select((.status // "") | test("^(complete|completed|stopped|failed|error|expired|crashed|gone)$";"i") | not))' "$@"; }
browsers=$(read_registry "$prefix")
if [ -n "$rprefix" ]; then records=$(read_registry "$rprefix"); else records='[]'; fi
gpu='null'; if command -v nvidia-smi >/dev/null 2>&1; then v=$(nvidia-smi --query-gpu=utilization.gpu --format=csv,noheader,nounits 2>/dev/null | awk 'NR==1{gsub(/ /,"",$0); print $0}'); case "$v" in ''|*[!0-9]*) ;; *) gpu=$v;; esac; fi
if [ "$(uname -s)" = Darwin ]; then mem=$(vm_stat | awk '/page size of/{p=$4} /Pages free:/{f=$3} /Pages inactive:/{i=$3} /Pages speculative:/{s=$3} END{printf "%.1f",(f+i+s)*p/1073741824}'); display='null'
else mem=$(awk '/^MemAvailable:/{printf "%.1f",$2/1048576}' /proc/meminfo); display='null'; if [ -f /etc/p1000-agent.json ] || [ -f /etc/intel-agent.json ]; then export DISPLAY=:0 XAUTHORITY="${XAUTHORITY:-$home/.Xauthority}"; if ! xset q >/dev/null 2>&1; then display=$(jq -nc --arg v "cannot open X display :0 using $XAUTHORITY" '$v'); elif ! xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null | grep -Eq 'window id # 0x[1-9a-f][0-9a-f]*'; then display=$(jq -nc --arg v 'no window manager on :0; the X server may be at the lightdm greeter; restore its login session in an idle window' '$v'); fi; fi
fi
jq -nc --argjson caps "$caps" --argjson browsers "$browsers" --argjson records "$records" --argjson gpu "$gpu" --argjson mem "$mem" --argjson display "$display" --argjson intel "$intel" --argjson p1000 "$p1000" '{caps:$caps,browsers:$browsers,records:$records,gpu_util:$gpu,mem_gb:$mem,display_problem:$display,intel:$intel,p1000:$p1000}'
"#.to_owned()
}

/// Read one Simlock worker's devices and leases from its SQLite state file.
pub(crate) fn read_simlock_worker(
    state_path: &std::path::Path,
    config_path: &std::path::Path,
    daemon_running: bool,
) -> Result<Json, String> {
    if !state_path.is_file() {
        return Err(format!(
            "Simlock state file {} is missing.",
            state_path.display()
        ));
    }
    let config = read_json(config_path, "Simlock config")?;
    let connection = rusqlite::Connection::open_with_flags(
        state_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| {
        format!(
            "Simlock state file {} is damaged: {error}",
            state_path.display()
        )
    })?;
    connection
        .pragma_update(None, "query_only", true)
        .map_err(|error| {
            format!(
                "Simlock state file {} is unreadable: {error}",
                state_path.display()
            )
        })?;
    let mut devices = Vec::new();
    {
        let mut statement = connection
            .prepare("SELECT id, platform, model, os_version, state, mode, physical FROM devices WHERE deleted_at IS NULL ORDER BY id")
            .map_err(|error| format!("Simlock state file {} is damaged: {error}", state_path.display()))?;
        let rows = statement
            .query_map([], |row| {
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "platform": row.get::<_, String>(1)?,
                    "model": row.get::<_, String>(2)?,
                    "osVersion": row.get::<_, String>(3)?,
                    "state": row.get::<_, String>(4)?,
                    "mode": row.get::<_, Option<String>>(5)?,
                    "physical": row.get::<_, i64>(6)? != 0,
                }))
            })
            .map_err(|error| {
                format!(
                    "Simlock state file {} is damaged: {error}",
                    state_path.display()
                )
            })?;
        for row in rows {
            devices.push(row.map_err(|error| {
                format!(
                    "Simlock state file {} is damaged: {error}",
                    state_path.display()
                )
            })?);
        }
    }
    let mut leases = Vec::new();
    {
        let mut statement = connection
            .prepare("SELECT id, device_id, requester_id, granted_at, ttl_deadline FROM leases ORDER BY granted_at, id")
            .map_err(|error| format!("Simlock state file {} is damaged: {error}", state_path.display()))?;
        let rows = statement
            .query_map([], |row| {
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "deviceId": row.get::<_, String>(1)?,
                    "requesterId": row.get::<_, String>(2)?,
                    "grantedAt": row.get::<_, i64>(3)?,
                    "ttlDeadline": row.get::<_, f64>(4)?,
                }))
            })
            .map_err(|error| {
                format!(
                    "Simlock state file {} is damaged: {error}",
                    state_path.display()
                )
            })?;
        for row in rows {
            leases.push(row.map_err(|error| {
                format!(
                    "Simlock state file {} is damaged: {error}",
                    state_path.display()
                )
            })?);
        }
    }
    let devices: Vec<Json> = devices
        .into_iter()
        .map(|device| {
            let spec = json!({
                "physical": device["physical"],
                "platform": device["platform"],
                "model": device["model"],
                "osVersion": device["osVersion"],
            });
            json!({
                "id": device["id"],
                "spec": spec,
                "state": device["state"],
                "physical": device["physical"],
                "mode": device["mode"],
            })
        })
        .collect();
    let capacity = worker_capacity(&config, &devices);
    Ok(json!({
        "daemon": {"health": if daemon_running { "running" } else { "failed" }, "mode": "worker"},
        "capacity": capacity,
        "devices": devices,
        "leases": leases,
        "workers": Json::Null,
    }))
}

fn worker_capacity(config: &Json, devices: &[Json]) -> Json {
    let fixed = &config["capacity"]["config"];
    let global_max = int(&fixed["maxRunning"])
        .or_else(|| int(&fixed["limits"]["maxRunning"]))
        .unwrap_or(0);
    let platform_max = |platform: &str| {
        int(&fixed[platform]["maxRunning"])
            .or_else(|| int(&fixed["limits"][platform]["maxRunning"]))
            .unwrap_or(global_max)
    };
    let entry = |platform: Option<&str>| {
        let running = devices
            .iter()
            .filter(|device| {
                platform.is_none_or(|want| device["spec"]["platform"] == want)
                    && matches!(
                        device["state"].as_str(),
                        Some("ready" | "leased" | "reclaiming" | "quarantined")
                    )
            })
            .count() as i64;
        let max = platform.map_or(global_max, platform_max);
        json!({"running": running, "maxRunning": max, "reserved": Json::Null, "overLimit": Json::Null})
    };
    json!({"ios": entry(Some("ios")), "android": entry(Some("android")), "global": entry(None)})
}

/// Read the gateway's live aggregate without starting the Simlock CLI.
pub(crate) fn read_simlock_gateway(
    config_path: &std::path::Path,
    token_path: &std::path::Path,
) -> Result<Json, String> {
    read_simlock_gateway_from(
        config_path,
        token_path,
        std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .ok()
            .as_deref(),
    )
}

fn read_simlock_gateway_from(
    config_path: &std::path::Path,
    token_path: &std::path::Path,
    hostname: Option<&str>,
) -> Result<Json, String> {
    let config = read_json(config_path, "Simlock gateway config")?;
    let token_path = agent_token_path(token_path, hostname)?;
    let token = std::fs::read_to_string(&token_path).map_err(|error| {
        format!(
            "Simlock gateway credential {} is unreadable: {error}",
            token_path.display()
        )
    })?;
    let token = token.trim();
    if token.is_empty() {
        return Err(format!(
            "Simlock gateway credential {} is empty.",
            token_path.display()
        ));
    }
    let host = config["http"]["host"].as_str().unwrap_or("127.0.0.1");
    let host = if host == "0.0.0.0" || host == "::" {
        "127.0.0.1"
    } else {
        host
    };
    let port = config["http"]["port"].as_u64().unwrap_or(4700);
    let url = format!("http://{host}:{port}/v1/status");
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_millis(20_000))
        .no_proxy()
        .build()
        .map_err(|error| format!("Simlock gateway reader could not start: {error}"))?;
    let response = client
        .get(&url)
        .header("Accept", "application/json")
        .bearer_auth(token)
        .send()
        .map_err(|error| format!("Simlock gateway state is unreadable: {error}"))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .map_err(|error| format!("Simlock gateway state is unreadable: {error}"))?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("Simlock gateway state is too large.".to_owned());
    }
    if !status.is_success() {
        return Err(format!(
            "Simlock gateway state is unavailable (HTTP {}).",
            status.as_u16()
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("Simlock gateway state is damaged: {error}"))
}

fn int(value: &Json) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|n| n.round() as i64))
}
fn agent_token_path(
    token_path: &std::path::Path,
    hostname: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    let directory = token_path
        .parent()
        .ok_or("Simlock gateway credential directory is unavailable.")?;
    let name = hostname.unwrap_or("");
    let machine = aac_status::computers::exact_computer(name).unwrap_or(name);
    let direct = directory.join(format!("t3-{machine}"));
    if name.is_empty() || !direct.is_file() {
        return Err(format!(
            "Simlock gateway agent credential {} is missing.",
            direct.display()
        ));
    }
    Ok(direct)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::path::Path;

    fn config(path: &Path, value: &Json) {
        std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    }

    #[test]
    fn browser_state_missing_file_is_named() {
        let temp = tempfile::tempdir().unwrap();
        let error = read_browser_state(
            &temp.path().join("config.json"),
            &temp.path().join("sessions"),
            None,
            "browser-",
            None,
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(error.contains("config.json is unreadable"), "{error}");
    }

    #[test]
    fn browser_state_damaged_file_is_named() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        std::fs::create_dir(&sessions).unwrap();
        config(
            &temp.path().join("config.json"),
            &json!({"max_browsers": 2}),
        );
        std::fs::write(sessions.join("browser-aaaaaaaaaaaa.json"), "{").unwrap();
        let error = read_browser_state(
            &temp.path().join("config.json"),
            &sessions,
            None,
            "browser-",
            None,
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(error.contains("is damaged"), "{error}");
    }

    #[test]
    fn browser_state_normal_files_keep_counts_and_sessions() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        std::fs::create_dir(&sessions).unwrap();
        config(
            &temp.path().join("config.json"),
            &json!({"max_browsers": 2}),
        );
        config(
            &sessions.join("browser-aaaaaaaaaaaa.json"),
            &json!({"id":"aaaaaaaaaaaa","status":"ready","url":"about:blank"}),
        );
        let state = read_browser_state(
            &temp.path().join("config.json"),
            &sessions,
            None,
            "browser-",
            None,
            Some(12.3),
            Some(7),
            None,
        )
        .unwrap();
        assert_eq!(state.caps["max_browsers"], 2);
        assert_eq!(state.browsers.len(), 1);
        assert_eq!(state.mem_gb, Some(12.3));
        assert_eq!(state.gpu_util, Some(7));
    }

    #[test]
    fn simlock_state_missing_file_is_named() {
        let temp = tempfile::tempdir().unwrap();
        config(
            &temp.path().join("config.json"),
            &json!({"capacity":{"config":{"maxRunning":1}}}),
        );
        let error = read_simlock_worker(
            &temp.path().join("state.db"),
            &temp.path().join("config.json"),
            true,
        )
        .unwrap_err();
        assert!(error.contains("state.db is missing"), "{error}");
    }
    #[test]
    fn simlock_state_damaged_file_is_named() {
        let temp = tempfile::tempdir().unwrap();
        config(
            &temp.path().join("config.json"),
            &json!({"capacity":{"config":{"maxRunning":1}}}),
        );
        std::fs::write(temp.path().join("state.db"), "not sqlite").unwrap();
        let error = read_simlock_worker(
            &temp.path().join("state.db"),
            &temp.path().join("config.json"),
            true,
        )
        .unwrap_err();
        assert!(error.contains("state.db is damaged"), "{error}");
    }

    #[test]
    fn simlock_state_normal_file_keeps_devices_and_leases() {
        let temp = tempfile::tempdir().unwrap();
        config(
            &temp.path().join("config.json"),
            &json!({"capacity":{"strategy":"fixed","config":{"maxRunning":2,"android":{"maxRunning":1}}}}),
        );
        let connection = rusqlite::Connection::open(temp.path().join("state.db")).unwrap();
        connection.execute_batch(
            "CREATE TABLE devices (id TEXT PRIMARY KEY, platform TEXT NOT NULL, model TEXT NOT NULL, os_version TEXT NOT NULL, state TEXT NOT NULL, mode TEXT, physical INTEGER NOT NULL, deleted_at INTEGER);
             CREATE TABLE leases (id TEXT PRIMARY KEY, device_id TEXT NOT NULL, requester_id TEXT NOT NULL, granted_at INTEGER NOT NULL, ttl_deadline REAL NOT NULL);
             INSERT INTO devices VALUES ('dev_a','android','Pixel','36','leased','full',0,NULL);
             INSERT INTO leases VALUES ('lease_a','dev_a','fixture',100,200.0);",
        ).unwrap();
        drop(connection);
        let state = read_simlock_worker(
            &temp.path().join("state.db"),
            &temp.path().join("config.json"),
            true,
        )
        .unwrap();
        assert_eq!(state["capacity"]["android"]["running"], 1);
        assert_eq!(state["capacity"]["android"]["maxRunning"], 1);
        assert_eq!(state["devices"][0]["spec"]["model"], "Pixel");
        assert_eq!(state["leases"][0]["requesterId"], "fixture");
    }

    #[test]
    fn browser_rows_keep_counts_sessions_and_phone_capacity() {
        let state = BrowserState {
            caps: json!({"max_browsers": 4, "max_recordings": 2}),
            browsers: vec![json!({
                "id": "aaaaaaaaaaaa",
                "status": "ready",
                "url": "about:blank",
                "owner": {"tool": "fixture"},
                "created": 100.0,
            })],
            records: Vec::new(),
            gpu_util: Some(7),
            mem_gb: Some(12.3),
            display_problem: None,
            p1000: false,
            intel: false,
        };
        let fleet = json!({"machines": [{
            "name": "vm",
            "label": "Ubuntu VM",
            "hostnames": ["ubuntuserver", "vm"],
        }]});
        let simlock = json!({"capacity": {
            "ios": {"running": 1, "reserved": 2, "maxRunning": 6},
            "android": {"running": 0, "reserved": 0, "maxRunning": 5},
        }});
        let raw = browser_slots_json(
            &fleet,
            "vm",
            BTreeMap::from([("vm".to_owned(), Ok(state))]),
            Some(&simlock),
            200.0,
        );
        assert_eq!(raw["rows"][0]["state"]["browsers"], 1);
        assert_eq!(raw["rows"][0]["state"]["sessions"][0]["owner"], "fixture");
        assert_eq!(raw["rows"][0]["state"]["phones"]["ios"]["running"], 3);
        let rows = aac_status::produce::browsers::parse_slots(&raw).unwrap();
        assert_eq!(rows[0].browsers.running, Some(1));
        assert_eq!(
            rows[0]
                .phones
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .ios
                .as_ref()
                .unwrap()
                .running,
            Some(3)
        );
    }

    #[test]
    fn unknown_phone_reservations_are_not_invented() {
        assert!(
            phone_capacity(Some(&json!({
                "ios": {"running": 1, "maxRunning": 6},
                "android": {"running": 0, "reserved": 0, "maxRunning": 5},
            })))
            .is_none()
        );
    }

    #[test]
    fn gateway_agent_token_uses_the_registry_machine_id() {
        let temp = tempfile::tempdir().unwrap();
        let gateway = temp.path().join("gateway");
        std::fs::create_dir_all(&gateway).unwrap();
        std::fs::write(gateway.join("t3-vm"), "fixture").unwrap();
        let path = agent_token_path(&gateway.join("admin.token"), Some("ubuntuserver")).unwrap();
        assert_eq!(path, gateway.join("t3-vm"));
    }
}
