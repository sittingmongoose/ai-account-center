$ErrorActionPreference = 'Stop'
$build = Join-Path $env:LOCALAPPDATA 'CCS Bar\build'
New-Item -ItemType Directory -Force -Path $build | Out-Null
$installer = Join-Path $build 'dotnet-install.ps1'
Invoke-WebRequest -Uri 'https://dot.net/v1/dotnet-install.ps1' -OutFile $installer
& $installer -Version '10.0.401' -InstallDir (Join-Path $build 'dotnet') -NoPath
# The installer is a script, so it sets $? rather than $LASTEXITCODE on success.
if (-not $?) { throw 'SDK install failed' }
