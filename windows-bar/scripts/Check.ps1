param([switch]$Live, [string]$Dotnet, [string]$PublishDirectory, [string]$ReportDirectory)
$ErrorActionPreference = 'Stop'
function Resolve-Dotnet([string]$Requested) {
    $candidates = @()
    if ($Requested) {
        if (Test-Path -LiteralPath $Requested -PathType Leaf) { $candidates += (Get-Item -LiteralPath $Requested).FullName }
        else { $command = Get-Command $Requested -CommandType Application -ErrorAction SilentlyContinue; if ($command) { $candidates += $command.Source } }
    }
    else {
        $command = Get-Command dotnet -CommandType Application -ErrorAction SilentlyContinue
        if ($command) { $candidates += $command.Source }
        $candidates += (Join-Path $env:LOCALAPPDATA 'CCS Bar\build\dotnet\dotnet.exe')
    }
    foreach ($candidate in ($candidates | Select-Object -Unique)) {
        if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) { continue }
        try {
            $sdks = @(& $candidate --list-sdks 2>$null)
            if ($LASTEXITCODE -eq 0 -and @($sdks | Where-Object { $_ -match '^8\.\d+\.\d+' }).Count -gt 0) { return $candidate }
        }
        catch { continue }
    }
    throw 'A .NET 8 SDK is required. Install the Windows x64 SDK from https://dotnet.microsoft.com/en-us/download/dotnet/8.0, reopen PowerShell, or pass -Dotnet with its dotnet.exe path.'
}
$source = Split-Path -Parent $PSScriptRoot
$publish = if ($PublishDirectory) { $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($PublishDirectory) } else { Join-Path $source 'publish' }
$dll = Join-Path $publish 'CCSBar.dll'
if (-not (Test-Path -LiteralPath $dll)) { throw 'Build AI Account Center first, or pass -PublishDirectory with its published files.' }
$dotnetCommand = Resolve-Dotnet $Dotnet
$evidence = if ($ReportDirectory) { $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($ReportDirectory) } else { Join-Path $source 'evidence' }
New-Item -ItemType Directory -Force -Path $evidence | Out-Null
$mode = if ($Live) { '--check-live' } else { '--check' }
$report = Join-Path $evidence $(if ($Live) { 'live-check.json' } else { 'checks.json' })
# Offline checks and renders run against an isolated state folder (AAC_TRAY_STATE_DIR), so nothing they do can reach
# the tray's real connection or preferences. The sign-in checks also keep their fixture store in a folder of their own.
$isolatedState = $null
if (-not $Live) {
    $isolatedState = Join-Path ([IO.Path]::GetTempPath()) ('aac-check-state-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $isolatedState | Out-Null
    $env:AAC_TRAY_STATE_DIR = $isolatedState
}
try {
    & $dotnetCommand $dll $mode $report
    if ($LASTEXITCODE -ne 0) { throw "AI Account Center verification failed. See $report" }
    Get-Content -LiteralPath $report
    if (-not $Live) {
        # Offline render checks from the bundled sanitized fixture (one theme per process) and the installer checks.
        foreach ($theme in @('light', 'dark')) {
            & $dotnetCommand $dll --render-fixture (Join-Path $evidence 'render') $theme
            if ($LASTEXITCODE -ne 0) { throw "AI Account Center $theme render checks failed. See $(Join-Path $evidence 'render')" }
        }
    }
}
finally {
    if ($isolatedState) { Remove-Item Env:\AAC_TRAY_STATE_DIR -ErrorAction SilentlyContinue; Remove-Item -Recurse -Force $isolatedState -ErrorAction SilentlyContinue }
}
if (-not $Live) {
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'Test-InstallScripts.ps1') | Set-Content -LiteralPath (Join-Path $evidence 'install-script-checks.json')
    if ($LASTEXITCODE -ne 0) { throw "Installer script checks failed. See $(Join-Path $evidence 'install-script-checks.json')" }
}
