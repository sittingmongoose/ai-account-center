# Lane E (M3D4 staged, M3D5 pinned): place the hash-pinned aac-agent in AAC's cache folder on
# this Windows computer (JaredGamingPC, Alexis). The lead runs this on each live computer; the
# Rust service admits only an already installed, pinned agent and never uploads one itself.
# This script never builds (build on aac-winbuild with: node scripts/build-agent.js) and never
# edits config or trust files.
#
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\agent-install.ps1 -Binary <path> -Sha256 <hash>
#   -Binary   the packaged aac-agent.exe for this computer (dist\agent\x86_64-pc-windows-msvc\)
#   -Sha256   its pinned digest from src\web-server\services\agent-artifacts.json
#
# Verifies the source digest, stages a .part copy, verifies it again, renames atomically, then
# requires the --version handshake {name, version, protocol, target} for Windows.
param(
  [Parameter(Mandatory = $true)][string]$Binary,
  [Parameter(Mandatory = $true)][string]$Sha256
)
$ErrorActionPreference = 'Stop'

if ($Sha256 -notmatch '^[a-f0-9]{64}$') { throw 'sha256 must be 64 lowercase hex characters' }
if (!(Test-Path -LiteralPath $Binary -PathType Leaf)) { throw "no such binary: $Binary" }
$h = (Get-FileHash -LiteralPath $Binary -Algorithm SHA256).Hash.ToLowerInvariant()
if ($h -ne $Sha256) { throw "binary digest $h does not match the pinned $Sha256" }

$dir = Join-Path $env:LOCALAPPDATA 'ai-account-center\bin'
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$dest = Join-Path $dir 'aac-agent.exe'
$part = Join-Path $dir ("aac-agent.part." + $PID + ".exe")

Copy-Item -LiteralPath $Binary -Destination $part -Force
try {
  $h2 = (Get-FileHash -LiteralPath $part -Algorithm SHA256).Hash.ToLowerInvariant()
  if ($h2 -ne $Sha256) { throw 'staged copy digest mismatch' }
  Move-Item -LiteralPath $part -Destination $dest -Force
} finally {
  if (Test-Path -LiteralPath $part) { Remove-Item -LiteralPath $part -Force }
}

$identity = & $dest --version | Out-String
$j = $identity | ConvertFrom-Json
if ($j.name -ne 'aac-agent' -or $j.protocol -ne 1 -or $j.target -ne 'x86_64-pc-windows-msvc') {
  throw "unexpected agent identity for this computer: $identity"
}
Write-Output ("[OK] aac-agent " + $j.version + " (" + $j.target + ") installed and pinned")
