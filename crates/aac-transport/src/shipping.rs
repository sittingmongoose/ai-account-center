use crate::{CallOptions, Computer, ProcessOutput};
use aac_config::{OperatingSystem, TransportConfig};
use aac_core::{Cause, Failure};
use aac_protocol::{EXIT_AGENT_MISSING, Event, Handshake, PROTOCOL_VERSION, Request, frame_cap};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

/// The single package policy shared with the labelled TS fallback.
pub fn artifact_cap() -> usize {
    static CAP: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CAP.get_or_init(|| {
        serde_json::from_str::<serde_json::Value>(include_str!(
            "../../../config/tables/agent-transport-policy.json"
        ))
        .ok()
        .and_then(|v| v["artifactMaxBytes"].as_u64())
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(0)
    })
}

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
    /// is refused before any connection or remote write occurs. Resolve the path once to an
    /// absolute spelling before verification; Local execution and SCP use that exact path.
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
            path: std::path::absolute(path)?,
            hash,
            expected,
        };
        artifact.verify()?;
        Ok(artifact)
    }

    fn verify(&self) -> Result<(), Failure> {
        let bytes = aac_files::read_bounded(&self.path, artifact_cap()).map_err(|_| {
            Failure::new(
                Cause::AgentMismatch,
                "pinned agent artifact could not be read",
            )
        })?;
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
    allow_shipping: bool,
    ready: Arc<Mutex<bool>>,
}

impl AgentClient {
    /// Pair a computer with a locally verified artifact; this does not contact it.
    pub fn new(computer: Computer, artifact: AgentArtifact) -> Self {
        Self {
            computer,
            artifact,
            allow_shipping: true,
            ready: Arc::new(Mutex::new(false)),
        }
    }

    /// Use only an already installed, pinned agent. Exit 87 or an old handshake refuses;
    /// this policy never prepares a cache, invokes SCP, or changes a remote file.
    pub fn existing(computer: Computer, artifact: AgentArtifact) -> Self {
        Self {
            allow_shipping: false,
            ..Self::new(computer, artifact)
        }
    }

    /// Share this registry entry's pooled computer with a fixed package helper.
    pub fn computer(&self) -> &Computer {
        &self.computer
    }

    /// Check SHA-256 before every use and cache a successful handshake for this client.
    /// When shipping is allowed, exit 87 permits exactly one upload to
    /// `.part`, remote verification and atomic rename. Other errors never trigger an upload.
    /// All stages share the original deadline, cancellation token and byte cap.
    pub async fn ensure(&self, options: &CallOptions) -> Result<(), Failure> {
        let _lease = tokio::select! {
            biased;
            _ = options.cancel.cancelled() => return Err(Failure::new(Cause::Cancelled, "shipping cancelled")),
            _ = tokio::time::sleep_until(options.deadline) => return Err(Failure::new(Cause::Timeout, "shipping deadline exceeded")),
            lease = self.computer.shipping_lock() => lease,
        };
        self.verify(options).await?;
        let mut ready = self.ready.lock().await;
        if *ready {
            return Ok(());
        }
        self.ensure_locked(options).await?;
        *ready = true;
        Ok(())
    }

    async fn ensure_locked(&self, options: &CallOptions) -> Result<(), Failure> {
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
        if !self.allow_shipping {
            return Err(Failure::new(
                Cause::AgentMismatch,
                "existing agent missing or damaged",
            ));
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
        self.verify(options).await?;
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

    async fn verify(&self, options: &CallOptions) -> Result<(), Failure> {
        static VERIFY: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> =
            std::sync::OnceLock::new();
        let semaphore = VERIFY
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(4)))
            .clone();
        let artifact = self.artifact.clone();
        tokio::select! {
            biased;
            _ = options.cancel.cancelled() => Err(Failure::new(Cause::Cancelled, "verification cancelled")),
            _ = tokio::time::sleep_until(options.deadline) => Err(Failure::new(Cause::Timeout, "verification deadline exceeded")),
            result = async move {
                let permit = semaphore.acquire_owned().await.map_err(|_| Failure::new(Cause::Bug, "verification pool closed"))?;
                tokio::task::spawn_blocking(move || { let _permit = permit; artifact.verify() }).await.map_err(|_| Failure::new(Cause::Bug, "verification failed"))?
            } => result,
        }
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
        let cap = frame_cap(&request.verb);
        if input.len() >= cap {
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
        if output.code == Some(EXIT_AGENT_MISSING) {
            *self.ready.lock().await = false;
        }
        self.events(request, output)
    }

    /// Same verified call with bounded later stdin frames. Validated progress is streamed;
    /// terminal events are returned only after the child exits and all output is checked.
    pub async fn call_controlled(
        &self,
        request: &Request,
        options: &CallOptions,
        controls: mpsc::Receiver<Vec<u8>>,
        progress: mpsc::Sender<Event>,
    ) -> Result<Vec<Event>, Failure> {
        let input = serde_json::to_vec(request)
            .map_err(|_| Failure::new(Cause::Refused, "request encoding failed"))?;
        self.call_controlled_encoded(request, &input, options, controls, progress)
            .await
    }

    /// Preserve raw argument spellings (duplicate keys, numbers and whitespace) across the
    /// service hop. The validated envelope must describe exactly these bytes.
    pub async fn call_controlled_encoded(
        &self,
        request: &Request,
        encoded: &[u8],
        options: &CallOptions,
        controls: mpsc::Receiver<Vec<u8>>,
        progress: mpsc::Sender<Event>,
    ) -> Result<Vec<Event>, Failure> {
        request.validate()?;
        let decoded: Request = serde_json::from_slice(encoded)
            .map_err(|_| Failure::new(Cause::Refused, "invalid request encoding"))?;
        if &decoded != request {
            return Err(Failure::new(Cause::Refused, "request encoding mismatch"));
        }
        self.ensure(options).await?;
        let mut input = encoded.to_vec();
        if input.len() >= frame_cap(&request.verb) {
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
        let (sender, mut chunks) = mpsc::channel(4);
        let execute =
            self.computer
                .execute_controlled(&argv, input.into(), options, controls, sender);
        tokio::pin!(execute);
        let mut buffer = Vec::new();
        let mut terminal = false;
        let mut output = None;
        loop {
            tokio::select! {
                result = &mut execute, if output.is_none() => { output = Some(result?); },
                chunk = chunks.recv() => {
                    let Some(chunk) = chunk else { break; };
                    buffer.extend(chunk);
                    while let Some(n) = buffer.iter().position(|c| *c == b'\n') {
                        if terminal || n >= frame_cap(&request.verb) { return Err(Failure::new(Cause::AgentMismatch, "invalid agent event ordering or size")); }
                        let event: Event = serde_json::from_slice(&buffer[..n]).map_err(|_| Failure::new(Cause::AgentMismatch, "invalid agent event JSON"))?;
                        check_event(request, &event)?;
                        terminal = !matches!(event, Event::Progress { .. });
                        if !terminal {
                            tokio::select! {
                                _ = options.cancel.cancelled() => return Err(Failure::new(Cause::Cancelled,"agent receiver closed")),
                                _ = tokio::time::sleep_until(options.deadline) => return Err(Failure::new(Cause::Timeout,"agent progress deadline")),
                                sent = progress.send(event) => sent.map_err(|_| Failure::new(Cause::Cancelled,"agent receiver closed"))?,
                            }
                        }
                        buffer.drain(..=n);
                    }
                    if buffer.len() >= frame_cap(&request.verb) { return Err(Failure::new(Cause::OutputTooLarge, "agent event cap exceeded")); }
                }
            }
        }
        let output = match output {
            Some(output) => output,
            None => execute.await?,
        };
        if output.code == Some(EXIT_AGENT_MISSING) {
            *self.ready.lock().await = false;
        }
        self.events(request, output)
    }

    /// [`Self::call_controlled_encoded`] for a request whose arguments and result are large (an
    /// Analytics scan: megabytes of prints in, up to the frame cap out). The envelope is written
    /// here around the caller's arguments text, which is checked without a value tree and sent
    /// as a shared part (never copied); the terminal result comes back as its JSON text, carved
    /// out of the agent's output without a copy or a value tree.
    #[allow(clippy::too_many_arguments)]
    pub async fn call_controlled_args(
        &self,
        verb: &str,
        id: &str,
        deadline_ms: u64,
        args: crate::StdinPart,
        options: &CallOptions,
        controls: mpsc::Receiver<Vec<u8>>,
        progress: mpsc::Sender<Event>,
    ) -> Result<RawTerminal, Failure> {
        let request = Request {
            protocol: PROTOCOL_VERSION,
            verb: verb.to_owned(),
            id: id.to_owned(),
            deadline_ms,
            args: raw_args((*args).as_ref())?,
        };
        request.validate()?;
        self.ensure(options).await?;
        let head = format!(
            "{{\"protocol\":{PROTOCOL_VERSION},\"verb\":{},\"id\":{},\"deadlineMs\":{deadline_ms},\"args\":",
            serde_json::Value::from(verb),
            serde_json::Value::from(id),
        )
        .into_bytes();
        let input = crate::Stdin::default()
            .part(Arc::new(head))
            .part(args)
            .part(Arc::new(b"}\n"));
        // The frame without its line end.
        if input.len() > frame_cap(&request.verb) {
            return Err(Failure::new(
                Cause::OutputTooLarge,
                "agent request cap exceeded",
            ));
        }
        let argv = if matches!(self.computer.config.transport, TransportConfig::Local) {
            vec![self.artifact.path.as_os_str().into()]
        } else {
            self.launcher("run")
        };
        let (sender, mut chunks) = mpsc::channel(4);
        let execute = self
            .computer
            .execute_controlled(&argv, input, options, controls, sender);
        tokio::pin!(execute);
        let mut buffer = Vec::new();
        let mut terminal = false;
        let mut output = None;
        loop {
            tokio::select! {
                result = &mut execute, if output.is_none() => { output = Some(result?); },
                chunk = chunks.recv() => {
                    let Some(chunk) = chunk else { break; };
                    if terminal {
                        // Only the terminal line can follow; the output check reads it.
                        continue;
                    }
                    buffer.extend(chunk);
                    while let Some(n) = buffer.iter().position(|c| *c == b'\n') {
                        if terminal || n >= frame_cap(&request.verb) { return Err(Failure::new(Cause::AgentMismatch, "invalid agent event ordering or size")); }
                        let head = RawEvent::read(&buffer[..n])?;
                        head.check(&request)?;
                        terminal = head.kind != "progress";
                        if !terminal {
                            let event: Event = serde_json::from_slice(&buffer[..n]).map_err(|_| Failure::new(Cause::AgentMismatch, "invalid agent event JSON"))?;
                            tokio::select! {
                                _ = options.cancel.cancelled() => return Err(Failure::new(Cause::Cancelled,"agent receiver closed")),
                                _ = tokio::time::sleep_until(options.deadline) => return Err(Failure::new(Cause::Timeout,"agent progress deadline")),
                                sent = progress.send(event) => sent.map_err(|_| Failure::new(Cause::Cancelled,"agent receiver closed"))?,
                            }
                        }
                        buffer.drain(..=n);
                    }
                    if terminal {
                        buffer = Vec::new();
                    } else if buffer.len() >= frame_cap(&request.verb) { return Err(Failure::new(Cause::OutputTooLarge, "agent event cap exceeded")); }
                }
            }
        }
        drop(buffer);
        let output = match output {
            Some(output) => output,
            None => execute.await?,
        };
        if output.code == Some(EXIT_AGENT_MISSING) {
            *self.ready.lock().await = false;
        }
        raw_terminal(&request, output)
    }

    fn events(&self, request: &Request, output: ProcessOutput) -> Result<Vec<Event>, Failure> {
        let cap = frame_cap(&request.verb);
        let mut events = Vec::new();
        let mut terminal = false;
        for line in output
            .stdout
            .split(|c| *c == b'\n')
            .filter(|line| !line.is_empty())
        {
            if terminal || line.len() >= cap {
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

/// The terminal event of a raw call: the result value's JSON text, or the agent's failure.
#[derive(Debug)]
pub enum RawTerminal {
    /// The `value` of the `result` event, as the agent wrote it.
    Result(Vec<u8>),
    /// The agent's terminal failure.
    Failure(Failure),
}

/// The envelope of an encoded request, checked as [`Request::validate`] checks a parsed one,
/// without a value tree of its arguments (only their `action`, which a deadline cap may read).
/// A large request's arguments, checked as a JSON object without a value tree: only its
/// `action` (which bounds the deadline) is read.
fn raw_args(args: &[u8]) -> Result<serde_json::Value, Failure> {
    use serde_json::value::RawValue;
    use std::collections::BTreeMap;
    let refused = || Failure::new(Cause::Refused, "invalid request encoding");
    if !args.starts_with(b"{") {
        return Err(Failure::new(Cause::Refused, "invalid request envelope"));
    }
    let entries: BTreeMap<String, &RawValue> =
        serde_json::from_slice(args).map_err(|_| refused())?;
    let mut reduced = serde_json::Map::new();
    if let Some(action) = entries.get("action") {
        reduced.insert(
            "action".into(),
            serde_json::from_str(action.get()).map_err(|_| refused())?,
        );
    }
    Ok(serde_json::Value::Object(reduced))
}

/// One output line's type, protocol and id, its other fields left as text.
struct RawEvent<'a> {
    kind: String,
    protocol: u32,
    id: String,
    value: Option<&'a serde_json::value::RawValue>,
}

impl<'a> RawEvent<'a> {
    fn read(line: &'a [u8]) -> Result<Self, Failure> {
        use serde_json::value::RawValue;
        let invalid = || Failure::new(Cause::AgentMismatch, "invalid agent event JSON");
        let fields: std::collections::BTreeMap<String, &'a RawValue> =
            serde_json::from_slice(line).map_err(|_| invalid())?;
        let text = |name: &str| fields.get(name).map(|raw| raw.get()).ok_or_else(invalid);
        let kind: String = serde_json::from_str(text("type")?).map_err(|_| invalid())?;
        if !matches!(kind.as_str(), "progress" | "result" | "failure") {
            return Err(invalid());
        }
        Ok(RawEvent {
            protocol: serde_json::from_str(text("protocol")?).map_err(|_| invalid())?,
            id: serde_json::from_str(text("id")?).map_err(|_| invalid())?,
            value: fields.get("value").copied(),
            kind,
        })
    }

    fn check(&self, request: &Request) -> Result<(), Failure> {
        if self.protocol != PROTOCOL_VERSION || self.id != request.id {
            return Err(Failure::new(
                Cause::AgentMismatch,
                "agent event correlation mismatch",
            ));
        }
        Ok(())
    }
}

/// [`AgentClient::events`] for a raw call: the same ordering, size, correlation and exit checks,
/// with the result value carved out of the output.
fn raw_terminal(request: &Request, output: ProcessOutput) -> Result<RawTerminal, Failure> {
    let cap = frame_cap(&request.verb);
    let mismatch = |detail: &'static str| Failure::new(Cause::AgentMismatch, detail);
    let stdout = &output.stdout;
    let mut terminal: Option<Result<(usize, usize), Failure>> = None;
    for line in stdout
        .split(|c| *c == b'\n')
        .filter(|line| !line.is_empty())
    {
        if terminal.is_some() || line.len() >= cap {
            return Err(mismatch("invalid agent event ordering or size"));
        }
        let event = RawEvent::read(line)?;
        event.check(request)?;
        match event.kind.as_str() {
            "progress" => {
                serde_json::from_slice::<Event>(line)
                    .map_err(|_| mismatch("invalid agent event JSON"))?;
            }
            "result" => {
                let value = event
                    .value
                    .ok_or_else(|| mismatch("invalid agent event JSON"))?;
                let start = value.get().as_ptr() as usize - stdout.as_ptr() as usize;
                terminal = Some(Ok((start, start + value.get().len())));
            }
            _ => match serde_json::from_slice::<Event>(line) {
                Ok(Event::Failure { failure, .. }) => terminal = Some(Err(failure)),
                _ => return Err(mismatch("invalid agent event JSON")),
            },
        }
    }
    let failed = matches!(terminal, Some(Err(_)));
    if terminal.is_none() || !stdout.ends_with(b"\n") || (output.code != Some(0) && !failed) {
        return Err(mismatch("agent did not complete its request"));
    }
    match terminal {
        Some(Ok((start, end))) => {
            let mut bytes = output.stdout;
            bytes.truncate(end);
            bytes.drain(..start);
            Ok(RawTerminal::Result(bytes))
        }
        Some(Err(failure)) => Ok(RawTerminal::Failure(failure)),
        None => Err(mismatch("agent did not complete its request")),
    }
}

fn check_event(request: &Request, event: &Event) -> Result<(), Failure> {
    let (protocol, id) = match event {
        Event::Progress { protocol, id, .. }
        | Event::Result { protocol, id, .. }
        | Event::Failure { protocol, id, .. } => (protocol, id),
    };
    if *protocol != PROTOCOL_VERSION || id != &request.id {
        return Err(Failure::new(
            Cause::AgentMismatch,
            "agent event correlation mismatch",
        ));
    }
    Ok(())
}
