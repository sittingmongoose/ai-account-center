$ErrorActionPreference = 'Stop'
$build = Join-Path $env:LOCALAPPDATA 'CCS Bar\build'
New-Item -ItemType Directory -Force -Path $build | Out-Null
$installer = Join-Path $build 'dotnet-install.ps1'
Invoke-WebRequest -Uri 'https://dot.net/v1/dotnet-install.ps1' -OutFile $installer
& $installer -Version '8.0.425' -InstallDir (Join-Path $build 'dotnet') -NoPath
if ($LASTEXITCODE -ne 0) { throw "SDK install failed ($LASTEXITCODE)" }
