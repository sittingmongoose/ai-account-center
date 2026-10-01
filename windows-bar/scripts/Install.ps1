param([switch]$NoStart)
$ErrorActionPreference = 'Stop'
$source = Split-Path -Parent $PSScriptRoot
$publish = Join-Path $source 'publish'
$destination = Join-Path $env:LOCALAPPDATA 'CCS Bar\app'
$exe = Join-Path $destination 'CCSBar.exe'
if (-not (Test-Path (Join-Path $publish 'CCSBar.exe'))) { throw 'Build CCS Bar before installing it.' }
# Stop only our exact installation, leaving every provider and other app alone.
$installedTask = Get-ScheduledTask -TaskName 'CCS Bar' -ErrorAction SilentlyContinue
if ($installedTask) { Stop-ScheduledTask -TaskName 'CCS Bar' -ErrorAction SilentlyContinue }
Get-CimInstance Win32_Process -Filter "Name='CCSBar.exe'" | Where-Object { $_.ExecutablePath -eq $exe } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
New-Item -ItemType Directory -Force -Path $destination | Out-Null
Copy-Item (Join-Path $publish '*') $destination -Recurse -Force
Copy-Item (Join-Path $source 'LICENSE') (Join-Path $destination 'LICENSE') -Force
$identity = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
$action = New-ScheduledTaskAction -Execute $exe -WorkingDirectory $destination
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $identity
$principal = New-ScheduledTaskPrincipal -UserId $identity -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew
Register-ScheduledTask -TaskName 'CCS Bar' -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Description 'CCS accounts and usage tray. Connects only to the configured CCS dashboard.' -Force | Out-Null
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut((Join-Path ([Environment]::GetFolderPath('Programs')) 'CCS Bar.lnk'))
$shortcut.TargetPath = $exe
$shortcut.WorkingDirectory = $destination
$shortcut.Description = 'CCS usage and accounts'
$shortcut.Save()
if (-not $NoStart) { Start-ScheduledTask -TaskName 'CCS Bar' }
[PSCustomObject]@{ installed = $true; executable = $exe; startupTask = 'CCS Bar'; started = (-not $NoStart) } | ConvertTo-Json -Compress
