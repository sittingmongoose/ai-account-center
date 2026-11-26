//! Offline snapshot reader. No provider or dashboard requests are made here.
mod render;
mod rules;
use crate::js;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Default, Debug)]
/// Parsed offline reader arguments.
pub struct Options {
    /// Selected view or rules operation.
    pub command: String,
    /// Explicit snapshot directory.
    pub directory: Option<PathBuf>,
    /// Canonical provider name.
    pub provider: Option<String>,
    /// Case-insensitive account substring.
    pub account: Option<String>,
    /// Case-insensitive history window substring.
    pub window: Option<String>,
    /// Include hidden accounts.
    pub all: bool,
    /// Render JSON instead of text.
    pub json: bool,
    /// History lookback in hours.
    pub hours: f64,
    /// Rules consumer kind.
    pub kind: String,
}
impl Options {
    /// Parse options before or after a subcommand.
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut out = Self {
            hours: 168.,
            kind: "other".into(),
            ..Self::default()
        };
        let mut i = 0;
        while i < args.len() {
            let arg = args[i].as_str();
            match arg {
                "--all" => out.all = true,
                "--json" => out.json = true,
                "--dir" | "--provider" | "--account" | "--window" | "--hours" => {
                    i += 1;
                    let value = args
                        .get(i)
                        .ok_or_else(|| format!("aac-usage: {arg} needs a value"))?;
                    match arg {
                        "--dir" => out.directory = Some(value.into()),
                        "--provider" => {
                            out.provider = Some(
                                provider(value)
                                    .ok_or_else(|| {
                                        format!("aac-usage: unknown provider {value:?}")
                                    })?
                                    .into(),
                            )
                        }
                        "--account" => out.account = Some(value.to_lowercase()),
                        "--window" => out.window = Some(value.to_lowercase()),
                        _ => out.hours = value.parse().map_err(|_| "aac-usage: invalid hours")?,
                    }
                }
                "history" | "signins" | "rules" | "install-rules" if out.command.is_empty() => {
                    out.command = arg.into()
                }
                "claude" | "other" if out.command == "rules" => out.kind = arg.into(),
                _ => return Err(format!("aac-usage: unrecognized argument {arg}")),
            }
            i += 1;
        }
        if !out.hours.is_finite() {
            return Err("aac-usage: invalid hours".into());
        }
        Ok(out)
    }
}
fn provider(s: &str) -> Option<&'static str> {
    Some(match s.to_lowercase().as_str() {
        "codex" | "openai" | "chatgpt" => "codex",
        "claude" | "anthropic" => "claude",
        "antigravity" | "agy" | "gemini" => "antigravity",
        "muse" => "muse",
        "cursor" | "grok" => "cursor",
        "kimi" | "kimi-code" => "kimi-code",
        "qwen" => "qwen",
        "zai" | "z.ai" | "glm" | "zcode" => "zai",
        "opencode" | "opencode-go" => "opencode-go",
        _ => return None,
    })
}
pub(super) fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
pub(super) fn rows<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v[k].as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn matches(v: &Value, o: &Options, id: &str) -> bool {
    (!js::truthy(&v["hidden"]) || o.all)
        && o.provider.as_ref().is_none_or(|p| s(v, "provider") == p)
        && o.account.as_ref().is_none_or(|n| {
            ["label", "email", "profile", id]
                .iter()
                .any(|k| s(v, k).to_lowercase().contains(n))
        })
}
pub(super) fn time(v: &Value, k: &str) -> Option<i64> {
    js::date_parse(&v[k])
}
pub(super) fn sampled(w: &Value, a: &Value) -> Option<i64> {
    time(w, "sampledAt").or_else(|| time(a, "sampledAt"))
}
pub(super) fn passed(w: &Value, a: &Value, now: i64) -> bool {
    js::truthy(&w["resetPassed"])
        || time(w, "resetAt").is_some_and(|r| r <= now && sampled(w, a).is_none_or(|t| t < r))
}
pub(super) fn current(w: &Value, a: &Value, now: i64, limit: f64) -> bool {
    sampled(w, a).is_some_and(|t| (now - t) as f64 <= limit * 1000.)
}
pub(super) fn stale_limit(v: &Value) -> f64 {
    v["staleAfterSeconds"]
        .as_f64()
        .filter(|x| *x != 0.)
        .unwrap_or(300.)
}
/// Filter a snapshot and remove paces invalid at read time.
pub fn filter_snapshot(mut v: Value, o: &Options, now: i64) -> Value {
    let limit = stale_limit(&v);
    let mut accounts = rows(&v, "accounts")
        .iter()
        .filter(|a| matches(a, o, "id"))
        .cloned()
        .collect::<Vec<_>>();
    for a in &mut accounts {
        let meta = a.clone();
        if let Some(windows) = a["windows"].as_array_mut() {
            for w in windows {
                if !js::truthy(&w["resetPassed"]) && passed(w, &meta, now) {
                    w["resetPassed"] = true.into();
                    w["burn"] = Value::Null;
                }
                if !current(w, &meta, now, limit) {
                    w["burn"] = Value::Null;
                }
            }
        }
    }
    v["accounts"] = accounts.into();
    if o.provider.is_some() && v.get("signIn").is_some() {
        v["signIn"] = rows(&v, "signIn")
            .iter()
            .filter(|r| Some(s(r, "provider")) == o.provider.as_deref())
            .cloned()
            .collect::<Vec<_>>()
            .into();
    }
    v
}
fn load(folder: &Path, name: &str) -> Result<Value, String> {
    let p = folder.join(name);
    let raw = std::fs::read(&p)
        .map_err(|e| format!("aac-usage: {} could not be read ({e}).", p.display()))?;
    serde_json::from_slice(&raw)
        .map_err(|e| format!("aac-usage: {} could not be read ({e}).", p.display()))
}
/// Resolve the published native reader's sibling snapshot, after explicit and environment paths.
pub fn snapshot_folder(explicit: Option<&Path>) -> Option<PathBuf> {
    explicit
        .map(Path::to_path_buf)
        .or_else(|| {
            std::env::var_os("AAC_USAGE_DIR")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| {
            let exe = std::env::current_exe().ok()?;
            let p = exe.parent()?;
            p.join("usage.json").exists().then(|| p.to_path_buf())
        })
}
/// Render one offline command at the supplied UTC millisecond clock.
pub fn run(o: &Options, now: i64) -> Result<String, String> {
    let folder = snapshot_folder(o.directory.as_deref());
    if o.command == "rules" {
        return Ok(rules::text(folder.as_deref()));
    }
    if o.command == "install-rules" {
        return rules::install(folder.as_deref());
    }
    let folder=folder.ok_or("aac-usage: no snapshot folder. Run the copy in the folder the AI Account Center dashboard writes to, or pass --dir <folder> or set AAC_USAGE_DIR.")?;
    let labels = load(&folder, "computers.json").unwrap_or_else(|_| {
        serde_json::from_str(include_str!(
            "../../../../web-dashboard/public/computers.json"
        ))
        .unwrap_or(Value::Null)
    });
    let signins = load(&folder, "claude-signins.json")
        .ok()
        .filter(|v| v["version"] == 1);
    match o.command.as_str() {
        "signins" => {
            let v = signins.ok_or_else(|| {
                format!(
                    "aac-usage: no Claude sign-in renewal runner publishes into {}.",
                    folder.display()
                )
            })?;
            if o.json {
                pretty(&v)
            } else {
                Ok(render::signins(&v, o, now, &labels))
            }
        }
        "history" => {
            let mut v = load(&folder, "history.json")?;
            let since = now as f64 - o.hours * 3_600_000.;
            let series = rows(&v, "series")
                .iter()
                .filter(|r| {
                    matches(r, o, "accountId")
                        && o.window
                            .as_ref()
                            .is_none_or(|w| s(r, "window").to_lowercase().contains(w))
                })
                .map(|r| {
                    let mut r = r.clone();
                    r["points"] = rows(&r, "points")
                        .iter()
                        .filter(|p| js::date_parse(&p[0]).is_none_or(|t| t as f64 >= since))
                        .cloned()
                        .collect::<Vec<_>>()
                        .into();
                    r
                })
                .collect::<Vec<_>>();
            v["series"] = series.into();
            if o.json {
                pretty(&v)
            } else {
                let snapshot = load(&folder, "usage.json")?;
                v["staleAfterSeconds"] = snapshot["staleAfterSeconds"].clone();
                Ok(render::history(&v, now))
            }
        }
        _ => {
            let v = load(&folder, "usage.json")?;
            if o.json {
                pretty(&filter_snapshot(v, o, now))
            } else {
                Ok(render::show(&v, o, now, &folder, &labels, signins.as_ref()))
            }
        }
    }
}
fn pretty(v: &Value) -> Result<String, String> {
    serde_json::to_string_pretty(v)
        .map(|s| {
            let mut out = String::new();
            for c in s.chars() {
                if c.is_ascii() {
                    out.push(c);
                } else {
                    let mut buf = [0; 2];
                    for unit in c.encode_utf16(&mut buf) {
                        out.push_str(&format!("\\u{unit:04x}"));
                    }
                }
            }
            out.push('\n');
            out
        })
        .map_err(|e| e.to_string())
}
/// Encode non-ASCII scalars using Python terminal backslash escapes.
pub fn ascii(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        let n = c as u32;
        if c.is_ascii() {
            out.push(c);
        } else if n <= 255 {
            out.push_str(&format!("\\x{n:02x}"));
        } else if n <= 65535 {
            out.push_str(&format!("\\u{n:04x}"));
        } else {
            out.push_str(&format!("\\U{n:08x}"));
        }
    }
    out
}
