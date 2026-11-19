$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$support = [IO.Path]::GetFullPath($env:APPDATA)
$trashRoot = [IO.Path]::Combine($env:USERPROFILE, '.ccs', 'trash', 'claude')
$helper = [IO.Path]::Combine($env:LOCALAPPDATA, 'CCS-Claude', 'ccs-claude.exe')
$accountsFile = [IO.Path]::Combine($env:LOCALAPPDATA, 'CCS-Claude', 'ccs-claude-accounts.txt')
function Fail([int]$code) { exit $code }
function Add-AccountId([string]$id) { if (-not (Test-Path -LiteralPath $accountsFile -PathType Leaf)) { return }; foreach ($line in @(Get-Content -LiteralPath $accountsFile)) { if ($line.Trim() -ceq $id) { return } }; $text = [IO.File]::ReadAllText($accountsFile); if (($text.Length -gt 0) -and -not $text.EndsWith([string][char]10)) { Add-Content -LiteralPath $accountsFile -Value '' -Encoding UTF8 }; Add-Content -LiteralPath $accountsFile -Value $id -Encoding UTF8 }
function Drop-AccountId([string]$id) { if (-not (Test-Path -LiteralPath $accountsFile -PathType Leaf)) { return }; $kept = @(Get-Content -LiteralPath $accountsFile | Where-Object { $_.Trim() -cne $id }); if ($kept.Count -eq 0) { Remove-Item -LiteralPath $accountsFile -Force } else { Set-Content -LiteralPath $accountsFile -Value $kept -Encoding UTF8 } }
function Check-Profile([string]$path) { $full = [IO.Path]::GetFullPath($path); if ([IO.Path]::GetDirectoryName($full) -ne $support -or -not [IO.Path]::GetFileName($full).StartsWith('Claude')) { Fail 1 }; return $full }
function Check-Link([string]$path) { $full = [IO.Path]::GetFullPath($path); $dir = [IO.Path]::GetDirectoryName($full); $name = [IO.Path]::GetFileName($full); if (($dir -ne [Environment]::GetFolderPath('Desktop') -and $dir -ne [Environment]::GetFolderPath('Programs')) -or -not $name.StartsWith('Claude (') -or -not $name.EndsWith(').lnk')) { Fail 1 }; return $full }
function Claude-Exe { $package = Get-AppxPackage -Name 'Claude' | Sort-Object Version -Descending | Select-Object -First 1; if (-not $package) { Fail 3 }; $exe = Join-Path $package.InstallLocation 'app\Claude.exe'; if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) { Fail 3 }; return $exe }
function Make-Links([string[]]$links, [string]$profile) { $exe = Claude-Exe; $shell = New-Object -ComObject WScript.Shell; foreach ($link in $links) { $s = $shell.CreateShortcut($link); $s.TargetPath = $exe; $s.Arguments = '--user-data-dir="' + $profile + '"'; $s.WorkingDirectory = Split-Path -Parent $exe; $s.IconLocation = $exe + ',0'; $s.Save() } }
function Make-Task([string]$id) { $action = New-ScheduledTaskAction -Execute $helper -Argument ('ccs-claude://launch/' + $id); $principal = New-ScheduledTaskPrincipal -UserId ($env:USERDOMAIN + '\' + $env:USERNAME) -LogonType Interactive -RunLevel Limited; Register-ScheduledTask -TaskPath '\' -TaskName ('ccs-claude-' + $id) -Action $action -Principal $principal -Settings (New-ScheduledTaskSettingsSet) | Out-Null }
function Drop-Task([string]$id) { Get-ScheduledTask -TaskPath '\' -TaskName ('ccs-claude-' + $id) -ErrorAction SilentlyContinue | Unregister-ScheduledTask -Confirm:$false }
function Done($value) { [Console]::Write((ConvertTo-Json -Compress $value)); exit 0 }
if ($request.profileId) {
  if ($request.profileId -cnotmatch '^[a-z][a-z0-9-]{1,31}$') { Fail 1 }
  $id = $request.profileId
}
if ($request.trashName) {
  if ($request.trashName -cnotmatch '^[a-z][a-z0-9-]{1,31}-[0-9]{8}T[0-9]{6}Z$') { Fail 1 }
  $target = Join-Path $trashRoot $request.trashName
}
if ($request.profilePath) {
  $profile = Check-Profile $request.profilePath
  $links = @()
  if ($request.launcherPath) { $links += Check-Link $request.launcherPath }
  if ($request.startMenuPath) { $links += Check-Link $request.startMenuPath }
}
switch -CaseSensitive ($request.op) {
  'create' {
    $profile = Join-Path $support ('Claude-' + $id)
    $links = @((Join-Path ([Environment]::GetFolderPath('Desktop')) ('Claude (' + $id + ').lnk')), (Join-Path ([Environment]::GetFolderPath('Programs')) ('Claude (' + $id + ').lnk')))
    if (Test-Path -LiteralPath $profile) { Fail 3 }; foreach ($link in $links) { if (Test-Path -LiteralPath $link) { Fail 3 } }
    if (Get-ScheduledTask -TaskPath '\' -TaskName ('ccs-claude-' + $id) -ErrorAction SilentlyContinue) { Fail 3 }
    New-Item -ItemType Directory -Path $profile | Out-Null
    try { Make-Links $links $profile; Make-Task $id; Add-AccountId $id } catch { foreach ($link in $links) { Remove-Item -LiteralPath $link -Force -ErrorAction SilentlyContinue }; Drop-Task $id; Drop-AccountId $id; Remove-Item -LiteralPath $profile -Force -ErrorAction SilentlyContinue; Fail 1 }
    Done ([ordered]@{ launcherName = 'Claude (' + $id + ')'; launcherPath = $links[0]; startMenuPath = $links[1]; profilePath = $profile })
  }
  'undo' {
    foreach ($link in $links) { Remove-Item -LiteralPath $link -Force -ErrorAction SilentlyContinue }
    Drop-Task $id
    Drop-AccountId $id
    if ((Test-Path -LiteralPath $profile) -and @(Get-ChildItem -LiteralPath $profile -Force).Count -eq 0) { Remove-Item -LiteralPath $profile -Force }
    Done @{ ok = $true }
  }
  'state' {
    $needle = '--user-data-dir="' + $profile + '"'
    $found = @(Get-CimInstance Win32_Process -Filter "Name = 'claude.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($needle) })
    Done @{ running = ($found.Count -gt 0) }
  }
  'session' {
    $configPath = Join-Path $profile 'config.json'
    if (-not (Test-Path -LiteralPath $configPath -PathType Leaf)) { Done @{ signedIn = $false } }
    if ((Get-Item -LiteralPath $configPath -Force).Length -gt 1048576) { Fail 1 }
    $config = $null; try { $config = Get-Content -LiteralPath $configPath -Raw -Encoding UTF8 | ConvertFrom-Json } catch { Done @{ signedIn = $false } }
    $uuid = $config.lastKnownAccountUuid; $cache = $config.'oauth:tokenCacheV2'
    $uuidOk = ($uuid -is [string]) -and ($uuid -match '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$')
    Done @{ signedIn = [bool]($uuidOk -and ($cache -is [string]) -and ($cache.Length -gt 0)) }
  }
  'trash' {
    New-Item -ItemType Directory -Path $trashRoot -Force | Out-Null
    if (Test-Path -LiteralPath $target) { Fail 1 }
    $moved = $false; if (Test-Path -LiteralPath $profile) { if ([IO.Path]::GetPathRoot($profile) -ne [IO.Path]::GetPathRoot([IO.Path]::GetFullPath($trashRoot))) { Done @{ result = 'cross_volume' } }; [IO.Directory]::Move($profile, $target); $moved = $true } else { New-Item -ItemType Directory -Path $target | Out-Null }
    try { foreach ($link in $links) { Remove-Item -LiteralPath $link -Force -ErrorAction SilentlyContinue }; Drop-Task $id } catch { if ($moved) { [IO.Directory]::Move($target, $profile) }; Fail 1 }
    Done @{ result = 'moved' }
  }
  'restore' {
    if (-not (Test-Path -LiteralPath $target -PathType Container) -or (Test-Path -LiteralPath $profile)) { Fail 3 }
    [IO.Directory]::Move($target, $profile)
    Make-Links $links $profile; Drop-Task $id; Make-Task $id
    Done @{ ok = $true }
  }
  'purge' {
    if (Test-Path -LiteralPath $target) { if ((Get-Item -LiteralPath $target -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { Fail 1 }; Remove-Item -LiteralPath $target -Recurse -Force }
    Done @{ ok = $true }
  }
}
Fail 1
