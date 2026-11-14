//! Machine probe and executable presence, with unknown readings kept null.
use aac_core::{Cause, Failure};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};
use tokio::io::AsyncReadExt;

pub(crate) fn parse(platform: &str, text: &str) -> Value {
    crate::proof::scope(|| parse_proven(platform, text))
}
fn parse_proven(platform: &str, text: &str) -> Value {
    if !["linux", "mac", "windows"].contains(&platform) || !crate::proof::ascii(text) {
        return crate::proof::fallback();
    }
    for (key, value) in text.lines().filter_map(|l| l.split_once('=')) {
        if crate::proof::strip(key) == "cpu" {
            for n in crate::proof::words(value) {
                crate::proof::decimal(n);
            }
        } else if [
            "cores",
            "coreCount",
            "load",
            "memTotalKb",
            "memTotalB",
            "memAvailKb",
            "pageSize",
            "pagesAvail",
            "diskTotalKb",
            "diskTotalB",
            "diskFreeKb",
            "diskFreeB",
            "cpuLoad",
            "cpuPct",
        ]
        .contains(&crate::proof::strip(key))
        {
            crate::proof::decimal(crate::proof::strip(value));
        }
    }
    let values: BTreeMap<_, _> = text
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (crate::proof::strip(k), crate::proof::strip(v)))
        .collect();
    let n = |key: &str| {
        values
            .get(key)
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|n| n.is_finite() && *n >= 0.0)
    };
    let lines = |key: &str| {
        text.lines()
            .filter_map(|l| l.strip_prefix(key))
            .map(crate::proof::strip)
            .collect::<Vec<_>>()
    };
    let counts = lines("coreCount=");
    let cores = if counts.is_empty() {
        n("cores").filter(|n| *n != 0.0)
    } else {
        counts
            .iter()
            .map(|s| {
                if !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()) {
                    s.parse::<f64>().ok().filter(|n| *n > 0.0)
                } else {
                    None
                }
            })
            .collect::<Option<Vec<_>>>()
            .map(|v| v.iter().sum())
    };
    let total = n("memTotalKb")
        .map(|n| n * 1024.0)
        .or_else(|| n("memTotalB"))
        .filter(|n| *n != 0.0);
    let avail = if platform == "mac" {
        let pages: Vec<_> = lines("pagesAvail=")
            .iter()
            .filter_map(|s| {
                if s.is_empty() {
                    Some(0.0)
                } else {
                    s.parse::<f64>().ok().filter(|n| n.is_finite())
                }
            })
            .collect();
        n("pageSize")
            .filter(|_| !pages.is_empty())
            .map(|n| n * pages.iter().sum::<f64>())
    } else {
        n("memAvailKb").map(|n| n * 1024.0)
    };
    let disk_total = n("diskTotalKb")
        .map(|n| n * 1024.0)
        .or_else(|| n("diskTotalB"));
    let disk_free = n("diskFreeKb")
        .map(|n| n * 1024.0)
        .or_else(|| n("diskFreeB"));
    if cores.is_none() && total.is_none() && disk_total.is_none() {
        return Value::Null;
    }
    let cpu = if platform == "linux" {
        values
            .get("cpu")
            .and_then(|s| {
                crate::proof::words(s)
                    .map(str::parse::<f64>)
                    .collect::<Result<Vec<_>, _>>()
                    .ok()
            })
            .filter(|v| v.len() >= 4 && v.iter().all(|n| n.is_finite()))
            .map(|v| {
                let total: f64 = v.iter().take(8).sum();
                json!({"busy":total-v[3]-v.get(4).copied().unwrap_or(0.0),"total":total})
            })
    } else {
        None
    };
    let loads = lines("cpuLoad=");
    let busy = if platform == "windows" && !loads.is_empty() {
        loads
            .iter()
            .map(|s| {
                if s.is_empty() {
                    None
                } else {
                    s.parse::<f64>().ok().filter(|n| n.is_finite() && *n >= 0.0)
                }
            })
            .collect::<Option<Vec<_>>>()
            .map(|v| (v.iter().sum::<f64>() / v.len() as f64).round().min(100.0))
    } else {
        n("cpuPct")
            .and_then(|n| {
                if platform == "mac" {
                    cores.map(|c| n / c)
                } else {
                    Some(n)
                }
            })
            .map(|n| n.round().min(100.0))
    };
    let disk = match (disk_total, disk_free) {
        (Some(t), Some(f)) if t > 0.0 => {
            let mount = values.get("mount").copied().unwrap_or("/");
            let mount = mount
                .chars()
                .map(|c| if c < ' ' || c == '\u{7f}' { ' ' } else { c })
                .collect::<String>();
            let mount = crate::proof::strip(&mount);
            let mount = if mount.is_empty() || aac_core::scrub(mount).as_str() != mount {
                "/".into()
            } else if mount.chars().count() > 80 {
                format!("{}...", mount.chars().take(79).collect::<String>())
            } else {
                mount.to_string()
            };
            json!({"mount":mount,"freeBytes":f,"totalBytes":t})
        }
        _ => Value::Null,
    };
    let mut row = json!({"cores":cores,"load1":n("load"),"busyPct":busy,"memAvailableBytes":avail,"memTotalBytes":total,"disk":disk,"cpuTimes":cpu});
    if row["disk"].is_null() {
        row["diskError"] = "The machine did not report its system disk.".into();
    }
    if total.is_none() || avail.is_none() {
        row["memoryError"] = "The machine did not report its memory.".into();
    }
    if if platform == "linux" {
        cpu.is_none()
    } else {
        busy.is_none()
    } {
        row["cpuError"] = "The machine did not report its CPU.".into();
    }
    row
}

pub(crate) async fn run(args: &Value) -> Result<Value, Failure> {
    // Batch only the parser fixture boundary; production still refuses all arguments.
    #[cfg(feature = "parity-fixtures")]
    if let Some(probes) = args["probes"].as_array() {
        if probes.len() > 256 {
            return Err(Failure::new(Cause::Refused, "fixture batch too large"));
        }
        return Ok(Value::Array(
            probes
                .iter()
                .map(|p| match (p["platform"].as_str(), p["probe"].as_str()) {
                    (Some(platform), Some(text)) => parse(platform, text),
                    _ => crate::proof::fallback(),
                })
                .collect(),
        ));
    }

    #[cfg(feature = "parity-fixtures")]
    if let (Some(platform), Some(probe)) = (args["platform"].as_str(), args["probe"].as_str()) {
        return Ok(parse(platform, probe));
    }
    if args.as_object().is_none_or(|o| !o.is_empty()) {
        return Err(Failure::new(Cause::Refused, "inventory takes no arguments"));
    }
    let platform = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    };
    let script = match platform {
        "windows" => include_str!("probes/windows.txt"),
        "mac" => include_str!("probes/mac.txt"),
        _ => include_str!("probes/linux.txt"),
    };
    let mut command = if platform == "windows" {
        let mut c = tokio::process::Command::new("powershell.exe");
        c.args(["-NoProfile", "-NonInteractive", "-Command", script]);
        c
    } else {
        let mut c = tokio::process::Command::new("/bin/sh");
        c.args(["-c", script]);
        c
    };
    command
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|_| Failure::new(Cause::NotInstalled, "machine probe unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Failure::new(Cause::Bug, "probe pipe unavailable"))?;
    let mut bytes = Vec::new();
    stdout.take(65537).read_to_end(&mut bytes).await?;
    if bytes.len() > 65536 {
        return Ok(crate::proof::fallback());
    }
    let _ = child.wait().await?;
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(crate::proof::fallback());
    };
    let sample = parse(platform, text);
    if sample == crate::proof::fallback() {
        return Ok(sample);
    }
    let mut tools = BTreeMap::new();
    for name in [
        "claude",
        "codex",
        "python3",
        "node",
        "bun",
        "ssh",
        "pm-mail",
        "browser-check",
        "simlock",
    ] {
        let present =
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).any(|p| {
                executable(&p.join(name))
                    || (cfg!(windows)
                        && ["exe", "cmd", "bat"]
                            .iter()
                            .any(|e| executable(&p.join(format!("{name}.{e}")))))
            });
        tools.insert(name, present);
    }
    Ok(json!({"schemaVersion":1,"machine":sample,"tools":tools}))
}
fn executable(path: &Path) -> bool {
    let Ok(m) = std::fs::metadata(path) else {
        return false;
    };
    if !m.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}
