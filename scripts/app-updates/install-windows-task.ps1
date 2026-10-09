# Registers the fixed updater action; it never runs an update during setup.
$ErrorActionPreference = 'Stop'
$helper = Join-Path $HOME '.ccs\app-updates\app_updates.py'
if (-not (Test-Path -LiteralPath $helper -PathType Leaf)) { throw 'Updater helper is not deployed.' }
$python = (Get-Command python.exe -ErrorAction Stop).Source
# The task must run pythonw.exe: python.exe would open a console window for the whole run.
$pythonw = Join-Path (Split-Path -Parent $python) 'pythonw.exe'
if (-not (Test-Path -LiteralPath $pythonw -PathType Leaf)) { throw "pythonw.exe was not found beside $python. Install a Python that includes pythonw.exe (the python.org installer does) and register the task again." }
$python = $pythonw
$arguments = '"' + $helper + '" --apply --platform windows --task-child'
$action = New-ScheduledTaskAction -Execute $python -Argument $arguments
$principal = New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name) -LogonType Interactive -RunLevel Limited
# -Hidden keeps the whole update flow out of the default Task Scheduler view.
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Minutes 18) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew -Hidden
Register-ScheduledTask -TaskName 'CCS App Updates' -Action $action -Principal $principal -Settings $settings -Force | Out-Null
Write-Output 'CCS App Updates task registered; no updates were run.'
