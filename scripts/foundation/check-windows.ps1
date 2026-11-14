# Uses the aac-winbuild mirror/slot recipe; no provider or live configuration is read.
param(
    [Parameter(Mandatory=$true)][ValidatePattern('^[A-Za-z0-9_-]+$')][string]$Name,
    [switch]$Cleanup
)
$ErrorActionPreference = 'Stop'
$mirror = Join-Path $env:USERPROFILE "aac-build\$Name-foundation-check"
$archive = Join-Path $env:USERPROFILE "aac-foundation-$Name.tgz"
if ($Cleanup) {
    Set-Location $env:USERPROFILE
    Remove-Item -LiteralPath $mirror -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $archive -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $PSCommandPath -Force -ErrorAction SilentlyContinue
    exit 0
}
if (Test-Path -LiteralPath $mirror) { throw 'Own foundation mirror already exists; clean it before retrying.' }
New-Item -ItemType Directory -Path $mirror | Out-Null
tar -xzf $archive -C $mirror
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Set-Location $mirror
$env:Path = "$env:USERPROFILE\.cargo\bin;" + $env:Path
$env:CARGO_BUILD_JOBS = '2'
$env:CARGO_TARGET_DIR = Join-Path $mirror 'target'
$ErrorActionPreference = 'Continue'
cargo +1.99.1 check --locked --target x86_64-pc-windows-msvc -p aac-core -p aac-protocol -p aac-files 2>&1 | ForEach-Object { "$_" }
exit $LASTEXITCODE
