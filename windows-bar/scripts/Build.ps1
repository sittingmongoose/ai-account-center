param([string]$Dotnet = "$env:LOCALAPPDATA\CCS Bar\build\dotnet\dotnet.exe")
$ErrorActionPreference = 'Stop'
$env:DOTNET_CLI_TELEMETRY_OPTOUT = '1'
$env:DOTNET_NOLOGO = '1'
$source = Split-Path -Parent $PSScriptRoot
$project = Join-Path $source 'CCSBar\CCSBar.csproj'
$publish = Join-Path $source 'publish'
if (-not (Test-Path $Dotnet)) { throw 'A .NET 8 SDK is required. Run scripts/Install-Sdk.ps1 first.' }
& $Dotnet publish $project --configuration Release --runtime win-x64 --self-contained true --output $publish
if ($LASTEXITCODE -ne 0) { throw "dotnet publish failed ($LASTEXITCODE)" }
Write-Output "Published CCS Bar to $publish"
