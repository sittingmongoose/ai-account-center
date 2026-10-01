<#
.SYNOPSIS
Validate or register the four CCS Claude launch tasks for the current Windows user.
.DESCRIPTION
Uses the already installed per-user CCS Claude URI helper and its fixed account
allowlist. Tasks run only on demand in that user's interactive session, with
Limited privileges. Neither mode launches Claude or changes its authentication.
.EXAMPLE
powershell.exe -NoProfile -File windows-claude-launch-tasks.ps1 -Mode Validate
.EXAMPLE
powershell.exe -NoProfile -File windows-claude-launch-tasks.ps1 -Mode Install
#>
[CmdletBinding()]
param(
    [ValidateSet('Validate', 'Install')]
    [string]$Mode = 'Validate'
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-StrictMode -Version Latest

$accountIds = @('platyr', 'gmail', 'party', 'me')
$taskPath = '\'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$userSid = $identity.User.Value
$helper = Join-Path $env:LOCALAPPDATA 'CCS-Claude\ccs-claude.exe'
$desktop = [Environment]::GetFolderPath('Desktop')
if (-not (Test-Path -LiteralPath $helper -PathType Leaf)) {
    throw 'Install the per-user CCS Claude URI helper before registering launch tasks.'
}

$package = Get-AppxPackage -Name 'Claude' | Sort-Object Version -Descending | Select-Object -First 1
if (-not $package -or $package.PackageFamilyName -ne 'Claude_pzs8sxrjxfjjc') {
    throw 'The expected Claude app package is not installed for the current user.'
}
$claudeExecutable = Join-Path $package.InstallLocation 'app\Claude.exe'
if (-not (Test-Path -LiteralPath $claudeExecutable -PathType Leaf)) {
    throw 'The current Claude app executable is unavailable.'
}
$manifest = Get-AppxPackageManifest -Package $package
if (-not (@($manifest.Package.Applications.Application.Id) -contains 'Claude')) {
    throw 'The default Claude application ID is unavailable.'
}

function Get-HelperTarget([string]$AccountId) {
    $start = New-Object Diagnostics.ProcessStartInfo
    $start.FileName = $helper
    $start.Arguments = '--dry-run "ccs-claude://launch/' + $AccountId + '"'
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $process = [Diagnostics.Process]::Start($start)
    try {
        $output = $process.StandardOutput.ReadToEndAsync()
        $errorOutput = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(10000)) {
            $process.Kill()
            throw 'The CCS Claude helper validation timed out.'
        }
        $text = $output.GetAwaiter().GetResult().Trim()
        $errorText = $errorOutput.GetAwaiter().GetResult().Trim()
        if ($process.ExitCode -ne 0 -or $errorText.Length -ne 0) {
            throw 'The CCS Claude helper rejected an expected account mapping.'
        }
        $fields = $text -split "`t", 2
        if ($fields.Count -ne 2 -or $fields[0] -cne $AccountId) {
            throw 'The CCS Claude helper returned an unexpected account mapping.'
        }
        return $fields[1]
    } finally {
        $process.Dispose()
    }
}

function Get-PrincipalSid([string]$UserId) {
    if ($UserId -match '^S-1-') { return $UserId }
    try {
        $account = New-Object Security.Principal.NTAccount($UserId)
        return $account.Translate([Security.Principal.SecurityIdentifier]).Value
    } catch {
        throw 'An existing launch task has an unresolvable owner.'
    }
}

# Validate every helper target and existing task before changing any task.
$plans = @()
$shell = New-Object -ComObject WScript.Shell
try {
    foreach ($id in $accountIds) {
        $target = Get-HelperTarget $id
        if ($id -eq 'gmail') {
            if ($target -cne 'shell:AppsFolder\Claude_pzs8sxrjxfjjc!Claude') {
                throw 'The default Claude helper mapping differs from the expected app ID.'
            }
        } else {
            if ([IO.Path]::GetExtension($target) -ine '.lnk' -or
                [IO.Path]::GetFullPath([IO.Path]::GetDirectoryName($target)) -ine
                    [IO.Path]::GetFullPath($desktop) -or
                -not (Test-Path -LiteralPath $target -PathType Leaf)) {
                throw 'A named Claude helper mapping is not an existing desktop shortcut.'
            }
            $link = $shell.CreateShortcut($target)
            try {
                $profile = Join-Path $env:APPDATA ('Claude-' + $id)
                $expectedArguments = '--user-data-dir="' + $profile + '"'
                if ($link.TargetPath -ine $claudeExecutable -or
                    $link.Arguments -cne $expectedArguments) {
                    throw 'A named Claude shortcut does not select the expected app and profile.'
                }
            } finally {
                [void][Runtime.InteropServices.Marshal]::ReleaseComObject($link)
            }
        }

        $name = 'ccs-claude-' + $id
        $arguments = 'ccs-claude://launch/' + $id
        $existing = @(Get-ScheduledTask -TaskPath $taskPath -TaskName $name -ErrorAction SilentlyContinue)
        if ($existing.Count -gt 1) { throw 'More than one matching Claude launch task exists.' }
        if ($existing.Count -eq 1) {
            $task = $existing[0]
            $actions = @($task.Actions)
            if ((Get-PrincipalSid $task.Principal.UserId) -ne $userSid -or
                [string]$task.Principal.LogonType -notin @('Interactive', 'InteractiveToken') -or
                [string]$task.Principal.RunLevel -ne 'Limited' -or
                $actions.Count -ne 1 -or
                $actions[0].Execute -ine $helper -or
                $actions[0].Arguments -cne $arguments -or
                @($task.Triggers | Where-Object { $null -ne $_ }).Count -ne 0 -or
                -not $task.Settings.Enabled -or
                -not $task.Settings.AllowDemandStart) {
                throw 'An existing Claude launch task conflicts with the expected per-user definition.'
            }
        }
        $plans += [pscustomobject]@{ Id = $id; Name = $name; Arguments = $arguments; Exists = ($existing.Count -eq 1) }
    }
} finally {
    [void][Runtime.InteropServices.Marshal]::ReleaseComObject($shell)
}

$installedThisRun = @()
if ($Mode -eq 'Install') {
    $principal = New-ScheduledTaskPrincipal -UserId $userSid -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
        -MultipleInstances IgnoreNew -ExecutionTimeLimit (New-TimeSpan -Seconds 30)
    foreach ($plan in $plans) {
        if ($plan.Exists) { continue }
        $action = New-ScheduledTaskAction -Execute $helper -Argument $plan.Arguments
        $definition = New-ScheduledTask -Action $action -Principal $principal -Settings $settings `
            -Description 'CCS Claude account launcher; current user, interactive session, on demand only.'
        # No trigger and no Start-ScheduledTask: registration cannot open Claude.
        Register-ScheduledTask -TaskPath $taskPath -TaskName $plan.Name -InputObject $definition | Out-Null
        $installedThisRun += $plan.Name
    }
}

[ordered]@{
    mode = $Mode
    user = $identity.Name
    userSid = $userSid
    taskPath = $taskPath
    helperPath = $helper
    helperSha256 = (Get-FileHash -LiteralPath $helper -Algorithm SHA256).Hash.ToLowerInvariant()
    tasks = @($plans | ForEach-Object {
        [ordered]@{
            id = $_.Id
            name = $_.Name
            installed = $_.Exists -or ($installedThisRun -contains $_.Name)
            interactive = $true
            runLevel = 'Limited'
            onDemandOnly = $true
            launchArguments = $_.Arguments
        }
    })
    installedThisRun = $installedThisRun
    claudeLaunched = $false
    authenticationChanged = $false
} | ConvertTo-Json -Depth 5
