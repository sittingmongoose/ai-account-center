//! The Windows host steps: a byte-for-byte port of `claude-host-transport.ts`'s
//! `windowsHostScript` (the fixed PowerShell the reference sends over ssh) plus the Open
//! command from `claude-desktop-transport.ts`. The agent runs the same fixed script locally
//! behind the same encoded bootstrap, so the COM shortcuts, scheduled tasks and allowlist
//! keep the reference's exact behavior; `tests/spec/agent/claude-host-table.json` pins the
//! generated text against the real TS builder for every operation and value shape.
use super::{HostRequest, Launcher, Op};
use aac_core::{Cause, Failure};
use base64::Engine;
use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// The reference's `WINDOWS_SCRIPT_MAX_BYTES`: the measured safe stdin range.
pub(crate) const WINDOWS_SCRIPT_MAX_BYTES: usize = 16 * 1024;
/// `MAX_WINDOWS_COMMAND`: safely under the 8191-character command-line limit.
const MAX_WINDOWS_COMMAND: usize = 7600;
/// The reference's `maxBuffer` for a host step's reply.
const MAX_REPLY_BYTES: usize = 64 * 1024;

pub(crate) const BOOTSTRAP_TEXT: &str =
    "$s = [Console]::In.ReadToEnd(); & ([scriptblock]::Create($s)); exit $LASTEXITCODE";

fn validation() -> Failure {
    Failure::new(Cause::Refused, "Claude host value is not safe.")
}

/// utf16le + base64, as `Buffer.from(script, 'utf16le').toString('base64')`.
pub(crate) fn encoded_command(script: &str) -> String {
    let mut bytes = Vec::with_capacity(script.len() * 2);
    for unit in script.encode_utf16() {
        bytes.push(unit as u8);
        bytes.push((unit >> 8) as u8);
    }
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The fixed `WINDOWS_COMMON` prelude, verbatim.
const WINDOWS_COMMON: [&str; 16] = [
    "$ErrorActionPreference = 'Stop'",
    "$ProgressPreference = 'SilentlyContinue'",
    "$support = [IO.Path]::GetFullPath($env:APPDATA)",
    "$trashRoot = [IO.Path]::Combine($env:USERPROFILE, '.ccs', 'trash', 'claude')",
    "$helper = [IO.Path]::Combine($env:LOCALAPPDATA, 'CCS-Claude', 'ccs-claude.exe')",
    "$accountsFile = [IO.Path]::Combine($env:LOCALAPPDATA, 'CCS-Claude', 'ccs-claude-accounts.txt')",
    "function Fail([int]$code) { exit $code }",
    "function Add-AccountId([string]$id) { if (-not (Test-Path -LiteralPath $accountsFile -PathType Leaf)) { return }; foreach ($line in @(Get-Content -LiteralPath $accountsFile)) { if ($line.Trim() -ceq $id) { return } }; $text = [IO.File]::ReadAllText($accountsFile); if (($text.Length -gt 0) -and -not $text.EndsWith([string][char]10)) { Add-Content -LiteralPath $accountsFile -Value '' -Encoding UTF8 }; Add-Content -LiteralPath $accountsFile -Value $id -Encoding UTF8 }",
    "function Drop-AccountId([string]$id) { if (-not (Test-Path -LiteralPath $accountsFile -PathType Leaf)) { return }; $kept = @(Get-Content -LiteralPath $accountsFile | Where-Object { $_.Trim() -cne $id }); if ($kept.Count -eq 0) { Remove-Item -LiteralPath $accountsFile -Force } else { Set-Content -LiteralPath $accountsFile -Value $kept -Encoding UTF8 } }",
    "function Check-Profile([string]$path) { $full = [IO.Path]::GetFullPath($path); if ([IO.Path]::GetDirectoryName($full) -ne $support -or -not [IO.Path]::GetFileName($full).StartsWith('Claude')) { Fail 1 }; return $full }",
    "function Check-Link([string]$path) { $full = [IO.Path]::GetFullPath($path); $dir = [IO.Path]::GetDirectoryName($full); $name = [IO.Path]::GetFileName($full); if (($dir -ne [Environment]::GetFolderPath('Desktop') -and $dir -ne [Environment]::GetFolderPath('Programs')) -or -not $name.StartsWith('Claude (') -or -not $name.EndsWith(').lnk')) { Fail 1 }; return $full }",
    "function Claude-Exe { $package = Get-AppxPackage -Name 'Claude' | Sort-Object Version -Descending | Select-Object -First 1; if (-not $package) { Fail 3 }; $exe = Join-Path $package.InstallLocation 'app\\Claude.exe'; if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) { Fail 3 }; return $exe }",
    "function Make-Links([string[]]$links, [string]$profile) { $exe = Claude-Exe; $shell = New-Object -ComObject WScript.Shell; foreach ($link in $links) { $s = $shell.CreateShortcut($link); $s.TargetPath = $exe; $s.Arguments = '--user-data-dir=\"' + $profile + '\"'; $s.WorkingDirectory = Split-Path -Parent $exe; $s.IconLocation = $exe + ',0'; $s.Save() } }",
    "function Make-Task([string]$id) { $action = New-ScheduledTaskAction -Execute $helper -Argument ('ccs-claude://launch/' + $id); $principal = New-ScheduledTaskPrincipal -UserId ($env:USERDOMAIN + '\\' + $env:USERNAME) -LogonType Interactive -RunLevel Limited; Register-ScheduledTask -TaskPath '\\' -TaskName ('ccs-claude-' + $id) -Action $action -Principal $principal -Settings (New-ScheduledTaskSettingsSet) | Out-Null }",
    "function Drop-Task([string]$id) { Get-ScheduledTask -TaskPath '\\' -TaskName ('ccs-claude-' + $id) -ErrorAction SilentlyContinue | Unregister-ScheduledTask -Confirm:$false }",
    "function Done($value) { [Console]::Write((ConvertTo-Json -Compress $value)); exit 0 }",
];

/// `psValue`: the SAFE_PATH allowlist, then single quotes.
fn ps_value(value: &str) -> Result<String, Failure> {
    if !super::safe_path(value) {
        return Err(validation());
    }
    Ok(format!("'{value}'"))
}

/// `windowsHostScript`, byte for byte. The per-operation lines are the reference's.
pub(crate) fn windows_host_script(
    op: Op,
    profile_id: Option<&str>,
    trash_name: Option<&str>,
    launcher: Option<&Launcher>,
) -> Result<String, Failure> {
    let mut lines: Vec<String> = WINDOWS_COMMON
        .iter()
        .map(|line| (*line).to_string())
        .collect();
    if let Some(id) = profile_id {
        if !super::profile_id_valid(id) {
            return Err(Failure::new(Cause::Refused, "Invalid profile id."));
        }
        lines.push(format!("$id = '{id}'"));
    }
    if let Some(name) = trash_name {
        if !super::trash_name_valid(name) {
            return Err(Failure::new(Cause::Refused, "Invalid trash name."));
        }
        lines.push(format!("$target = Join-Path $trashRoot '{name}'"));
    }
    if let Some(launcher) = launcher {
        lines.push(format!(
            "$profile = Check-Profile {}",
            ps_value(&launcher.profile_path)?
        ));
        let mut links = Vec::new();
        if let Some(launcher_path) = &launcher.launcher_path {
            links.push(ps_value(launcher_path)?);
        }
        if let Some(start_menu) = &launcher.start_menu_path {
            links.push(ps_value(start_menu)?);
        }
        let checks: Vec<String> = links
            .iter()
            .map(|link| format!("(Check-Link {link})"))
            .collect();
        lines.push(format!("$links = @({})", checks.join(", ")));
    }
    match op {
        Op::Create => lines.extend([
            "$profile = Join-Path $support ('Claude-' + $id)".to_string(),
            "$links = @((Join-Path ([Environment]::GetFolderPath('Desktop')) ('Claude (' + $id + ').lnk')), (Join-Path ([Environment]::GetFolderPath('Programs')) ('Claude (' + $id + ').lnk')))".to_string(),
            "if (Test-Path -LiteralPath $profile) { Fail 3 }; foreach ($link in $links) { if (Test-Path -LiteralPath $link) { Fail 3 } }".to_string(),
            "if (Get-ScheduledTask -TaskPath '\\' -TaskName ('ccs-claude-' + $id) -ErrorAction SilentlyContinue) { Fail 3 }".to_string(),
            "New-Item -ItemType Directory -Path $profile | Out-Null".to_string(),
            "try { Make-Links $links $profile; Make-Task $id; Add-AccountId $id } catch { foreach ($link in $links) { Remove-Item -LiteralPath $link -Force -ErrorAction SilentlyContinue }; Drop-Task $id; Drop-AccountId $id; Remove-Item -LiteralPath $profile -Force -ErrorAction SilentlyContinue; Fail 1 }".to_string(),
            "Done ([ordered]@{ launcherName = 'Claude (' + $id + ')'; launcherPath = $links[0]; startMenuPath = $links[1]; profilePath = $profile })".to_string(),
        ]),
        Op::Undo => lines.extend([
            "foreach ($link in $links) { Remove-Item -LiteralPath $link -Force -ErrorAction SilentlyContinue }".to_string(),
            "Drop-Task $id".to_string(),
            "Drop-AccountId $id".to_string(),
            "if ((Test-Path -LiteralPath $profile) -and @(Get-ChildItem -LiteralPath $profile -Force).Count -eq 0) { Remove-Item -LiteralPath $profile -Force }".to_string(),
            "Done @{ ok = $true }".to_string(),
        ]),
        Op::State => lines.extend([
            "$needle = '--user-data-dir=\"' + $profile + '\"'".to_string(),
            "$found = @(Get-CimInstance Win32_Process -Filter \"Name = 'claude.exe'\" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($needle) })".to_string(),
            "Done @{ running = ($found.Count -gt 0) }".to_string(),
        ]),
        Op::Session => lines.extend([
            "$configPath = Join-Path $profile 'config.json'".to_string(),
            "if (-not (Test-Path -LiteralPath $configPath -PathType Leaf)) { Done @{ signedIn = $false } }".to_string(),
            "if ((Get-Item -LiteralPath $configPath -Force).Length -gt 1048576) { Fail 1 }".to_string(),
            "$config = $null; try { $config = Get-Content -LiteralPath $configPath -Raw -Encoding UTF8 | ConvertFrom-Json } catch { Done @{ signedIn = $false } }".to_string(),
            "$uuid = $config.lastKnownAccountUuid; $cache = $config.'oauth:tokenCacheV2'".to_string(),
            "$uuidOk = ($uuid -is [string]) -and ($uuid -match '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$')".to_string(),
            "Done @{ signedIn = [bool]($uuidOk -and ($cache -is [string]) -and ($cache.Length -gt 0)) }".to_string(),
        ]),
        Op::Trash => lines.extend([
            "New-Item -ItemType Directory -Path $trashRoot -Force | Out-Null".to_string(),
            "if (Test-Path -LiteralPath $target) { Fail 1 }".to_string(),
            "$moved = $false; if (Test-Path -LiteralPath $profile) { if ([IO.Path]::GetPathRoot($profile) -ne [IO.Path]::GetPathRoot([IO.Path]::GetFullPath($trashRoot))) { Done @{ result = 'cross_volume' } }; [IO.Directory]::Move($profile, $target); $moved = $true } else { New-Item -ItemType Directory -Path $target | Out-Null }".to_string(),
            "try { foreach ($link in $links) { Remove-Item -LiteralPath $link -Force -ErrorAction SilentlyContinue }; Drop-Task $id } catch { if ($moved) { [IO.Directory]::Move($target, $profile) }; Fail 1 }".to_string(),
            "Done @{ result = 'moved' }".to_string(),
        ]),
        Op::Restore => lines.extend([
            "if (-not (Test-Path -LiteralPath $target -PathType Container) -or (Test-Path -LiteralPath $profile)) { Fail 3 }".to_string(),
            "[IO.Directory]::Move($target, $profile)".to_string(),
            "Make-Links $links $profile; Drop-Task $id; Make-Task $id".to_string(),
            "Done @{ ok = $true }".to_string(),
        ]),
        Op::Purge => lines.extend([
            "if (Test-Path -LiteralPath $target) { if ((Get-Item -LiteralPath $target -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { Fail 1 }; Remove-Item -LiteralPath $target -Recurse -Force }".to_string(),
            "Done @{ ok = $true }".to_string(),
        ]),
        Op::Open => return Err(validation()),
    }
    let script = lines.join("\n");
    if script.len() > WINDOWS_SCRIPT_MAX_BYTES {
        return Err(Failure::new(
            Cause::Refused,
            "Claude host script is too large.",
        ));
    }
    Ok(script)
}

/// `windowsAccountListScript`: the Open's list rewrite, one fixed line. Ids use the desktop
/// profile pattern (`CLAUDE_PROFILE_ID_PATTERN`), 1..64 unique entries.
pub(crate) fn windows_account_list_script(ids: &[String]) -> Result<Vec<String>, Failure> {
    if ids.is_empty()
        || ids.len() > 64
        || ids.iter().enumerate().any(|(i, id)| ids[..i].contains(id))
        || ids.iter().any(|id| !super::desktop_profile_id_valid(id))
    {
        return Err(Failure::new(
            Cause::Refused,
            "Claude Windows account list is not valid.",
        ));
    }
    let want: String = ids.iter().map(|id| format!("{id}`n")).collect();
    Ok(vec![format!(
        "$tmp = $null; try {{ $dir = [IO.Path]::Combine($env:LOCALAPPDATA, 'CCS-Claude'); if (Test-Path -LiteralPath $dir -PathType Container) {{ $list = Join-Path $dir 'ccs-claude-accounts.txt'; $want = \"{want}\"; $have = $null; if (Test-Path -LiteralPath $list -PathType Leaf) {{ if ((Get-Item -LiteralPath $list).Length -le 8192) {{ $have = [IO.File]::ReadAllText($list) -replace \"`r`n\", \"`n\" }} }}; if ($have -cne $want) {{ if ($null -ne $have) {{ Copy-Item -LiteralPath $list -Destination (Join-Path $dir 'ccs-claude-accounts.previous.txt') -Force }}; $tmp = Join-Path $dir 'ccs-claude-accounts.txt.tmp'; [IO.File]::WriteAllText($tmp, $want, (New-Object Text.UTF8Encoding($false))); if (Test-Path -LiteralPath $list -PathType Leaf) {{ [IO.File]::Replace($tmp, $list, [NullString]::Value) }} else {{ [IO.File]::Move($tmp, $list) }} }} }} }} catch {{ try {{ if ($tmp) {{ [IO.File]::Delete($tmp) }} }} catch {{ }} }}"
    )])
}

/// `openClaudeWindowsLauncher`'s encoded command: the fixed task checks and start, with the
/// account-list step when the list carries this profile and fits the command-line limit.
pub(crate) fn windows_open_command(
    profile_id: &str,
    account_list: Option<&[String]>,
) -> Result<String, Failure> {
    if !super::desktop_profile_id_valid(profile_id) {
        return Err(Failure::new(
            Cause::Refused,
            "Select a configured Claude Windows account.",
        ));
    }
    let encode = |list_lines: &[String]| -> String {
        let mut parts = vec!["$ErrorActionPreference = 'Stop'".to_string()];
        parts.extend(list_lines.iter().cloned());
        parts.extend([
            format!("$task = Get-ScheduledTask -TaskPath '\\' -TaskName 'ccs-claude-{profile_id}' -ErrorAction Stop"),
            "$currentSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value".to_string(),
            "$taskUser = [string]$task.Principal.UserId".to_string(),
            "$taskSid = if ($taskUser -match '^S-1-') { $taskUser } else { (New-Object Security.Principal.NTAccount($taskUser)).Translate([Security.Principal.SecurityIdentifier]).Value }".to_string(),
            "if ($taskSid -ne $currentSid -or [string]$task.Principal.LogonType -notin @('Interactive', 'InteractiveToken') -or [string]$task.Principal.RunLevel -ne 'Limited') { exit 1 }".to_string(),
            "$helper = [IO.Path]::Combine($env:LOCALAPPDATA, 'CCS-Claude', 'ccs-claude.exe')".to_string(),
            format!("if (@($task.Actions).Count -ne 1 -or $task.Actions[0].Execute -ne $helper -or $task.Actions[0].Arguments -cne 'ccs-claude://launch/{profile_id}') {{ exit 1 }}"),
            "if (@($task.Triggers | Where-Object { $null -ne $_ }).Count -ne 0 -or -not $task.Settings.Enabled -or -not $task.Settings.AllowDemandStart) { exit 1 }".to_string(),
            "Start-ScheduledTask -InputObject $task".to_string(),
        ]);
        let script = parts.join("; ");
        format!(
            "powershell.exe -NoProfile -NonInteractive -EncodedCommand {}",
            encoded_command(&script)
        )
    };
    let mut command = encode(&[]);
    if let Some(list) = account_list
        && list.iter().any(|id| id == profile_id)
        && let Ok(list_lines) = windows_account_list_script(list)
    {
        let with_list = encode(&list_lines);
        if with_list.len() <= MAX_WINDOWS_COMMAND {
            command = with_list;
        }
    }
    Ok(command)
}

/// Run one fixed script locally behind the reference bootstrap and read its small JSON
/// reply. A fixture home redirects the environment roots the script reads; the shell-folder
/// and task steps are refused in fixture mode before reaching this function.
/// `GetFolderPath` expands `%USERPROFILE%` from the registry, so Check-Link needs the real
/// one: session and state keep it (they never touch the trash root); purge redirects it.
fn run_script(
    script: &str,
    fixture_home: Option<&Path>,
    redirect_userprofile: bool,
) -> Result<Value, Failure> {
    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &encoded_command(BOOTSTRAP_TEXT),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(home) = fixture_home {
        command
            .env("APPDATA", home.join("AppData").join("Roaming"))
            .env("LOCALAPPDATA", home.join("AppData").join("Local"));
        if redirect_userprofile {
            command.env("USERPROFILE", home);
        }
    }
    let mut child = command
        .spawn()
        .map_err(|_| Failure::new(Cause::Refused, "A Claude host step failed."))?;
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(script.as_bytes());
        let _ = stdin.flush();
    }
    drop(child.stdin.take());
    let output = child
        .wait_with_output()
        .map_err(|_| Failure::new(Cause::Refused, "A Claude host step failed."))?;
    if output.stdout.len() > MAX_REPLY_BYTES {
        return Err(Failure::new(
            Cause::OutputTooLarge,
            "Claude host reply is too large.",
        ));
    }
    if !output.status.success() {
        return Err(match output.status.code() {
            Some(3) => Failure::new(Cause::InUse, "The Claude profile already exists."),
            _ => Failure::new(Cause::Refused, "A Claude host step failed."),
        });
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|_| Failure::new(Cause::Refused, "A Claude host step failed."))?;
    let value: Value = serde_json::from_str(text.trim())
        .map_err(|_| Failure::new(Cause::Refused, "A Claude host step failed."))?;
    if !value.is_object() {
        return Err(Failure::new(Cause::Refused, "A Claude host step failed."));
    }
    Ok(value)
}

pub(crate) fn run(req: &HostRequest, fixture_home: Option<&Path>) -> Result<Value, Failure> {
    let launcher = req.launcher.as_ref();
    match req.op {
        Op::Open => {
            let profile_id = req.profile_id.as_deref().ok_or_else(validation)?;
            let command = windows_open_command(profile_id, req.account_list.as_deref())?;
            // The command is the reference's full command line; run its fixed argv locally.
            let script_b64 = command.rsplit(' ').next().ok_or_else(validation)?;
            let mut child = Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-EncodedCommand",
                    script_b64,
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|_| Failure::new(Cause::Refused, "A Claude host step failed."))?;
            let status = child
                .wait()
                .map_err(|_| Failure::new(Cause::Refused, "A Claude host step failed."))?;
            if !status.success() {
                return Err(Failure::new(Cause::Refused, "A Claude host step failed."));
            }
            Ok(serde_json::json!({"ok": true}))
        }
        op => {
            let script = windows_host_script(
                op,
                req.profile_id.as_deref(),
                req.trash_name.as_deref(),
                launcher,
            )?;
            run_script(&script, fixture_home, matches!(op, Op::Purge))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_matches_the_reference() {
        // Captured from the TS constant: Buffer.from(BOOTSTRAP_TEXT, 'utf16le').toString('base64').
        let reference = "JABzACAAPQAgAFsAQwBvAG4AcwBvAGwAZQBdADoAOgBJAG4ALgBSAGUAYQBkAFQAbwBFAG4AZAAoACkAOwAgACYAIAAoAFsAcwBjAHIAaQBwAHQAYgBsAG8AYwBrAF0AOgA6AEMAcgBlAGEAdABlACgAJABzACkAKQA7ACAAZQB4AGkAdAAgACQATABBAFMAVABFAFgASQBUAEMATwBEAEUA";
        assert_eq!(encoded_command(BOOTSTRAP_TEXT), reference);
    }

    #[test]
    fn account_list_validation_matches_the_reference() {
        let ok = vec!["store".to_string(), "work2".to_string()];
        assert!(windows_account_list_script(&ok).is_ok());
        assert!(windows_account_list_script(&[]).is_err());
        assert!(windows_account_list_script(&["a".to_string(), "a".to_string()]).is_err());
        assert!(windows_account_list_script(&["bad id".to_string()]).is_err());
        assert!(windows_account_list_script(&["-lead".to_string()]).is_err());
        let sixty_five: Vec<String> = (0..65).map(|i| format!("p{i}")).collect();
        assert!(windows_account_list_script(&sixty_five).is_err());
    }
}
