//! Port of the Open orchestration: `claude-desktop-open-service.ts` (target resolution, the
//! managed-history decision, click coalescing and the launcher call), the route flow of
//! `claude-desktop-routes.ts`' POST open (join, 202 progress, sync wait and the exact refusal
//! bodies) and the fixed Open command builders of `claude-desktop-transport.ts`. The launcher
//! runs through lane F's `claude-host` verb when the agent proves it; the reference SSH command
//! is the fallback, exactly as the TS transport falls back.
use super::manifest::{
    self, Launcher, Profile, can_open_mac, can_open_windows, list_profiles, profile_id_pattern,
    read_pending_profiles, windows_launcher_account_list,
};
use super::markers;
use super::operations::{self, OpenErrorKind, Operation, Operations};
use super::policy::{ExpectedLauncher, load_policy};
use super::sync_service::{self, HelperCall, SyncObserver};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

/// The fixed refusal sentence for an unconfirmed history copy.
pub use super::operations::CLAUDE_HISTORY_UNCONFIRMED_MESSAGE;

const SSH_TIMEOUT_MS: u64 = 8000;
const MAX_HISTORY_BYTES: usize = 1024 * 1024;
const MAX_WINDOWS_COMMAND: usize = 7600;
const WINDOWS_OPEN_BOOTSTRAP: &str = "$encoded = [Console]::In.ReadToEnd(); $script = [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String($encoded)); & ([scriptblock]::Create($script)); exit $LASTEXITCODE";
const RECENT_OPEN_MS: u64 = 1000;

/// One Open's progress callbacks (the TS `ClaudeOpenObserver`).
pub trait OpenObserver: Send + Sync {
    /// The optional history copy finished; `created_count` is its confirmed record count.
    fn synchronized(&self, status: &str, created_count: u64);
    /// The launcher is about to run.
    fn opening(&self);
    /// A bounded copy batch is about to be appended.
    fn copying(&self, total_count: u64, confirmed_count: u64);
}

impl OpenObserver for operations::Observer {
    fn synchronized(&self, status: &str, created_count: u64) {
        operations::Observer::synchronized(self, status, created_count);
    }
    fn opening(&self) {
        operations::Observer::opening(self);
    }
    fn copying(&self, total_count: u64, confirmed_count: u64) {
        operations::Observer::copying(self, total_count, confirmed_count);
    }
}

/// The ordinary Open's observer: no progress is tracked for it.
pub struct NoOpenObserver;
impl OpenObserver for NoOpenObserver {
    fn synchronized(&self, _status: &str, _created_count: u64) {}
    fn opening(&self) {}
    fn copying(&self, _total_count: u64, _confirmed_count: u64) {}
}

/// A sync observer over an Open observer.
struct ObserverSync<'a>(&'a dyn OpenObserver);
impl SyncObserver for ObserverSync<'_> {
    fn copying(&self, total_count: u64, confirmed_count: u64) {
        self.0.copying(total_count, confirmed_count);
    }
}

/// The remote boundary: lane F's launcher verb, lane E's history verb and the reference SSH
/// commands. The production implementation lives in `remote`; fixtures implement it in memory.
pub trait Remote: HelperCall {
    /// Try the agent launcher and distinguish a definite non-dispatch from a transport error.
    fn host_open<'a>(
        &'a self,
        platform: &'a str,
        ssh_host: &'a str,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Result<AgentOpen, SshError>> + Send + 'a>>;

    /// One fixed reference command over the existing SSH policy.
    fn ssh_exec<'a>(
        &'a self,
        platform: &'a str,
        ssh_host: &'a str,
        argv: Vec<String>,
        stdin: Option<Vec<u8>>,
        timeout_ms: u64,
        max_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Result<SshOutput, SshError>> + Send + 'a>>;
}

/// A proven outcome from the agent's Open attempt.
#[derive(PartialEq, Eq)]
pub enum AgentOpen {
    /// The agent confirmed launch.
    Opened,
    /// The agent definitely did not run the launcher.
    NotDispatched,
}

/// Captured reference command output.
pub struct SshOutput {
    /// The exit code, absent when a signal ended the child.
    pub code: Option<i32>,
    /// Bounded stdout bytes.
    pub stdout: Vec<u8>,
}

/// How a reference command failed (`ClaudeDesktopTransportError`'s two forms).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SshError {
    /// The deadline killed the child.
    Timeout,
    /// Any other failure.
    Failed,
}

impl From<SshError> for OpenErrorKind {
    fn from(error: SshError) -> Self {
        match error {
            SshError::Timeout => OpenErrorKind::TransportTimeout,
            SshError::Failed => OpenErrorKind::Transport,
        }
    }
}

/// `checkHost`: the safe usage SSH alias shape.
fn check_host(host: Option<&str>) -> Result<&str, OpenErrorKind> {
    let Some(host) = host else {
        return Err(OpenErrorKind::Other);
    };
    let bytes = host.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 128
        || !bytes[0].is_ascii_alphanumeric()
        || !bytes[1..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(b))
    {
        return Err(OpenErrorKind::Other);
    }
    Ok(host)
}

/// `checkPath`: bounded, control-free, platform-absolute.
fn check_path<'a>(value: Option<&'a str>, platform: &str) -> Result<&'a str, OpenErrorKind> {
    let Some(value) = value else {
        return Err(OpenErrorKind::Other);
    };
    let absolute = if platform == "mac" {
        value.starts_with('/')
    } else {
        // path.win32.isAbsolute: a drive letter plus separator, or a UNC double separator.
        let bytes = value.as_bytes();
        (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/'))
            || value.starts_with("\\\\")
            || value.starts_with("//")
    };
    if value.is_empty()
        || value.len() > 4096
        || value.bytes().any(|b| b < 0x20 || b == 0x7f)
        || !absolute
    {
        return Err(OpenErrorKind::Other);
    }
    Ok(value)
}

/// The transport's `quoteShell`.
fn quote_shell(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// The fixed Mac Open command (`macOpenCommand`), byte for byte.
pub fn mac_open_command(launcher_path: Option<&str>) -> Result<String, OpenErrorKind> {
    let checked = check_path(launcher_path, "mac")?;
    if !checked.ends_with(".app") {
        return Err(OpenErrorKind::Other);
    }
    let app = quote_shell(checked);
    Ok([
        format!("test -d {app} || exit 1"),
        "console_user=$(/usr/bin/stat -f '%Su' /dev/console)".to_string(),
        "test \"$console_user\" != root && test \"$console_user\" != loginwindow || exit 1"
            .to_string(),
        "console_uid=$(/usr/bin/id -u \"$console_user\") || exit 1".to_string(),
        "current_uid=$(/usr/bin/id -u) || exit 1".to_string(),
        "test \"$console_uid\" = \"$current_uid\" || exit 1".to_string(),
        format!("exec /usr/bin/open {app}"),
    ]
    .join("\n"))
}

/// The account-list PowerShell lines (`windowsAccountListScript`), byte for byte.
pub fn windows_account_list_script(ids: &[String]) -> Result<Vec<String>, OpenErrorKind> {
    let unique: std::collections::HashSet<&String> = ids.iter().collect();
    if ids.is_empty()
        || ids.len() > 64
        || unique.len() != ids.len()
        || ids.iter().any(|id| !profile_id_pattern(id))
    {
        return Err(OpenErrorKind::Other);
    }
    let want = ids.iter().map(|id| format!("{id}`n")).collect::<String>();
    Ok(vec![format!(
        "$tmp = $null; try {{ $dir = [IO.Path]::Combine($env:LOCALAPPDATA, 'CCS-Claude'); if (Test-Path -LiteralPath $dir -PathType Container) {{ $list = Join-Path $dir 'ccs-claude-accounts.txt'; $want = \"{want}\"; $have = $null; if (Test-Path -LiteralPath $list -PathType Leaf) {{ if ((Get-Item -LiteralPath $list).Length -le 8192) {{ $have = [IO.File]::ReadAllText($list) -replace \"`r`n\", \"`n\" }} }}; if ($have -cne $want) {{ if ($null -ne $have) {{ Copy-Item -LiteralPath $list -Destination (Join-Path $dir 'ccs-claude-accounts.previous.txt') -Force }}; $tmp = Join-Path $dir 'ccs-claude-accounts.txt.tmp'; [IO.File]::WriteAllText($tmp, $want, (New-Object Text.UTF8Encoding($false))); if (Test-Path -LiteralPath $list -PathType Leaf) {{ [IO.File]::Replace($tmp, $list, [NullString]::Value) }} else {{ [IO.File]::Move($tmp, $list) }} }} }} }} catch {{ try {{ if ($tmp) {{ [IO.File]::Delete($tmp) }} }} catch {{ }} }}"
    )])
}

/// UTF-16LE base64, the PowerShell `-EncodedCommand` form.
fn encode_powershell(script: &str) -> String {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let units: Vec<u8> = script
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    STANDARD.encode(units)
}

/// The fixed Windows Open command (`windowsOpenCommand`), byte for byte.
pub fn windows_open_command(
    profile_id: &str,
    account_list: Option<&[String]>,
) -> Result<String, OpenErrorKind> {
    if !profile_id_pattern(profile_id) {
        return Err(OpenErrorKind::Other);
    }
    let encode = |list_lines: Vec<String>| -> String {
        let script = [
            "$ErrorActionPreference = 'Stop'".to_string(),
        ]
        .into_iter()
        .chain(list_lines)
        .chain([
            format!("$task = Get-ScheduledTask -TaskPath '\\' -TaskName 'ccs-claude-{profile_id}' -ErrorAction Stop"),
            "$currentSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value".to_string(),
            "$taskUser = [string]$task.Principal.UserId".to_string(),
            "$taskSid = if ($taskUser -match '^S-1-') { $taskUser } else { (New-Object Security.Principal.NTAccount($taskUser)).Translate([Security.Principal.SecurityIdentifier]).Value }".to_string(),
            "if ($taskSid -ne $currentSid -or [string]$task.Principal.LogonType -notin @('Interactive', 'InteractiveToken') -or [string]$task.Principal.RunLevel -ne 'Limited') { exit 1 }".to_string(),
            "$helper = [IO.Path]::Combine($env:LOCALAPPDATA, 'CCS-Claude', 'ccs-claude.exe')".to_string(),
            format!("if (@($task.Actions).Count -ne 1 -or $task.Actions[0].Execute -ne $helper -or $task.Actions[0].Arguments -cne 'ccs-claude://launch/{profile_id}') {{ exit 1 }}"),
            "if (@($task.Triggers | Where-Object { $null -ne $_ }).Count -ne 0 -or -not $task.Settings.Enabled -or -not $task.Settings.AllowDemandStart) { exit 1 }".to_string(),
            "Start-ScheduledTask -InputObject $task".to_string(),
        ])
        .collect::<Vec<_>>()
        .join("; ");
        format!(
            "powershell.exe -NoProfile -NonInteractive -EncodedCommand {}",
            encode_powershell(&script)
        )
    };
    let mut command = encode(Vec::new());
    if let Some(list) = account_list
        && list.iter().any(|id| id == profile_id)
    {
        // An invalid list, or one past the command-line limit, is left for later.
        if let Ok(lines) = windows_account_list_script(list) {
            let with_list = encode(lines);
            if with_list.len() <= MAX_WINDOWS_COMMAND {
                command = with_list;
            }
        }
    }
    Ok(command)
}

/// The HTTP reply the internal Open route hands the front door.
pub struct OpenReply {
    /// The status the TS route sends (200, 202, 404, 409, 500, 502, 504).
    pub status: u16,
    /// The exact wire body, field order included.
    pub body: Value,
}

fn refusal(kind: OpenErrorKind) -> OpenReply {
    let (status, body) = match kind {
        OpenErrorKind::NotFound => (
            404,
            json!({"error": "Claude desktop profile was not found."}),
        ),
        OpenErrorKind::HistoryHeld => (
            409,
            json!({"error": CLAUDE_HISTORY_UNCONFIRMED_MESSAGE, "code": "history_unconfirmed"}),
        ),
        OpenErrorKind::NotConfigured => (
            409,
            json!({"error": "Claude desktop launcher is not configured for this platform."}),
        ),
        OpenErrorKind::TransportTimeout => {
            (504, json!({"error": "Claude desktop request timed out."}))
        }
        OpenErrorKind::Transport => (502, json!({"error": "Claude desktop request failed."})),
        OpenErrorKind::Other => (
            500,
            json!({"error": "Claude account could not be opened safely."}),
        ),
    };
    OpenReply { status, body }
}

/// The resolved Open target: the profile and its platform launcher.
struct Target {
    profile: Profile,
    launcher: Launcher,
}

/// The coalescing state: one pending Open per key, plus the recent-success debounce.
struct Coalesce {
    pending: HashMap<String, watch::Sender<Option<Result<(), OpenErrorKind>>>>,
    recent: Vec<(String, u64)>,
}

/// The Open job owner: one instance per service boot, bound by `serve_with_claude_open`.
pub struct ClaudeOpen {
    ccs_dir: PathBuf,
    operations: Operations,
    remote: Arc<dyn Remote>,
    coalesce: Mutex<Coalesce>,
    /// `readDashboardPreferences().snapshotCleanup.auto !== false`, resolved per copy with the
    /// TS default-on-error rule.
    snapshot_cleanup_auto: Box<dyn Fn() -> bool + Send + Sync>,
}

impl ClaudeOpen {
    /// A job owner for one boot root.
    pub fn new(
        ccs_dir: PathBuf,
        remote: Arc<dyn Remote>,
        snapshot_cleanup_auto: Box<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        Self {
            ccs_dir,
            operations: Operations::new(),
            remote,
            coalesce: Mutex::new(Coalesce {
                pending: HashMap::new(),
                recent: Vec::new(),
            }),
            snapshot_cleanup_auto,
        }
    }

    /// The progress store (the GET profiles handler reads it for `openOperation`).
    pub fn operations(&self) -> &Operations {
        &self.operations
    }

    /// The boot root this owner serves.
    pub fn ccs_dir(&self) -> &Path {
        &self.ccs_dir
    }

    /// `resolveOpenTarget`: safe-ID shape, membership (manifest then pending), the platform
    /// launcher and the durable hold. Default-ness plays no part here.
    fn resolve_target(&self, id: &str, platform: &str) -> Result<Target, OpenErrorKind> {
        if !profile_id_pattern(id) {
            return Err(OpenErrorKind::NotFound);
        }
        let manifest = list_profiles(&self.ccs_dir).map_err(|_| OpenErrorKind::Other)?;
        let found = manifest
            .iter()
            .find(|entry| entry.id.as_deref() == Some(id));
        let profile = match found {
            Some(profile) => profile.clone(),
            None => {
                let pending = read_pending_profiles(&self.ccs_dir)
                    .map_err(|_| OpenErrorKind::NotConfigured)?;
                match pending.iter().find(|entry| entry.id == id) {
                    Some(pending) => manifest::pending_open_profile(pending),
                    None => return Err(OpenErrorKind::NotFound),
                }
            }
        };
        let openable = if platform == "mac" {
            can_open_mac(&profile)
        } else {
            can_open_windows(&profile)
        };
        if !openable {
            return Err(OpenErrorKind::NotConfigured);
        }
        let launcher = match platform {
            "mac" => profile.mac.clone(),
            _ => profile.windows.clone(),
        }
        .ok_or(OpenErrorKind::NotConfigured)?;
        if markers::open_held(&self.ccs_dir, id, platform) {
            return Err(OpenErrorKind::HistoryHeld);
        }
        Ok(Target { profile, launcher })
    }

    fn expected<'a>(&self, profile: &'a Profile, platform: &str) -> Option<ExpectedLauncher<'a>> {
        let launcher = match platform {
            "mac" => profile.mac.as_ref()?,
            _ => profile.windows.as_ref()?,
        };
        Some(ExpectedLauncher {
            ssh_host: launcher.ssh_host.as_deref(),
            profile_path: launcher.profile_path.as_deref(),
            launcher_path: launcher.launcher_path.as_deref(),
        })
    }

    /// `claudeOpenUsesManagedHistory`: true only when a verified private policy certainly
    /// applies; the one refusal raised here is the hold, so a click can join a running Open.
    async fn uses_managed_history(&self, id: &str, platform: &str) -> Result<bool, OpenErrorKind> {
        let target = match self.resolve_target(id, platform) {
            Ok(target) => target,
            Err(OpenErrorKind::HistoryHeld) => return Err(OpenErrorKind::HistoryHeld),
            Err(_) => return Ok(false),
        };
        let policy = load_policy(
            &self.ccs_dir,
            id,
            &target.profile.email,
            self.expected(&target.profile, "mac"),
            self.expected(&target.profile, "windows"),
        );
        Ok(policy.is_some())
    }

    /// `openClaudeDesktopProfile`: resolve, coalesce repeated clicks, run the optional copy
    /// inside the same authorized Open, then the launcher.
    async fn open_profile(
        self: &Arc<Self>,
        id: &str,
        platform: &str,
        observer: &dyn OpenObserver,
    ) -> Result<(), OpenErrorKind> {
        let target = self.resolve_target(id, platform)?;
        let key = json!([
            self.ccs_dir,
            id,
            platform,
            target.launcher.ssh_host,
            target.launcher.launcher_path
        ])
        .to_string();
        let (sender, join) = {
            let mut coalesce = self.coalesce.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = coalesce.pending.get(&key) {
                (None, Some(existing.subscribe()))
            } else {
                let now = now_ms();
                if coalesce.recent.iter().any(|(recent_key, at)| {
                    *recent_key == key && now.saturating_sub(*at) < RECENT_OPEN_MS
                }) {
                    return Ok(());
                }
                let (sender, receiver) = watch::channel(None);
                coalesce.pending.insert(key.clone(), sender.clone());
                (Some(sender), Some(receiver))
            }
        };
        let Some(sender) = sender else {
            // Join the pending Open: its outcome is this click's outcome.
            let mut receiver = join.ok_or(OpenErrorKind::Other)?;
            loop {
                if let Some(outcome) = *receiver.borrow_and_update() {
                    return outcome;
                }
                if receiver.changed().await.is_err() {
                    return Err(OpenErrorKind::Other);
                }
            }
        };
        let outcome = self.run_open(&target, id, platform, observer).await;
        {
            let mut coalesce = self.coalesce.lock().unwrap_or_else(|e| e.into_inner());
            coalesce.pending.remove(&key);
            if outcome.is_ok() {
                if coalesce.recent.len() >= 128 {
                    coalesce.recent.clear();
                }
                coalesce.recent.push((key, now_ms()));
            }
        }
        let _ = sender.send(Some(outcome));
        outcome
    }

    /// The opening itself: optional copy, hold recheck, launcher.
    async fn run_open(
        self: &Arc<Self>,
        target: &Target,
        id: &str,
        platform: &str,
        observer: &dyn OpenObserver,
    ) -> Result<(), OpenErrorKind> {
        // The optional neutral index copy runs within this same coalesced authorized Open.
        let history_sync = sync_service::synchronize_before_open(
            &self.ccs_dir,
            &target.profile,
            platform,
            self.remote.as_ref(),
            &ObserverSync(observer),
            (self.snapshot_cleanup_auto)(),
        )
        .await;
        let status = history_sync["status"].as_str().unwrap_or_default();
        let created = history_sync["createdCount"].as_u64().unwrap_or(0);
        observer.synchronized(status, created);
        if markers::open_held(&self.ccs_dir, id, platform)
            || history_sync["reason"] == json!("create_only_transaction_unconfirmed")
        {
            return Err(OpenErrorKind::HistoryHeld);
        }
        observer.opening();
        if platform == "mac" {
            self.open_mac(&target.launcher).await
        } else {
            // Rebuild the launcher's account list from the current profiles in the same call,
            // so a stale list never blocks this Open. Best effort only.
            let account_list = (|| -> Option<Vec<String>> {
                let manifest = list_profiles(&self.ccs_dir).ok()?;
                let pending = read_pending_profiles(&self.ccs_dir).ok()?;
                windows_launcher_account_list(&manifest, &pending)
            })();
            self.open_windows(&target.launcher, id, account_list.as_deref())
                .await
        }
    }

    /// `openClaudeMacLauncher`: the agent opens the wrapper when it proves the step; the fixed
    /// SSH command is the fallback.
    async fn open_mac(&self, launcher: &Launcher) -> Result<(), OpenErrorKind> {
        let launcher_path = check_path(launcher.launcher_path.as_deref(), "mac")?;
        if !launcher_path.ends_with(".app") {
            return Err(OpenErrorKind::Other);
        }
        let ssh_host = check_host(launcher.ssh_host.as_deref())?;
        let args = json!({
            "host": "mac", "op": "open",
            "launcher": {
                "launcherName": launcher.launcher_name,
                "launcherPath": launcher_path,
                "profilePath": launcher.profile_path.as_deref().unwrap_or(launcher_path),
                "sshHost": ssh_host
            }
        });
        if self.remote.host_open("mac", ssh_host, args).await? == AgentOpen::Opened {
            return Ok(());
        }
        let command = mac_open_command(Some(launcher_path))?;
        let output = self
            .remote
            .ssh_exec(
                "mac",
                ssh_host,
                vec!["/bin/sh".into(), "-c".into(), command],
                None,
                SSH_TIMEOUT_MS,
                MAX_HISTORY_BYTES,
            )
            .await?;
        if output.code == Some(0) {
            Ok(())
        } else {
            Err(OpenErrorKind::Transport)
        }
    }

    /// `openClaudeWindowsLauncher`: the agent starts the fixed task when it proves the step;
    /// the fixed SSH command is the fallback.
    async fn open_windows(
        &self,
        launcher: &Launcher,
        profile_id: &str,
        account_list: Option<&[String]>,
    ) -> Result<(), OpenErrorKind> {
        if !profile_id_pattern(profile_id) {
            return Err(OpenErrorKind::Other);
        }
        let ssh_host = check_host(launcher.ssh_host.as_deref())?;
        let mut args = json!({"host": "windows", "op": "open", "profileId": profile_id});
        if let Some(list) = account_list {
            args["accountList"] = json!(list);
        }
        if self.remote.host_open("windows", ssh_host, args).await? == AgentOpen::Opened {
            return Ok(());
        }
        let command = windows_open_command(profile_id, account_list)?;
        let (_, payload) = command.rsplit_once(' ').ok_or(OpenErrorKind::Other)?;
        // The SSH transport wraps argv in PowerShell. Send the generated program on stdin
        // so wrapping it cannot exceed Windows' command-line limit.
        let argv = [
            "powershell.exe",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &encode_powershell(WINDOWS_OPEN_BOOTSTRAP),
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let output = self
            .remote
            .ssh_exec(
                "windows",
                ssh_host,
                argv,
                Some(payload.as_bytes().to_vec()),
                SSH_TIMEOUT_MS,
                MAX_HISTORY_BYTES,
            )
            .await?;
        if output.code == Some(0) {
            Ok(())
        } else {
            Err(OpenErrorKind::Transport)
        }
    }

    /// The route flow: a click while its Open still runs joins it; the copy's own hold must
    /// not refuse it. A held refusal joins a running Open when there is one; a durable hold
    /// keeps its 409.
    pub async fn open(
        self: &Arc<Self>,
        id: &str,
        platform: &str,
        respond_async: bool,
    ) -> OpenReply {
        let scope = self.ccs_dir.to_string_lossy().to_string();
        let running = || self.operations.running(&scope, id, platform);
        let start_or_join = || async {
            let managed = self.uses_managed_history(id, platform).await;
            let managed = match managed {
                Ok(managed) => managed,
                Err(kind) => {
                    // Only the hold of an Open running now is joined; every other refusal stands.
                    if kind == OpenErrorKind::HistoryHeld
                        && let Some(joined) = running()
                    {
                        return Ok(Some(joined));
                    }
                    return Err(kind);
                }
            };
            if let Some(joined) = running() {
                return Ok(Some(joined));
            }
            if !managed {
                // No managed history copy: the ordinary Open, answered when it is done.
                let this = Arc::clone(self);
                this.open_profile(id, platform, &NoOpenObserver).await?;
                return Ok(None);
            }
            // The same refusals as before, answered before any work starts.
            self.resolve_target(id, platform)?;
            // Every managed Open is tracked, so a polling client sees its progress too.
            Ok(running().or_else(|| {
                let this = Arc::clone(self);
                let id = id.to_string();
                let platform = platform.to_string();
                let run_id = id.clone();
                let run_platform = platform.clone();
                Some(self.operations.start(&scope, &id, &platform, move |observer| {
                    let this = Arc::clone(&this);
                    async move { this.open_profile(&run_id, &run_platform, &observer).await }
                }))
            }))
        };
        let operation = match running() {
            Some(joined) => Some(joined),
            None => match start_or_join().await {
                Ok(operation) => operation,
                // Only the hold of an Open running now is joined; a durable hold keeps 409.
                Err(OpenErrorKind::HistoryHeld) => match running() {
                    Some(joined) => Some(joined),
                    None => return refusal(OpenErrorKind::HistoryHeld),
                },
                Err(kind) => return refusal(kind),
            },
        };
        let Some(operation) = operation else {
            return OpenReply {
                status: 200,
                body: json!({"opened": true, "id": id, "platform": platform}),
            };
        };
        if respond_async {
            return OpenReply {
                status: 202,
                body: json!({
                    "id": id, "platform": platform,
                    "state": operation.state, "operationId": operation.id
                }),
            };
        }
        // Wait for this Open's end and answer as before: 200, or the same refusal.
        let outcome = match self.operations.settled(&scope, id, platform) {
            Some(wait) => wait.await,
            None => Ok(()),
        };
        match outcome {
            Ok(()) => OpenReply {
                status: 200,
                body: json!({"opened": true, "id": id, "platform": platform}),
            },
            Err(kind) => refusal(kind),
        }
    }

    /// "Clean up now": retention for every known profile on each of its platforms.
    pub async fn cleanup(&self) -> OpenReply {
        match sync_service::cleanup_all_history_snapshots(&self.ccs_dir, self.remote.as_ref()).await
        {
            Ok(total) => OpenReply {
                status: 200,
                body: total,
            },
            Err(()) => OpenReply {
                status: 500,
                body: json!({"error": "History snapshots could not be cleaned safely."}),
            },
        }
    }

    /// The operation to show for one profile (the GET profiles composition).
    pub fn operation_for(&self, scope: &str, profile_id: &str) -> Option<Operation> {
        self.operations.for_profile(scope, profile_id)
    }
}

/// The GET /desktop-profiles composition: add each Open's progress as `openOperation` to the
/// manifest rows that carry a nonempty string id, exactly as the TS `withOperations` does.
pub fn with_open_operations(mut value: Value, open: &ClaudeOpen, scope: &Path) -> Value {
    let scope = scope.to_string_lossy().to_string();
    if let Some(profiles) = value.get_mut("profiles").and_then(Value::as_array_mut) {
        for profile in profiles {
            let Some(id) = profile
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
            else {
                continue;
            };
            let operation = open
                .operation_for(&scope, id)
                .map(|operation| operation.to_json())
                .unwrap_or(Value::Null);
            if let Some(map) = profile.as_object_mut() {
                map.insert("openOperation".into(), operation);
            }
        }
    }
    value
}

/// `readDashboardPreferences().snapshotCleanup.auto !== false` with the TS default-on-error
/// rule: a missing, unreadable or malformed preferences file keeps automatic retention on.
pub fn snapshot_cleanup_auto(ccs_dir: &Path) -> bool {
    let Ok(bytes) = std::fs::read(ccs_dir.join("dashboard-preferences.json")) else {
        return true;
    };
    let Some(value) = super::js::parse(&bytes) else {
        return true;
    };
    value["snapshotCleanup"]["auto"] != json!(false)
}

/// The wall clock in milliseconds.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
