//! Transcript metadata checks use a fixed POSIX program and the pinned endpoint over SSH.
use super::*;
use std::process::{Command, Stdio};
const PROBE: &str = r#"ok=true
while IFS= read -r name; do
 if [ ! -f "$name" ] || [ -L "$name" ] || [ "$(/usr/bin/stat -c %h -- "$name" 2>/dev/null)" != 1 ]; then ok=false; fi
 p=${name%/*}
 while :; do
  if [ ! -d "$p" ] || [ -L "$p" ]; then ok=false; fi
  [ "$p" = / ] && break
  p=${p%/*}; [ -n "$p" ] || p=/
 done
done
[ "$ok" = true ] && printf '{"verified":true}' || { printf '{"verified":false}'; exit 1; }
"#;
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}
fn environment(cmd: &mut Command) {
    cmd.env_clear();
    for (k, v) in std::env::vars_os() {
        let name = k.to_string_lossy();
        let name = if cfg!(windows) {
            name.to_ascii_uppercase()
        } else {
            name.into_owned()
        };
        if ["PATH", "HOME", "USER", "LOGNAME", "LANG", "SSH_AUTH_SOCK"].contains(&name.as_str())
            || cfg!(windows) && ["SYSTEMROOT", "USERPROFILE", "WINDIR"].contains(&name.as_str())
        {
            cmd.env(k, v);
        }
    }
}
fn executable() -> Checked<PathBuf> {
    let path = std::env::var_os("PATH").ok_or(())?;
    for p in std::env::split_paths(&path) {
        let p = p.join(if cfg!(windows) { "ssh.exe" } else { "ssh" });
        if p.is_file() {
            return fs::canonicalize(p).map_err(|_| ());
        }
    }
    Err(())
}
fn identity(p: &Path) -> Checked<String> {
    let key = aac_files::history_transaction::identity(p).map_err(|_| ())?;
    let m = fs::metadata(p).map_err(|_| ())?;
    #[cfg(unix)]
    let change = {
        use std::os::unix::fs::MetadataExt;
        Some((m.ctime(), m.ctime_nsec()))
    };
    #[cfg(not(unix))]
    let change: Option<(i64, i64)> = None;
    Ok(format!(
        "{key:?}:{}:{:?}:{change:?}",
        m.len(),
        m.modified().map_err(|_| ())?
    ))
}

fn options(mut probe: impl FnMut(&str) -> Checked<(bool, Vec<u8>)>) -> Checked<Vec<String>> {
    let mut options = vec![];
    for flag in ["KnownHostsCommand=none", "SetEnv=AAC_SSH_POLICY=1"] {
        let (ok, err) = probe(flag)?;
        if ok {
            options.extend(["-o".into(), flag.into()]);
        } else if !String::from_utf8_lossy(&err)
            .to_ascii_lowercase()
            .contains("bad configuration option:")
        {
            return Err(());
        }
    }
    Ok(options)
}
fn argv(
    bound: &Bound<'_>,
    known: &Path,
    identity: Option<&Path>,
    capabilities: Vec<String>,
) -> Checked<Vec<String>> {
    let endpoint = &bound.policy["ssh"][bound.platform];
    let safe = |p: &Path| -> Checked<String> {
        let p = p.to_str().ok_or(())?;
        if !Path::new(p).is_absolute() || p.chars().any(|c| matches!(c, '\n' | '\r' | '%' | '"')) {
            return Err(());
        }
        Ok(p.into())
    };
    let mut out = vec!["-T".into()];
    out.extend(capabilities);
    for option in [
        "BatchMode=yes",
        "StrictHostKeyChecking=yes",
        "VerifyHostKeyDNS=no",
        "UpdateHostKeys=no",
        "ForwardAgent=no",
        "ForwardX11=no",
        "GlobalKnownHostsFile=none",
    ] {
        out.extend(["-o".into(), option.into()]);
    }
    if let Some(p) = identity {
        out.extend([
            "-i".into(),
            safe(p)?,
            "-o".into(),
            "IdentitiesOnly=yes".into(),
        ]);
    }
    out.extend([
        "-o".into(),
        format!("UserKnownHostsFile=\"{}\"", safe(known)?),
        "-o".into(),
        format!("Hostname={}", text(&endpoint["hostname"])?),
        "-l".into(),
        text(&endpoint["username"])?.into(),
        "-p".into(),
        endpoint["port"].as_u64().ok_or(())?.to_string(),
    ]);
    for option in [
        "ConnectTimeout=5",
        "ConnectionAttempts=1",
        "ControlMaster=no",
        "ControlPath=none",
        "PermitLocalCommand=no",
        "ClearAllForwardings=yes",
    ] {
        out.extend(["-o".into(), option.into()]);
    }
    out.extend([
        "--".into(),
        text(&endpoint["alias"])?.into(),
        format!("/bin/sh -c {}", quote(PROBE)),
    ]);
    Ok(out)
}
pub(super) fn verify(bound: &Bound<'_>, args: &Value) -> Checked<Value> {
    bound.bind()?;
    let paths = write::references(&args["records"], bound.policy)?;
    if let Some(f) = bound.fixture {
        return if f["transcriptsVerified"] == true {
            Ok(json!({"verified":true}))
        } else {
            Err(())
        };
    }
    let executable = executable()?;
    let before = identity(&executable)?;
    let capabilities = options(|flag| {
        let mut cmd = Command::new(&executable);
        environment(&mut cmd);
        cmd.args(["-G", "-F", "none", "-o", flag, "--", "localhost"]);
        let (status, _, err) = native::capture_command(cmd, Vec::new(), 8192, 2)?;
        Ok((status.success(), err))
    })?;
    if identity(&executable)? != before {
        return Err(());
    }
    let home = std::env::var_os("CCS_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| bound.home.clone());
    let known = std::env::var_os("AAC_AGENT_KNOWN_HOSTS_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".ssh/known_hosts"));
    let key = std::env::var_os("AAC_AGENT_IDENTITY_FILE")
        .map(PathBuf::from)
        .map(|p| {
            if let Some(s) = p.to_str().and_then(|s| s.strip_prefix("~/")) {
                home.join(s)
            } else {
                p
            }
        });
    let argv = argv(bound, &known, key.as_deref(), capabilities)?;
    if identity(&executable)? != before {
        return Err(());
    }
    let mut cmd = Command::new(&executable);
    environment(&mut cmd);
    cmd.args(&argv).stdin(Stdio::piped());
    let input = paths.join("\n") + if paths.is_empty() { "" } else { "\n" };
    let (status, raw, _) = native::capture_command(cmd, input.into_bytes(), 128, 12)?;
    if !status.success()
        || serde_json::from_slice::<Value>(&raw).ok() != Some(json!({"verified":true}))
    {
        return Err(());
    }
    bound.bind()?;
    Ok(json!({"verified":true}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capability_downgrade_is_only_for_unsupported_options() {
        assert_eq!(
            options(|_| Ok((false, b"Bad configuration option: unknown".to_vec())))
                .expect("known downgrade"),
            Vec::<String>::new()
        );
        assert!(options(|_| Ok((false, b"permission denied".to_vec()))).is_err());
        assert_eq!(
            options(|_| Ok((true, vec![]))).expect("modern ssh").len(),
            4
        );
    }
}
