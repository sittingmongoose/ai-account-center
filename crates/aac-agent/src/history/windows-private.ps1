
$ErrorActionPreference='Stop'
[Console]::InputEncoding=[System.Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false)
try {
  $q=([Console]::In.ReadToEnd() | ConvertFrom-Json)
  if (($q.kind -ne 'file') -and ($q.kind -ne 'directory')) { throw 'refused' }
  $item=Get-Item -LiteralPath ([string]$q.path) -Force -ErrorAction Stop
  if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'refused' }
  $parent=[IO.Path]::GetDirectoryName($item.FullName)
  while ($parent) {
    $parentItem=Get-Item -LiteralPath $parent -Force -ErrorAction Stop
    if (-not $parentItem.PSIsContainer -or (($parentItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) { throw 'refused' }
    $next=[IO.Path]::GetDirectoryName($parent)
    if ($next -eq $parent) { break }
    $parent=$next
  }
  if (($q.kind -eq 'directory') -ne [bool]$item.PSIsContainer) { throw 'refused' }
  $acl=Get-Acl -LiteralPath ([string]$q.path) -ErrorAction Stop
  if (-not $acl.AreAccessRulesCanonical) { throw 'refused' }
  $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value
  $owner=$acl.GetOwner([Security.Principal.SecurityIdentifier]).Value
  if ($owner -ne $sid) { throw 'refused' }
  $allowed=@($sid,'S-1-5-18','S-1-5-32-544')
  $userAllow=$false
  $rules=$acl.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier])
  if ($rules.Count -lt 1) { throw 'refused' }
  foreach ($rule in $rules) {
    if ($rule.AccessControlType -eq [Security.AccessControl.AccessControlType]::Allow) {
      if ($allowed -notcontains $rule.IdentityReference.Value) { throw 'refused' }
      if (($rule.IdentityReference.Value -eq $sid) -and (($rule.FileSystemRights -band [Security.AccessControl.FileSystemRights]::FullControl) -eq [Security.AccessControl.FileSystemRights]::FullControl)) { $userAllow=$true }
    }
  }
  if (-not $userAllow) { throw 'refused' }
  [Console]::Out.Write('{"private":true}')
} catch { [Console]::Out.Write('{"private":false}'); exit 1 }
