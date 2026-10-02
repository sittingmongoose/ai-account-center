# Reopen check: a second launch (Start menu, desktop shortcut) shows the running tray's panel instead of starting
# another tray, and the logon task's --background start stays hidden. Runs a published build against an isolated
# state folder (AAC_TRAY_STATE_DIR) holding a loopback fixture connection that nothing answers, so the real tray, its
# connection and its tasks are untouched. Evidence is the build's milestone trace (AAC_TRAY_TRACE: milestone names
# only), because a non-interactive SSH session cannot put a window on screen.
param([Parameter(Mandatory = $true)][string]$PublishDirectory)
$ErrorActionPreference = 'Stop'
$exe = Join-Path (Resolve-Path $PublishDirectory) 'CCSBar.exe'
$state = Join-Path ([IO.Path]::GetTempPath()) ('aac-instance-check-' + [Guid]::NewGuid().ToString('N'))
$trace = Join-Path $state 'trace.txt'
New-Item -ItemType Directory -Force -Path $state | Out-Null
$results = [ordered]@{}
$first = $null
function Lines { if (Test-Path $trace) { @(Get-Content $trace | ForEach-Object { $_.Substring(13) }) } else { @() } }
try {
    $env:AAC_TRAY_STATE_DIR = $state
    $configure = New-Object System.Diagnostics.ProcessStartInfo $exe, '--configure-stdin'
    $configure.UseShellExecute = $false; $configure.RedirectStandardInput = $true; $configure.RedirectStandardOutput = $true
    $process = [System.Diagnostics.Process]::Start($configure)
    $process.StandardInput.Write('{"baseURL":"http://127.0.0.1:9","username":"fixture","password":"fixture-only"}'); $process.StandardInput.Close()
    [void]$process.WaitForExit(20000)
    $results['fixture_connection_in_isolated_folder'] = Test-Path (Join-Path $state 'connection.dpapi')
    $env:AAC_TRAY_TRACE = $trace
    $first = Start-Process -FilePath $exe -ArgumentList '--background' -PassThru
    Start-Sleep -Seconds 5
    $before = Lines
    $results['background_start_runs_and_listens'] = -not $first.HasExited -and ($before -contains 'listening')
    $results['background_start_stays_hidden'] = -not ($before | Where-Object { $_ -like 'panel shown*' })
    $second = Start-Process -FilePath $exe -PassThru
    $exited = $second.WaitForExit(15000)
    Start-Sleep -Seconds 2
    $after = Lines
    $results['second_launch_hands_over_and_exits'] = $exited -and $second.ExitCode -eq 0 -and (@($after | Where-Object { $_ -eq 'instance owner' }).Count -eq 1)
    $request = [Array]::IndexOf($after, 'show request')
    $results['second_launch_shows_running_panel'] = $request -ge 0 -and ($after[($request + 1)..($after.Count - 1)] -contains 'panel shown, visible=True') -and -not $first.HasExited
    $results['one_tray_process'] = @(Get-Process CCSBar -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe }).Count -eq 1
    $results['hotkey_state_traced'] = [bool]($before | Where-Object { $_ -like 'hotkey *' })
}
finally {
    if ($first -and -not $first.HasExited) { Stop-Process -Id $first.Id -Force }
    Remove-Item Env:\AAC_TRAY_STATE_DIR, Env:\AAC_TRAY_TRACE -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
    Remove-Item -Recurse -Force $state -ErrorAction SilentlyContinue
}
$passed = -not ($results.Values -contains $false)
[PSCustomObject]@{ passed = $passed; checks = $results; trace = $after } | ConvertTo-Json -Depth 3
if (-not $passed) { exit 1 }
