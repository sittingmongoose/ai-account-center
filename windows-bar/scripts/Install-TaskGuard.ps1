# The decision functions accept metadata only; fixtures never query or change tasks.
function ConvertTo-AiAccountCenterSid([string]$Identity) {
    if ($Identity -match '^S-1-') { return $Identity }
    if ([string]::IsNullOrWhiteSpace($Identity)) { throw 'A task identity is unavailable.' }
    return ([Security.Principal.NTAccount]::new($Identity)).Translate([Security.Principal.SecurityIdentifier]).Value
}

function Assert-AiAccountCenterSecurity($Security, [string]$UserSid) {
    $trusted = @($UserSid, 'S-1-5-18', 'S-1-5-32-544')
    if (-not $Security.DaclPresent -or @($Security.Rules).Count -eq 0 -or
        $Security.OwnerSid -notin $trusted) {
        throw 'An existing task or executable has an unapproved owner or unavailable permissions.'
    }
    # Generic write/all, delete, owner/DACL changes and file/directory write rights.
    # Foreign read/execute grants do not authorize replacing an installed task/file.
    $writeMask = [long]0x500D0156
    foreach ($rule in $Security.Rules) {
        if ($rule.Kind -notin @('Allow', 'Deny') -or [string]::IsNullOrWhiteSpace($rule.Sid)) {
            throw 'An existing task or executable has unsupported permissions.'
        }
        if ($rule.Kind -eq 'Allow' -and $rule.Sid -notin $trusted -and
            (([long]$rule.Mask -band 0xffffffffL) -band $writeMask) -ne 0) {
            throw 'An existing task or executable permits another identity to change it.'
        }
    }
}

function Assert-AiAccountCenterTaskSnapshot($Snapshot, [string]$UserSid, [string[]]$AllowedExecutables) {
    if ($Snapshot.Name -notin @('AI Account Center', 'CCS Bar') -or $Snapshot.TaskPath -ne '\' -or
        $Snapshot.PrincipalSid -ne $UserSid -or $Snapshot.LogonType -notin @('Interactive', 'InteractiveToken') -or
        $Snapshot.RunLevel -ne 'Limited' -or @($Snapshot.Actions).Count -ne 1 -or @($Snapshot.Triggers).Count -gt 1) {
        throw 'An existing tray task conflicts with the expected current-user definition.'
    }
    $action = @($Snapshot.Actions)[0]
    if ($action.Execute -notin $AllowedExecutables -or -not [string]::IsNullOrEmpty([string]$action.Arguments) -or
        $action.WorkingDirectory -ine [IO.Path]::GetDirectoryName($action.Execute)) {
        throw 'An existing tray task has an unapproved executable or action.'
    }
    foreach ($trigger in $Snapshot.Triggers) {
        if ($trigger.Kind -ne 'MSFT_TaskLogonTrigger' -or $trigger.UserSid -ne $UserSid -or
            -not [string]::IsNullOrEmpty([string]$trigger.RepeatInterval) -or
            -not [string]::IsNullOrEmpty([string]$trigger.RepeatDuration)) {
            throw 'An existing tray task has an unapproved startup trigger.'
        }
    }
    Assert-AiAccountCenterSecurity $Snapshot.TaskSecurity $UserSid
    if (@($Snapshot.ExecutableSecurity).Count -ne 3) { throw 'Executable ownership could not be verified.' }
    foreach ($security in $Snapshot.ExecutableSecurity) {
        Assert-AiAccountCenterSecurity $security $UserSid
    }
}

function Get-AiAccountCenterFileSecurity([string]$Path) {
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw 'An existing tray executable path contains a redirect.'
    }
    $acl = Get-Acl -LiteralPath $Path -ErrorAction Stop
    return [pscustomobject]@{
        OwnerSid = $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value
        DaclPresent = $true
        Rules = @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]) | ForEach-Object {
            [pscustomobject]@{ Sid=$_.IdentityReference.Value; Kind=$_.AccessControlType.ToString(); Mask=[long]$_.FileSystemRights }
        })
    }
}

function Assert-AiAccountCenterTaskSetUnchanged($Before, $After) {
    if ($Before.Count -ne $After.Count) { throw 'Tray tasks changed during staging; rerun the installer.' }
    foreach ($name in $Before.Keys) {
        if (-not $After.ContainsKey($name) -or
            [string]::IsNullOrEmpty($Before[$name].AiAccountCenterDefinitionHash) -or
            $Before[$name].AiAccountCenterDefinitionHash -cne $After[$name].AiAccountCenterDefinitionHash) {
            throw 'Tray task definitions or permissions changed during staging; rerun the installer.'
        }
    }
}

function Get-AiAccountCenterInstallTasks([string]$CanonicalExe, [string]$LegacyExe) {
    $userSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $localAppData = [Environment]::GetFolderPath('LocalApplicationData')
    if ($CanonicalExe -ine (Join-Path $localAppData 'AI Account Center\app\AIAccountCenter.exe') -or
        $LegacyExe -ine (Join-Path $localAppData 'CCS Bar\app\CCSBar.exe')) {
        throw 'Tray installation paths must belong to the current user local application directory.'
    }
    # Both destinations are replaced, even when no task references one of them.
    foreach ($executable in @($CanonicalExe, $LegacyExe)) {
        $directory = [IO.Path]::GetDirectoryName($executable)
        foreach ($path in @($executable, $directory, ([IO.Path]::GetDirectoryName($directory)))) {
            if (Test-Path -LiteralPath $path) {
                if (($path -eq $executable -and -not (Test-Path -LiteralPath $path -PathType Leaf)) -or
                    ($path -ne $executable -and -not (Test-Path -LiteralPath $path -PathType Container))) {
                    throw 'An existing tray installation path has an unexpected file type.'
                }
                Assert-AiAccountCenterSecurity (Get-AiAccountCenterFileSecurity $path) $userSid
            }
        }
    }
    $names = @('AI Account Center', 'CCS Bar')
    # Enumerating with Stop distinguishes an absent task from unreadable task metadata.
    $tasks = @(Get-ScheduledTask -TaskPath '\' -ErrorAction Stop | Where-Object { $_.TaskName -in $names })
    $owned = @{}
    if ($tasks.Count -eq 0) { return $owned }
    $scheduler = $null; $folder = $null
    try {
        $scheduler = New-Object -ComObject 'Schedule.Service'
        $scheduler.Connect()
        $folder = $scheduler.GetFolder('\')
        foreach ($name in $names) {
            $matches = @($tasks | Where-Object { $_.TaskName -eq $name })
            if ($matches.Count -gt 1) { throw 'More than one matching tray task exists.' }
            if ($matches.Count -eq 0) { continue }
            $task = $matches[0]
            $registered = $null
            try {
                $registered = $folder.GetTask($name)
                $sddl = $registered.GetSecurityDescriptor(7)
                $descriptor = [Security.AccessControl.RawSecurityDescriptor]::new($sddl)
                $hasher = [Security.Cryptography.SHA256]::Create()
                try {
                    $definition = [Text.Encoding]::UTF8.GetBytes([string]$registered.Xml + "`n" + [string]$sddl)
                    $definitionHash = [BitConverter]::ToString($hasher.ComputeHash($definition)).Replace('-', '')
                } finally { $hasher.Dispose() }
                $taskSecurity = [pscustomobject]@{
                    OwnerSid = $descriptor.Owner.Value
                    DaclPresent = ($null -ne $descriptor.DiscretionaryAcl)
                    Rules = @($descriptor.DiscretionaryAcl | ForEach-Object {
                        $kind = if ($_.AceType.ToString() -eq 'AccessAllowed') { 'Allow' }
                                elseif ($_.AceType.ToString() -eq 'AccessDenied') { 'Deny' } else { 'Unsupported' }
                        [pscustomobject]@{ Sid=$_.SecurityIdentifier.Value; Kind=$kind; Mask=[long]$_.AccessMask }
                    })
                }
            } finally {
                if ($registered) { [void][Runtime.InteropServices.Marshal]::ReleaseComObject($registered) }
            }
            $actions = @($task.Actions | ForEach-Object {
                [pscustomobject]@{ Execute=[string]$_.Execute; Arguments=[string]$_.Arguments; WorkingDirectory=[string]$_.WorkingDirectory }
            })
            if ($actions.Count -ne 1 -or $actions[0].Execute -notin @($CanonicalExe, $LegacyExe)) {
                throw 'An existing tray task has an unapproved executable or action.'
            }
            $executable = $actions[0].Execute
            if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
                throw 'An existing tray task executable is unavailable.'
            }
            $directory = [IO.Path]::GetDirectoryName($executable)
            $security = @(@($executable, $directory, ([IO.Path]::GetDirectoryName($directory))) | ForEach-Object {
                Get-AiAccountCenterFileSecurity $_
            })
            $snapshot = [pscustomobject]@{
                Name=$name; TaskPath=[string]$task.TaskPath
                PrincipalSid=(ConvertTo-AiAccountCenterSid $task.Principal.UserId)
                LogonType=$task.Principal.LogonType.ToString(); RunLevel=$task.Principal.RunLevel.ToString()
                Actions=$actions; TaskSecurity=$taskSecurity; ExecutableSecurity=$security
                Triggers=@($task.Triggers | Where-Object { $null -ne $_ } | ForEach-Object {
                    [pscustomobject]@{ Kind=$_.CimClass.CimClassName; UserSid=(ConvertTo-AiAccountCenterSid $_.UserId)
                        RepeatInterval=[string]$_.Repetition.Interval; RepeatDuration=[string]$_.Repetition.Duration }
                })
            }
            Assert-AiAccountCenterTaskSnapshot $snapshot $userSid @($CanonicalExe, $LegacyExe)
            $task | Add-Member -NotePropertyName AiAccountCenterDefinitionHash -NotePropertyValue $definitionHash -Force
            $owned[$name] = $task
        }
    } finally {
        if ($folder) { [void][Runtime.InteropServices.Marshal]::ReleaseComObject($folder) }
        if ($scheduler) { [void][Runtime.InteropServices.Marshal]::ReleaseComObject($scheduler) }
    }
    return $owned
}
