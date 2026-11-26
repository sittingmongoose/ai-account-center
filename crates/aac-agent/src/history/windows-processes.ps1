$ErrorActionPreference='Stop'
Add-Type @'
using System; using System.Runtime.InteropServices;
public class AACHistoryArgv {
 [DllImport("shell32.dll", SetLastError=true)] public static extern IntPtr CommandLineToArgvW([MarshalAs(UnmanagedType.LPWStr)] string command, out int count);
 [DllImport("kernel32.dll")] public static extern IntPtr LocalFree(IntPtr pointer);
}
'@
$rows=[Collections.Generic.List[object]]::new()
foreach ($p in @(Get-CimInstance Win32_Process -Filter "Name='Claude.exe'")) {
 if ($p.CommandLine -isnot [string]) {throw 'unknown process'}
 $count=0; $ptr=[AACHistoryArgv]::CommandLineToArgvW($p.CommandLine,[ref]$count)
 if ($ptr -eq [IntPtr]::Zero -or $count -lt 1 -or $count -ge 1024) {throw 'unknown arguments'}
 try { $argv=[Collections.Generic.List[string]]::new();for($i=0;$i -lt $count;$i++) {$v=[Runtime.InteropServices.Marshal]::ReadIntPtr($ptr,$i*[IntPtr]::Size);$argv.Add([Runtime.InteropServices.Marshal]::PtrToStringUni($v))};$rows.Add($argv.ToArray()) }
 finally {[void][AACHistoryArgv]::LocalFree($ptr)}
}
ConvertTo-Json -InputObject @($rows.ToArray()) -Depth 5 -Compress
