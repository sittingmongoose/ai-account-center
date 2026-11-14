use crate::{CallOptions, Computer, ProcessOutput};
use aac_config::{OperatingSystem, TransportConfig};
use aac_core::{Cause, Failure};
use aac_protocol::{
    EXIT_AGENT_MISSING, Event, Handshake, MAX_FRAME_BYTES, PROTOCOL_VERSION, Request,
};
use std::ffi::OsString;
use std::path::PathBuf;

/// An artifact verified locally against an explicit SHA-256 manifest and handshake.
/// Binary contents are never formatted in diagnostics; the source path is used only for SCP.
#[derive(Clone)]
pub struct AgentArtifact {
    path: PathBuf,
    hash: String,
    expected: Handshake,
}

impl AgentArtifact {
    /// Verify a regular non-symlink artifact up to 64 MiB. A mismatched or malformed manifest
    /// is refused before any connection or remote write occurs.
    pub fn load(path: PathBuf, hash: String, expected: Handshake) -> Result<Self, Failure> {
        if hash.len() != 64
            || !hash
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            || expected.name != "aac-agent"
            || expected.protocol != PROTOCOL_VERSION
            || expected.version.is_empty()
            || expected.target.is_empty()
            || !expected
                .version
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b".-+".contains(&c))
            || !expected
                .target
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
        {
            return Err(Failure::new(Cause::AgentMismatch, "invalid agent manifest"));
        }
        let artifact = Self {
            path,
            hash,
            expected,
        };
        artifact.verify()?;
        Ok(artifact)
    }

    fn verify(&self) -> Result<(), Failure> {
        let bytes = aac_files::read_bounded(&self.path, 64 * 1024 * 1024)?;
        if aac_core::sha256_hex(&bytes) != self.hash {
            return Err(Failure::new(
                Cause::AgentMismatch,
                "agent artifact hash mismatch",
            ));
        }
        Ok(())
    }
}

/// Hash-pinned health/agent client. Shipping is explicit through `ensure` or `call`; callers
/// authorize cache-only upload by constructing the client with an artifact manifest.
#[derive(Clone)]
pub struct AgentClient {
    computer: Computer,
    artifact: AgentArtifact,
}

impl AgentClient {
    /// Pair a computer with a locally verified artifact; this does not contact it.
    pub fn new(computer: Computer, artifact: AgentArtifact) -> Self {
        Self { computer, artifact }
    }

    /// Check SHA-256 and handshake before every use. Exit 87 permits exactly one upload to
    /// `.part`, remote verification and atomic rename. Other errors never trigger an upload.
    /// All stages share the original deadline, cancellation token and byte cap.
    pub async fn ensure(&self, options: &CallOptions) -> Result<(), Failure> {
        let _lease = tokio::select! {
            biased;
            _ = options.cancel.cancelled() => return Err(Failure::new(Cause::Cancelled, "shipping cancelled")),
            _ = tokio::time::sleep_until(options.deadline) => return Err(Failure::new(Cause::Timeout, "shipping deadline exceeded")),
            lease = self.computer.shipping_lock() => lease,
        };
        self.ensure_locked(options).await
    }

    async fn ensure_locked(&self, options: &CallOptions) -> Result<(), Failure> {
        self.artifact.verify()?;
        if matches!(self.computer.config.transport, TransportConfig::Local) {
            let output = self
                .computer
                .execute(
                    &[self.artifact.path.as_os_str().into(), "--version".into()],
                    &[],
                    options,
                )
                .await?;
            return self.handshake(output);
        }
        let output = self
            .computer
            .execute(&self.launcher("probe"), &[], options)
            .await?;
        if output.code == Some(0) {
            return self.handshake(output);
        }
        if output.code != Some(EXIT_AGENT_MISSING) {
            return Err(Failure::new(Cause::AgentMismatch, "agent launcher failed"));
        }
        let prepared = self
            .computer
            .execute(&self.launcher("prepare"), &[], options)
            .await?;
        if prepared.code != Some(0) {
            return Err(Failure::new(
                Cause::NotWritable,
                "agent cache preparation failed",
            ));
        }
        let part = if self.computer.config.os == OperatingSystem::Windows {
            let prepared: serde_json::Value =
                serde_json::from_slice(&prepared.stdout).map_err(|_| {
                    Failure::new(Cause::AgentMismatch, "invalid agent cache path reply")
                })?;
            let path = prepared
                .get("partPath")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| Failure::new(Cause::AgentMismatch, "missing agent cache path"))?;
            let path = path.replace('\\', "/");
            if path.len() > 4096
                || path.chars().any(char::is_control)
                || !path.ends_with(&format!("/aac-agent-{}.part", self.artifact.hash))
                || !(path.starts_with('/')
                    || (path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
                        && path.as_bytes().get(1) == Some(&b':')))
            {
                return Err(Failure::new(
                    Cause::AgentMismatch,
                    "invalid agent cache upload path",
                ));
            }
            path
        } else {
            format!(
                ".cache/ai-account-center/bin/aac-agent-{}.part",
                self.artifact.hash
            )
        };
        self.artifact.verify()?;
        let upload = self
            .computer
            .scp(&self.artifact.path, &part, options)
            .await?;
        if upload.code != Some(0) {
            return Err(Failure::new(Cause::NotWritable, "agent upload failed"));
        }
        let installed = self
            .computer
            .execute(&self.launcher("commit"), &[], options)
            .await?;
        self.handshake(installed)
    }

    /// Execute a request after verifying its agent, then validate bounded NDJSON correlation,
    /// version and terminal ordering. A failure event remains a failure, with no value invented.
    pub async fn call(
        &self,
        request: &Request,
        options: &CallOptions,
    ) -> Result<Vec<Event>, Failure> {
        request.validate()?;
        self.ensure(options).await?;
        let mut input = serde_json::to_vec(request)
            .map_err(|_| Failure::new(Cause::Refused, "request encoding failed"))?;
        if input.len() >= MAX_FRAME_BYTES {
            return Err(Failure::new(
                Cause::OutputTooLarge,
                "agent request cap exceeded",
            ));
        }
        input.push(b'\n');
        let argv = if matches!(self.computer.config.transport, TransportConfig::Local) {
            vec![self.artifact.path.as_os_str().into()]
        } else {
            self.launcher("run")
        };
        let output = self.computer.execute(&argv, &input, options).await?;
        let mut events = Vec::new();
        let mut terminal = false;
        for line in output
            .stdout
            .split(|c| *c == b'\n')
            .filter(|line| !line.is_empty())
        {
            if terminal || line.len() > MAX_FRAME_BYTES {
                return Err(Failure::new(
                    Cause::AgentMismatch,
                    "invalid agent event ordering or size",
                ));
            }
            let event: Event = serde_json::from_slice(line)
                .map_err(|_| Failure::new(Cause::AgentMismatch, "invalid agent event JSON"))?;
            let (protocol, id) = match &event {
                Event::Progress { protocol, id, .. } => (protocol, id),
                Event::Result { protocol, id, .. } | Event::Failure { protocol, id, .. } => {
                    terminal = true;
                    (protocol, id)
                }
            };
            if *protocol != PROTOCOL_VERSION || id != &request.id {
                return Err(Failure::new(
                    Cause::AgentMismatch,
                    "agent event correlation mismatch",
                ));
            }
            events.push(event);
        }
        if !terminal
            || !output.stdout.ends_with(b"\n")
            || (output.code != Some(0) && !matches!(events.last(), Some(Event::Failure { .. })))
        {
            return Err(Failure::new(
                Cause::AgentMismatch,
                "agent did not complete its request",
            ));
        }
        Ok(events)
    }

    fn handshake(&self, output: ProcessOutput) -> Result<(), Failure> {
        if output.code != Some(0) || output.stdout.len() > 4096 {
            return Err(Failure::new(Cause::AgentMismatch, "agent handshake failed"));
        }
        let actual: Handshake = serde_json::from_slice(&output.stdout)
            .map_err(|_| Failure::new(Cause::AgentMismatch, "invalid agent handshake"))?;
        if actual != self.artifact.expected {
            return Err(Failure::new(
                Cause::AgentMismatch,
                "agent identity or version mismatch",
            ));
        }
        Ok(())
    }

    fn launcher(&self, mode: &str) -> Vec<OsString> {
        let hash = &self.artifact.hash;
        if self.computer.config.os == OperatingSystem::Windows {
            let script = format!(
                r#"$ErrorActionPreference='Stop'; $d=Join-Path $env:LOCALAPPDATA 'ai-account-center\bin'; $f=Join-Path $d 'aac-agent.exe'; $p=Join-Path $d 'aac-agent-{hash}.part'; $h='{hash}'; $mode='{mode}';
if ($mode -eq 'prepare') {{ New-Item -ItemType Directory -Force -Path $d | Out-Null; [Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); @{{partPath=$p}} | ConvertTo-Json -Compress; exit 0 }}
if ($mode -eq 'commit') {{ if (!(Test-Path -LiteralPath $p -PathType Leaf) -or (Get-FileHash -LiteralPath $p -Algorithm SHA256).Hash.ToLowerInvariant() -ne $h) {{ exit 87 }}; if (Test-Path -LiteralPath $f) {{ [System.IO.File]::Replace($p,$f,$null) }} else {{ [System.IO.File]::Move($p,$f) }} }}
if (!(Test-Path -LiteralPath $f -PathType Leaf) -or (Get-FileHash -LiteralPath $f -Algorithm SHA256).Hash.ToLowerInvariant() -ne $h) {{ exit 87 }}
if ($mode -eq 'run') {{ & $f }} else {{ & $f --version }}; exit $LASTEXITCODE"#
            );
            vec![
                "powershell.exe".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-EncodedCommand".into(),
                crate::process::encode_powershell(&script).into(),
            ]
        } else {
            let script = format!(
                r#"set -eu
umask 077
d="$HOME/.cache/ai-account-center/bin"
f="$d/aac-agent"
p="$d/aac-agent-{hash}.part"
h='{hash}'
hash_file() {{ if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d ' ' -f 1; else shasum -a 256 "$1" | cut -d ' ' -f 1; fi; }}
case '{mode}' in
prepare) mkdir -p "$d"; [ ! -L "$d" ]; chmod 700 "$d"; exit 0 ;;
commit) [ -f "$p" ] && [ ! -L "$p" ] && [ "$(hash_file "$p")" = "$h" ] || exit 87; chmod 700 "$p"; mv -f "$p" "$f" ;;
esac
[ -f "$f" ] && [ ! -L "$f" ] && [ "$(hash_file "$f")" = "$h" ] || exit 87
if [ '{mode}' = run ]; then exec "$f"; else exec "$f" --version; fi"#
            );
            vec!["sh".into(), "-c".into(), script.into()]
        }
    }
}
