# Offline checks for the installer scripts: they parse, and the task guard accepts only the two approved logon
# task actions (no arguments, or exactly --background). Metadata fixtures only; no task is queried or changed.
$ErrorActionPreference = 'Stop'
$results = [ordered]@{}
foreach ($script in @('Install.ps1', 'Install-TaskGuard.ps1', 'Build.ps1', 'Check.ps1')) {
    $tokens = $null; $errors = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot $script), [ref]$tokens, [ref]$errors)
    $results["parses_$script"] = @($errors).Count -eq 0
}
. (Join-Path $PSScriptRoot 'Install-TaskGuard.ps1')
$sid = 'S-1-5-21-1000-1000-1000-1001'
$exe = 'C:\Users\fixture\AppData\Local\AI Account Center\app\AIAccountCenter.exe'
$security = [PSCustomObject]@{ DaclPresent = $true; OwnerSid = $sid; Rules = @([PSCustomObject]@{ Kind = 'Allow'; Sid = $sid; Mask = 0x1F01FF }) }
function Snapshot([string]$Arguments) {
    [PSCustomObject]@{
        Name = 'AI Account Center'; TaskPath = '\'; PrincipalSid = $sid; LogonType = 'Interactive'; RunLevel = 'Limited'
        Actions = @([PSCustomObject]@{ Execute = $exe; Arguments = $Arguments; WorkingDirectory = [IO.Path]::GetDirectoryName($exe) })
        Triggers = @([PSCustomObject]@{ Kind = 'MSFT_TaskLogonTrigger'; UserSid = $sid; RepeatInterval = ''; RepeatDuration = '' })
        TaskSecurity = $security; ExecutableSecurity = @($security, $security, $security)
    }
}
function Accepts([string]$Arguments) { try { Assert-AiAccountCenterTaskSnapshot (Snapshot $Arguments) $sid @($exe); $true } catch { $false } }
$results['guard_accepts_no_arguments'] = Accepts ''
$results['guard_accepts_background'] = Accepts '--background'
$results['guard_rejects_other_arguments'] = -not (Accepts '--evil') -and -not (Accepts '--background --check x') -and -not (Accepts '"C:\other.exe"')
$installer = Get-Content -Raw (Join-Path $PSScriptRoot 'Install.ps1')
$results['installer_creates_desktop_shortcut'] = $installer -match "GetFolderPath\('Desktop'\)" -and $installer -match 'Desktop-AI Account Center\.lnk'
$results['installer_sets_shortcut_icon'] = $installer -match 'IconLocation' -and $installer -match 'TrayLight\.ico' -and $installer -match 'TrayDark\.ico'
$results['installer_starts_task_in_background'] = $installer -match "-Argument '--background'"
$passed = -not ($results.Values -contains $false)
[PSCustomObject]@{ passed = $passed; checks = $results } | ConvertTo-Json -Depth 3
if (-not $passed) { exit 1 }
