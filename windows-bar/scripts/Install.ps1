param([switch]$NoStart)
$ErrorActionPreference = 'Stop'
$source = Split-Path -Parent $PSScriptRoot
$publish = Join-Path $source 'publish'
$productRoot = Join-Path $env:LOCALAPPDATA 'AI Account Center'
$destination = Join-Path $productRoot 'app'
$exe = Join-Path $destination 'AIAccountCenter.exe'
$legacyDestination = Join-Path $env:LOCALAPPDATA 'CCS Bar\app'
$legacyExe = Join-Path $legacyDestination 'CCSBar.exe'
. (Join-Path $PSScriptRoot 'Install-TaskGuard.ps1')
# Refuse foreign task definitions before staging or executing any installer input.
$ownedTasks = Get-AiAccountCenterInstallTasks $exe $legacyExe
if (-not (Test-Path (Join-Path $publish 'CCSBar.exe'))) { throw 'Build AI Account Center before installing it.' }
$stamp = [DateTimeOffset]::UtcNow.ToString('yyyyMMddTHHmmssZ') + '-' + [Guid]::NewGuid().ToString('N').Substring(0,8)
$stageRoot = Join-Path $productRoot ('staging\' + $stamp)
$stagedApp = Join-Path $stageRoot 'app'
$stagedExe = Join-Path $stagedApp 'AIAccountCenter.exe'
$backup = Join-Path $productRoot ('backups\' + $stamp)
New-Item -ItemType Directory -Force -Path $stagedApp | Out-Null
foreach ($item in Get-ChildItem -LiteralPath $publish) {
    if ($item.Name -eq 'CCSBar.exe') { Copy-Item -LiteralPath $item.FullName -Destination $stagedExe -Force }
    else { Copy-Item -LiteralPath $item.FullName -Destination $stagedApp -Recurse -Force }
}
Copy-Item (Join-Path $source 'LICENSE') (Join-Path $stagedApp 'LICENSE') -Force
# Validate the renamed self-contained apphost before touching the installed app.
$verifyReport = Join-Path $stageRoot 'staged-checks.json'
$verification = Start-Process -FilePath $stagedExe -ArgumentList @('--check', ('"' + $verifyReport + '"')) -PassThru -Wait
if ($verification.ExitCode -ne 0 -or -not (Test-Path $verifyReport) -or -not (Get-Content -Raw $verifyReport | ConvertFrom-Json).passed) { throw 'Staged AI Account Center verification failed. The installed app was not changed.' }
$canonicalTask = $ownedTasks['AI Account Center']
$legacyTask = $ownedTasks['CCS Bar']
$previousTask = if ($canonicalTask) { $canonicalTask } else { $legacyTask }
$startupEnabled = if ($previousTask) { [bool]$previousTask.Settings.Enabled } else { $true }
$startupTriggers = @(if ($previousTask) { $previousTask.Triggers | Where-Object { $null -ne $_ } } else { New-ScheduledTaskTrigger -AtLogOn -User ([System.Security.Principal.WindowsIdentity]::GetCurrent().Name) })
$taskStates = @{}
New-Item -ItemType Directory -Force -Path $backup | Out-Null
foreach ($taskName in @('AI Account Center', 'CCS Bar')) {
    $task = $ownedTasks[$taskName]
    if ($task) {
        $taskStates[$taskName] = $task.State.ToString()
        Export-ScheduledTask -TaskName $taskName | Set-Content -LiteralPath (Join-Path $backup ($taskName + '.xml')) -Encoding Unicode
    }
}
if (Test-Path $destination) { Copy-Item -LiteralPath $destination -Destination (Join-Path $backup 'primary-app') -Recurse -Force }
if (Test-Path $legacyDestination) { Copy-Item -LiteralPath $legacyDestination -Destination (Join-Path $backup 'legacy-app') -Recurse -Force }
# Save shortcut state and identify only files this installation would introduce.
$shell = New-Object -ComObject WScript.Shell
foreach ($shortcutName in @('AI Account Center.lnk', 'CCS Bar.lnk')) {
    $shortcutPath = Join-Path ([Environment]::GetFolderPath('Programs')) $shortcutName
    if (Test-Path $shortcutPath) { Copy-Item -LiteralPath $shortcutPath -Destination (Join-Path $backup $shortcutName) -Force }
}
$introducedDirectories = @()
foreach ($rootPath in @($destination, $legacyDestination)) {
    if (-not (Test-Path -LiteralPath $rootPath)) { $introducedDirectories += $rootPath }
    foreach ($directory in Get-ChildItem -LiteralPath $stagedApp -Recurse -Directory) {
        $relative = $directory.FullName.Substring($stagedApp.Length).TrimStart('\')
        $target = Join-Path $rootPath $relative
        if (-not (Test-Path -LiteralPath $target)) { $introducedDirectories += $target }
    }
}
$introducedFiles = @()
$introducedShortcuts = @()
foreach ($file in Get-ChildItem -LiteralPath $stagedApp -Recurse -File) {
    $relative = $file.FullName.Substring($stagedApp.Length).TrimStart('\')
    $legacyRelative = if ($relative -eq 'AIAccountCenter.exe') { 'CCSBar.exe' } else { $relative }
    foreach ($target in @((Join-Path $destination $relative), (Join-Path $legacyDestination $legacyRelative))) {
        if (-not (Test-Path -LiteralPath $target)) { $introducedFiles += [PSCustomObject]@{Path=$target;Hash=(Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash} }
    }
}
# Revalidate immediately before task/process/install mutation; failures need no rollback.
$currentOwnedTasks = Get-AiAccountCenterInstallTasks $exe $legacyExe
Assert-AiAccountCenterTaskSetUnchanged $ownedTasks $currentOwnedTasks
try {
    # Backups are complete before the running app is stopped.
    foreach ($taskName in $taskStates.Keys) { Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue }
    Get-CimInstance Win32_Process -Filter "Name='CCSBar.exe' OR Name='AIAccountCenter.exe'" | Where-Object { $_.ExecutablePath -eq $exe -or $_.ExecutablePath -eq $legacyExe } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    New-Item -ItemType Directory -Force -Path $destination | Out-Null
    Copy-Item (Join-Path $stagedApp '*') $destination -Recurse -Force
    foreach ($file in Get-ChildItem -LiteralPath $stagedApp -Recurse -File) {
        $relative = $file.FullName.Substring($stagedApp.Length).TrimStart('\')
        if ((Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash -ne (Get-FileHash -LiteralPath (Join-Path $destination $relative) -Algorithm SHA256).Hash) { throw 'Installed app verification failed.' }
    }
    # The old executable remains a current binary alias. Both paths use the
    # existing singleton/event names and private DPAPI store. No credentials move.
    New-Item -ItemType Directory -Force -Path $legacyDestination | Out-Null
    foreach ($item in Get-ChildItem -LiteralPath $stagedApp) {
        if ($item.Name -eq 'AIAccountCenter.exe') { Copy-Item -LiteralPath $item.FullName -Destination $legacyExe -Force }
        else { Copy-Item -LiteralPath $item.FullName -Destination $legacyDestination -Recurse -Force }
    }
    $identity = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
    $action = New-ScheduledTaskAction -Execute $exe -WorkingDirectory $destination
    $principal = New-ScheduledTaskPrincipal -UserId $identity -LogonType Interactive -RunLevel Limited
    $settings = if ($previousTask) { $previousTask.Settings } else {
        New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew
    }
    $settings.Enabled = $startupEnabled
    $registration = @{ TaskName='AI Account Center'; Action=$action; Principal=$principal; Settings=$settings; Description='AI Account Center accounts and usage tray.'; Force=$true }
    if ($startupTriggers.Count -gt 0) { $registration.Trigger = $startupTriggers }
    Register-ScheduledTask @registration | Out-Null
    # Keep the old task as a manual alias without a second logon trigger.
    $aliasSettings = if ($legacyTask) { $legacyTask.Settings } else {
        New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew
    }
    Register-ScheduledTask -TaskName 'CCS Bar' -Action $action -Principal $principal -Settings $aliasSettings -Description 'Compatibility alias for AI Account Center. The AI Account Center task preserves your logon preference.' -Force | Out-Null
    foreach ($shortcutName in @('AI Account Center.lnk', 'CCS Bar.lnk')) {
        $shortcutPath = Join-Path ([Environment]::GetFolderPath('Programs')) $shortcutName
        if (-not (Test-Path $shortcutPath)) { $introducedShortcuts += $shortcutPath }
        $shortcut = $shell.CreateShortcut($shortcutPath)
        $shortcut.TargetPath = $exe; $shortcut.WorkingDirectory = $destination; $shortcut.Description = 'AI Account Center usage and accounts'; $shortcut.Save()
    }
    $started = -not $NoStart -and $startupEnabled
    if ($started) { Start-ScheduledTask -TaskName 'AI Account Center' }
    [PSCustomObject]@{ installed=$true; executable=$exe; startupTask='AI Account Center'; startupEnabled=$startupEnabled; startupTriggerCount=$startupTriggers.Count; compatibilityTask='CCS Bar'; legacyExecutable=$legacyExe; privateStorePreserved=$true; stagedVerificationPassed=$true; rollbackBackup=$backup; started=$started } | ConvertTo-Json -Compress
}
catch {
    $installFailure = $_
    # Do not stop or overwrite a foreign task even while recovering an install.
    try { [void](Get-AiAccountCenterInstallTasks $exe $legacyExe) }
    catch { throw ('Task ownership could not be verified during rollback. Backups remain at ' + $backup + '.') }
    # Unlock only our binaries before restoring the prior installation.
    foreach ($taskName in @('AI Account Center', 'CCS Bar')) { Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue }
    Get-CimInstance Win32_Process -Filter "Name='CCSBar.exe' OR Name='AIAccountCenter.exe'" -ErrorAction SilentlyContinue | Where-Object { $_.ExecutablePath -eq $exe -or $_.ExecutablePath -eq $legacyExe } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    foreach ($file in $introducedFiles) {
        if ((Test-Path -LiteralPath $file.Path) -and (Get-FileHash -LiteralPath $file.Path -Algorithm SHA256 -ErrorAction SilentlyContinue).Hash -eq $file.Hash) { Remove-Item -LiteralPath $file.Path -Force -ErrorAction Continue }
    }
    foreach ($directory in ($introducedDirectories | Sort-Object Length -Descending)) {
        if ((Test-Path -LiteralPath $directory) -and @(Get-ChildItem -LiteralPath $directory -Force -ErrorAction SilentlyContinue).Count -eq 0) { Remove-Item -LiteralPath $directory -Force -ErrorAction Continue }
    }
    foreach ($path in $introducedShortcuts) { Remove-Item -LiteralPath $path -Force -ErrorAction SilentlyContinue }
    foreach ($entry in @(@{Directory='primary-app';Destination=$destination}, @{Directory='legacy-app';Destination=$legacyDestination})) {
        $saved = Join-Path $backup $entry.Directory
        if (Test-Path $saved) { Copy-Item (Join-Path $saved '*') $entry.Destination -Recurse -Force -ErrorAction Continue }
    }
    foreach ($taskName in @('AI Account Center', 'CCS Bar')) {
        $saved = Join-Path $backup ($taskName + '.xml')
        if (Test-Path $saved) { Register-ScheduledTask -TaskName $taskName -Xml (Get-Content -Raw $saved) -Force -ErrorAction Continue | Out-Null }
        else { Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue }
        if ($taskStates[$taskName] -eq 'Running') { Start-ScheduledTask -TaskName $taskName -ErrorAction Continue }
    }
    foreach ($shortcutName in @('AI Account Center.lnk', 'CCS Bar.lnk')) {
        $saved = Join-Path $backup $shortcutName
        if (Test-Path $saved) { Copy-Item -LiteralPath $saved -Destination (Join-Path ([Environment]::GetFolderPath('Programs')) $shortcutName) -Force -ErrorAction Continue }
    }
    throw $installFailure
}
