//! Linux-only fresh idle Codex terminal relaunch (Python `app_update_confirmed_codex.py`);
//! no package or account change. A trusted caller supplies one captured context;
//! its environment stays in memory. Only a fixed installed standalone or official
//! npm Codex native binary can be launched.
use crate::apps::{
    Host,
    updater::{Context, Installed, Platform, Process, terminal},
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const FAILED: &str = "The confirmed Codex CLI could not reopen in a fresh terminal.";

fn own_uid() -> u32 {
    #[cfg(unix)]
    {
        nix::unistd::getuid().as_raw()
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn parts(path: &Path, root: &Path) -> Option<Vec<String>> {
    let relative = path.strip_prefix(crate::apps::real(root)).ok()?;
    Some(
        relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect(),
    )
}

fn release_word(word: &str) -> bool {
    let bytes = word.as_bytes();
    (1..=128).contains(&bytes.len())
        && bytes[0].is_ascii_digit()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Python `installed_codex(candidate)`: only fixed standalone or official installed npm
/// native binary layouts; `None` for anything else.
pub(super) fn installed_codex(host: &Host, candidate: &Path) -> Option<PathBuf> {
    let candidate = crate::apps::real(candidate);
    let info = std::fs::metadata(&candidate).ok()?;
    #[cfg(unix)]
    let owner = {
        use std::os::unix::fs::MetadataExt as _;
        info.uid()
    };
    #[cfg(not(unix))]
    let owner = 0;
    if candidate.file_name()? != "codex" || !info.is_file() || (owner != 0 && owner != own_uid()) {
        return None;
    }
    let releases = host.home.join(".codex/packages/standalone/releases");
    if let Some(relative) = parts(&candidate, &releases)
        && relative.len() == 3
        && relative[1] == "bin"
        && relative[2] == "codex"
        && release_word(&relative[0])
    {
        return Some(candidate);
    }
    let roots = [
        host.home.join(".local/lib/node_modules/@openai/codex"),
        host.system("/usr/local/lib/node_modules/@openai/codex"),
        host.system("/usr/lib/node_modules/@openai/codex"),
    ];
    let architectures = ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"];
    for root in roots {
        let Some(relative) = parts(&candidate, &root) else {
            continue;
        };
        let info_path = root.join("package.json");
        if std::fs::metadata(&info_path).map_or(true, |meta| meta.len() > 65_536)
            || crate::apps::read_json(&info_path, 65_536)
                .is_none_or(|package| package["name"] != "@openai/codex")
        {
            continue;
        }
        let words: Vec<&str> = relative.iter().map(String::as_str).collect();
        let nested = words.len() == 7
            && words[..2] == ["node_modules", "@openai"]
            && matches!(words[2], "codex-linux-x64" | "codex-linux-arm64")
            && words[3] == "vendor"
            && architectures.contains(&words[4])
            && words[5..] == ["bin", "codex"];
        let bundled = words.len() == 4
            && words[0] == "vendor"
            && architectures.contains(&words[1])
            && (words[2..] == ["bin", "codex"] || words[2..] == ["codex", "codex"]);
        if nested || bundled {
            return Some(candidate);
        }
    }
    None
}

/// The validated relaunch context (Python `restart`'s checks), or `None`.
pub(super) fn confirmed_context(host: &Host, platform: Platform, value: &Value) -> Option<Process> {
    if platform != Platform::Ubuntu || !cfg!(target_os = "linux") {
        return None;
    }
    let fields = value.as_object()?;
    let mut keys: Vec<&str> = fields.keys().map(String::as_str).collect();
    keys.sort_unstable();
    if keys != ["cwd", "env", "exe"] && keys != ["args", "cwd", "env", "exe"] {
        return None;
    }
    let active = installed_codex(host, Path::new(fields["exe"].as_str()?))?;
    let cwd = fields["cwd"].as_str()?;
    let args = fields
        .get("args")
        .map_or(Some(true), |args| args.as_array().map(Vec::is_empty))?;
    let env = fields["env"].as_object()?;
    if !Path::new(cwd).is_absolute() || !Path::new(cwd).is_dir() || !args || env.len() > 4096 {
        return None;
    }
    let mut pairs = Vec::with_capacity(env.len());
    for (key, content) in env {
        let content = content.as_str()?;
        if key.is_empty() || key.contains(['=', '\0']) || content.contains('\0') {
            return None;
        }
        pairs.push((key.clone(), content.to_owned()));
    }
    let lookup = |name: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    // Python `env.get('CODEX_HOME') or Path(env.get('HOME', home)) / '.codex'`.
    let codex_home = match lookup("CODEX_HOME").filter(|value| !value.is_empty()) {
        Some(home) => PathBuf::from(home),
        None => lookup("HOME")
            .map_or_else(|| host.home.clone(), PathBuf::from)
            .join(".codex"),
    };
    if crate::apps::real(&codex_home) != crate::apps::real(&host.home.join(".codex")) {
        return None;
    }
    let exe = active.to_string_lossy().into_owned();
    Some(Process {
        pid: 0,
        ppid: 0,
        uid: own_uid(),
        exe: exe.clone(),
        identity: "new".into(),
        args: vec![exe],
        cwd: Some(cwd.to_owned()),
        env: pairs,
        session: None,
    })
}

/// Python `app_update_confirmed_codex.main`: reopen one confirmed idle Codex CLI in a
/// fresh terminal (`check` only verifies it could). Always an answer object:
/// `{"success":true[, "restartTargets"]}` or the fixed failure message.
pub(crate) async fn confirmed_codex_restart(cx: &Context, value: &Value, check: bool) -> Value {
    let refused = || json!({"success": false, "message": FAILED});
    let Some(context) = confirmed_context(&cx.host, cx.platform, value) else {
        return refused();
    };
    let contexts = [context];
    if terminal::check_terminal(cx, &contexts).await.is_err() {
        return refused();
    }
    if check {
        return json!({"success": true});
    }
    let install = Installed::new(
        "codex-cli",
        Platform::Ubuntu,
        PathBuf::from(&contexts[0].exe),
        None,
        "native",
    );
    match terminal::restart_cli(cx, &install, &contexts).await {
        Ok(targets) => json!({"success": true, "restartTargets": targets}),
        Err(_) => refused(),
    }
}
