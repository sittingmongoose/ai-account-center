param([switch]$Live)
$ErrorActionPreference = 'Stop'
$source = Split-Path -Parent $PSScriptRoot
$dll = Join-Path $source 'publish\CCSBar.dll'
$dotnet = Join-Path $env:LOCALAPPDATA 'CCS Bar\build\dotnet\dotnet.exe'
$evidence = Join-Path $source 'evidence'
New-Item -ItemType Directory -Force -Path $evidence | Out-Null
$mode = if ($Live) { '--check-live' } else { '--check' }
$report = Join-Path $evidence $(if ($Live) { 'live-check.json' } else { 'checks.json' })
& $dotnet $dll $mode $report
if ($LASTEXITCODE -ne 0) { throw "CCS Bar verification failed. See $report" }
Get-Content $report
