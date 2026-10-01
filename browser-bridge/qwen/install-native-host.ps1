[CmdletBinding()]
param([switch]$Install, [switch]$ValidateOnly)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
if ($Install -and $ValidateOnly) { throw 'Choose either Install or ValidateOnly.' }
$HostName = 'com.ccs.qwen_usage_bridge'
$ExpectedId = 'clobbdmblhillanldmmjnlpbaafbnklj'
$SourceRoot = $PSScriptRoot
$SourceExtension = Join-Path $SourceRoot 'extension'
$SourceHost = Join-Path $SourceRoot 'native-host\publish'
$SourceHelpers = Join-Path $SourceRoot 'helpers'
$HelperNames = @('plan_common.py', 'plan_usage.py')
$BinaryName = 'CCS.QwenUsageBridge.exe'
$Executable = Join-Path $SourceHost $BinaryName
$Manifest = Get-Content -LiteralPath (Join-Path $SourceExtension 'manifest.json') -Raw | ConvertFrom-Json
$PublicBytes = [Convert]::FromBase64String($Manifest.key)
$Hash = [Security.Cryptography.SHA256]::Create().ComputeHash($PublicBytes)
$Id = -join ($Hash[0..15] | ForEach-Object { [char](97 + ($_ -shr 4)); [char](97 + ($_ -band 15)) })
if ($Id -ne $ExpectedId) { throw 'The extension identity does not match this native host.' }
if (-not (Test-Path -LiteralPath $Executable -PathType Leaf)) { throw 'Build or copy the published Windows native helper first.' }
foreach ($Name in @('CCS.QwenUsageBridge.dll', 'CCS.QwenUsageBridge.deps.json', 'CCS.QwenUsageBridge.runtimeconfig.json')) {
  if (-not (Test-Path -LiteralPath (Join-Path $SourceHost $Name) -PathType Leaf)) { throw 'The native helper package is incomplete.' }
}
foreach ($Name in $HelperNames) {
  if (-not (Test-Path -LiteralPath (Join-Path $SourceHelpers $Name) -PathType Leaf)) { throw 'The v2-compatible Qwen usage collectors are missing from this package.' }
}
$Target = Join-Path $env:LOCALAPPDATA 'CCS\QwenUsageBridge'
$TargetHost = Join-Path $Target 'host'
$TargetExtension = Join-Path $Target 'extension'
$TargetHelpers = Join-Path $env:USERPROFILE '.ccs\account-usage'
$ManifestPath = Join-Path $Target 'com.ccs.qwen_usage_bridge.json'
$RegistryPaths = @(
  'HKCU:\Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\com.ccs.qwen_usage_bridge',
  'HKCU:\Software\Google\Chrome\NativeMessagingHosts\com.ccs.qwen_usage_bridge'
)
if (-not $Install) {
  [pscustomobject]@{ validated = $true; extensionId = $ExpectedId; version = $Manifest.version; installDirectory = $Target; extensionDirectory = $TargetExtension; collectorDirectory = $TargetHelpers; registryKeys = $RegistryPaths; browserPoliciesChanged = $false; extensionInstalled = $false } | ConvertTo-Json -Depth 4
  exit 0
}
New-Item -ItemType Directory -Path $TargetHost, $TargetExtension, $TargetHelpers -Force | Out-Null
$HelperNames | ForEach-Object { Copy-Item -LiteralPath (Join-Path $SourceHelpers $_) -Destination $TargetHelpers -Force }
Get-ChildItem -LiteralPath $SourceHost -File | ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination $TargetHost -Force }
Get-ChildItem -LiteralPath $SourceExtension -File | ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination $TargetExtension -Force }
$HostManifest = [ordered]@{ name = $HostName; description = 'AI Account Center Qwen usage only'; path = (Join-Path $TargetHost $BinaryName); type = 'stdio'; allowed_origins = @('chrome-extension://clobbdmblhillanldmmjnlpbaafbnklj/') }
[IO.File]::WriteAllText($ManifestPath, ($HostManifest | ConvertTo-Json -Depth 4), (New-Object Text.UTF8Encoding($false)))
foreach ($Key in $RegistryPaths) {
  New-Item -Path $Key -Force | Out-Null
  Set-Item -LiteralPath $Key -Value $ManifestPath
  if ((Get-Item -LiteralPath $Key).GetValue('') -ne $ManifestPath) { throw 'Native host registration could not be verified.' }
}
[pscustomobject]@{ installed = $true; extensionId = $ExpectedId; version = $Manifest.version; extensionDirectory = $TargetExtension; collectorDirectory = $TargetHelpers; manifestPath = $ManifestPath; browserPoliciesChanged = $false; extensionInstalled = $false } | ConvertTo-Json -Depth 4
