param([string]$Dotnet, [string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$env:DOTNET_CLI_TELEMETRY_OPTOUT = '1'
$env:DOTNET_NOLOGO = '1'
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
            if ($LASTEXITCODE -eq 0 -and @($sdks | Where-Object { $_ -match '^10\.\d+\.\d+' }).Count -gt 0) { return $candidate }
        }
        catch { continue }
    }
    throw 'A .NET 10 SDK is required. Install the Windows x64 SDK from https://dotnet.microsoft.com/en-us/download/dotnet/10.0, reopen PowerShell, or pass -Dotnet with its dotnet.exe path.'
}
$source = Split-Path -Parent $PSScriptRoot
$project = Join-Path $source 'CCSBar\CCSBar.csproj'
$publish = if ($OutputDirectory) { $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputDirectory) } else { Join-Path $source 'publish' }
$dotnetCommand = Resolve-Dotnet $Dotnet
& $dotnetCommand publish $project --configuration Release --runtime win-x64 --self-contained true --output $publish
if ($LASTEXITCODE -ne 0) { throw "dotnet publish failed ($LASTEXITCODE)" }
Write-Output "Published AI Account Center to $publish"
