<#
.SYNOPSIS
Validate or register the CCS Claude launch tasks for the current Windows user.
.DESCRIPTION
Takes the account IDs from -AccountIds or -AccountListFile (a generated list,
one ID per line) and validates each with the shared safe-ID rule. Uses the
already installed per-user CCS Claude URI helper, which reads the same account
set from its generated sibling file. -DefaultAccountId selects the profile
that maps to the default Store app target; every other ID maps to its exact
saved named profile. Tasks run only on demand in that user's interactive
session, with Limited privileges. Neither mode launches Claude or changes its
authentication.
.EXAMPLE
powershell.exe -NoProfile -File windows-claude-launch-tasks.ps1 -Mode Validate -AccountIds work,home -DefaultAccountId work
.EXAMPLE
powershell.exe -NoProfile -File windows-claude-launch-tasks.ps1 -Mode Install -AccountListFile .\ccs-claude-accounts.txt -DefaultAccountId work
#>
[CmdletBinding()]
param(
    [ValidateSet('Validate', 'Install')]
    [string]$Mode = 'Validate',
    [string[]]$AccountIds,
    [string]$AccountListFile,
    [string]$DefaultAccountId
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-StrictMode -Version Latest

$accountIdPattern = '^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$'
function Assert-AccountId([string]$Value, [string]$Where) {
    if ([string]::IsNullOrEmpty($Value) -or $Value -cnotmatch $accountIdPattern) {
        throw "An account ID in $Where is not a valid safe profile ID."
    }
}

$accountIds = @()
if ($AccountIds -and $AccountIds.Count -gt 0) { $accountIds += $AccountIds }
if (-not [string]::IsNullOrEmpty($AccountListFile)) {
    $lines = Get-Content -LiteralPath $AccountListFile -ErrorAction Stop
    foreach ($line in $lines) {
        $trimmed = $line.Trim()
        if ($trimmed.Length -eq 0 -or $trimmed.StartsWith('#')) { continue }
        $accountIds += $trimmed
    }
}
if ($accountIds.Count -eq 0) {
    throw 'Pass the Claude account IDs with -AccountIds or -AccountListFile.'
}
if ($accountIds.Count -gt 64) { throw 'Too many Claude account IDs (at most 64).' }
foreach ($id in $accountIds) { Assert-AccountId $id 'the account list' }
$distinct = @($accountIds | Sort-Object -Unique -CaseSensitive)
if ($distinct.Count -ne $accountIds.Count) { throw 'The Claude account list has duplicate IDs.' }
if (-not [string]::IsNullOrEmpty($DefaultAccountId)) {
    Assert-AccountId $DefaultAccountId '-DefaultAccountId'
    if (-not ($accountIds -ccontains $DefaultAccountId)) {
        throw 'The default account ID is not in the account list.'
    }
}
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

function Get-ResolvedLaunchPlan([string]$AccountId) {
    $start = New-Object Diagnostics.ProcessStartInfo
    $start.FileName = $helper
    $start.Arguments = '--describe "ccs-claude://launch/' + $AccountId + '"'
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $process = [Diagnostics.Process]::Start($start)
    try {
        $output = $process.StandardOutput.ReadToEndAsync()
        $errors = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(10000)) {
            $process.Kill()
            throw 'The Claude helper launch-plan validation timed out.'
        }
        $text = $output.GetAwaiter().GetResult()
        $errorText = $errors.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0 -or $errorText.Length -ne 0 -or $text.Length -gt 8192) {
            throw 'The Claude helper rejected the saved profile launch plan.'
        }
        return $text | ConvertFrom-Json
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
        if (-not [string]::IsNullOrEmpty($DefaultAccountId) -and $id -ceq $DefaultAccountId) {
            if ($target -cne 'shell:AppsFolder\Claude_pzs8sxrjxfjjc!Claude') {
                throw 'The default Claude helper mapping differs from the expected app ID.'
            }
        } else {
            $profile = Join-Path $env:APPDATA ('Claude-' + $id)
            $expectedArguments = '--user-data-dir="' + $profile + '"'
            if ([IO.Path]::GetExtension($target) -ieq '.lnk') {
                if ([IO.Path]::GetFullPath([IO.Path]::GetDirectoryName($target)) -ine
                        [IO.Path]::GetFullPath($desktop) -or
                    -not (Test-Path -LiteralPath $target -PathType Leaf)) {
                    throw 'A legacy named Claude mapping is not an existing desktop shortcut.'
                }
                $link = $shell.CreateShortcut($target)
                try {
                    if ($link.TargetPath -ine $claudeExecutable -or
                        $link.Arguments -cne $expectedArguments) {
                        throw 'A legacy Claude shortcut does not select the current app and saved profile.'
                    }
                } finally {
                    [void][Runtime.InteropServices.Marshal]::ReleaseComObject($link)
                }
            } else {
                $resolved = Get-ResolvedLaunchPlan $id
                if ($target -ine $claudeExecutable -or
                    $resolved.schema -ne 1 -or $resolved.profileId -cne $id -or
                    $resolved.executable -ine $claudeExecutable -or
                    $resolved.arguments -cne $expectedArguments -or
                    $resolved.profilePath -ine $profile -or
                    $null -ne $resolved.defaultAppTarget -or
                    -not (Test-Path -LiteralPath $profile -PathType Container)) {
                    throw 'The Claude helper does not select the current app and exact saved profile.'
                }
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
