//! Windows process reads and stops (Python `windows_processes`, `windows_owned_pids`,
//! `windows_context`, `live_contexts`, `terminate_cli`'s Windows branch).
#![allow(unsafe_code)]
use super::{Process, ProcessTable, UpdateFailure};
use std::collections::{HashMap, HashSet};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    Security::{EqualSid, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser},
    System::{
        Diagnostics::Debug::ReadProcessMemory,
        RemoteDesktop::ProcessIdToSessionId,
        Threading::{
            GetCurrentProcess, GetExitCodeProcess, GetProcessTimes, IsWow64Process, OpenProcess,
            OpenProcessToken, PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_TERMINATE, PROCESS_VM_READ, TerminateProcess,
        },
    },
};

const LINEAGE_LIMIT: usize = super::processes::LINEAGE_LIMIT;
const STILL_ACTIVE: u32 = 259;

/// The CIM inventory script (same filter as the Python helper).
const SCRIPT: &str = r#"$ErrorActionPreference='Stop';
$npm=[IO.Path]::Combine($env:APPDATA,'npm','node_modules');
$rows=@(); $lineage=New-Object Text.StringBuilder; foreach($p in Get-CimInstance Win32_Process){
 $born=0; if($p.CreationDate){$born=$p.CreationDate.ToFileTimeUtc()};
 [void]$lineage.Append([string]$p.ProcessId+','+[string]$p.ParentProcessId+','+[string]$born+';');
 $tree=[bool]($p.ExecutablePath -and $p.ExecutablePath.StartsWith($npm,[StringComparison]::OrdinalIgnoreCase));
 if($p.Name -notin @('ChatGPT.exe','Claude.exe','codex.exe','claude.exe','agy.exe','omp.exe','node.exe','muse.exe','muse-bin.exe','T3 Code (Nightly).exe','t3-resource-monitor.exe','cursorsandbox.exe','rg.exe','OpenConsole.exe','elevate.exe','ZCode.exe','muse-acp.exe') -and -not $tree){continue};
 if(-not $p.ExecutablePath){continue};
 try{$started=[Diagnostics.Process]::GetProcessById($p.ProcessId).StartTime.ToFileTimeUtc().ToString()}catch{continue};
 $rows+=@{pid=[int]$p.ProcessId;ppid=[int]$p.ParentProcessId;exe=$p.ExecutablePath;args=$p.CommandLine;identity=$started;session=[int]$p.SessionId}
}; ConvertTo-Json -InputObject @{rows=@($rows);tree=$lineage.ToString()} -Compress -Depth 4"#;

struct Handle(HANDLE);

impl Handle {
    fn open(access: u32, pid: u32) -> Option<Self> {
        // SAFETY: OpenProcess has no pointer arguments; a null result is checked.
        let handle = unsafe { OpenProcess(access, 0, pid) };
        (!handle.is_null()).then_some(Self(handle))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenProcess/OpenProcessToken and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

fn token_user(process: HANDLE) -> Option<Vec<u8>> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: token is a valid out pointer.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let token = Handle(token);
    let mut needed: u32 = 256;
    for _ in 0..2 {
        let mut buffer = vec![0_u8; needed as usize];
        // SAFETY: buffer has `len` writable bytes; needed is a valid out pointer.
        let ok = unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut needed,
            )
        };
        if ok != 0 {
            return Some(buffer);
        }
        if needed as usize <= buffer.len() || needed > 4096 {
            return None;
        }
    }
    None
}

fn same_user(mine: &[u8], theirs: &[u8]) -> bool {
    // SAFETY: both buffers hold a TOKEN_USER written by GetTokenInformation; its SID
    // pointer points inside the same buffer, which outlives this call.
    unsafe {
        let mine = &*mine.as_ptr().cast::<TOKEN_USER>();
        let theirs = &*theirs.as_ptr().cast::<TOKEN_USER>();
        EqualSid(mine.User.Sid, theirs.User.Sid) != 0
    }
}

/// Python `windows_owned_pids`: the pids among these that run under this user's SID.
fn owned_pids(pids: &[u32]) -> Result<HashSet<u32>, UpdateFailure> {
    // SAFETY: GetCurrentProcess returns a pseudo handle that needs no closing.
    let mine = token_user(unsafe { GetCurrentProcess() })
        .ok_or_else(|| UpdateFailure::new("restart_context"))?;
    Ok(pids
        .iter()
        .copied()
        .filter(|pid| {
            Handle::open(PROCESS_QUERY_LIMITED_INFORMATION, *pid)
                .and_then(|process| token_user(process.0))
                .is_some_and(|theirs| same_user(&mine, &theirs))
        })
        .collect())
}

/// The process's creation FILETIME as Python's identity string, or `None`.
pub(super) fn started(pid: u32) -> Option<String> {
    let process = Handle::open(PROCESS_QUERY_LIMITED_INFORMATION, pid)?;
    creation(&process)
}

fn creation(process: &Handle) -> Option<String> {
    let mut times = [windows_sys::Win32::Foundation::FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    }; 4];
    let [created, exited, kernel, user] = &mut times;
    // SAFETY: four valid FILETIME out pointers.
    if unsafe { GetProcessTimes(process.0, created, exited, kernel, user) } == 0 {
        return None;
    }
    Some(
        ((u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime))
            .to_string(),
    )
}

/// Python `windows_arguments`: CommandLineToArgvW rules, without the call.
pub(super) fn arguments(value: &str) -> Vec<String> {
    if value.is_empty() || value.len() > 65_536 {
        return Vec::new();
    }
    let chars: Vec<char> = value.chars().collect();
    let mut args = Vec::new();
    let mut index = 0;
    // argv[0]: quotes toggle, no escapes.
    let mut first = String::new();
    let mut quoted = false;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '"' {
            quoted = !quoted;
        } else if !quoted && (ch == ' ' || ch == '\t') {
            break;
        } else {
            first.push(ch);
        }
        index += 1;
    }
    args.push(first);
    loop {
        while index < chars.len() && (chars[index] == ' ' || chars[index] == '\t') {
            index += 1;
        }
        if index >= chars.len() {
            break;
        }
        let mut arg = String::new();
        let mut quoted = false;
        while index < chars.len() {
            let ch = chars[index];
            if !quoted && (ch == ' ' || ch == '\t') {
                break;
            }
            if ch == '\\' {
                let mut slashes = 0;
                while index < chars.len() && chars[index] == '\\' {
                    slashes += 1;
                    index += 1;
                }
                if index < chars.len() && chars[index] == '"' {
                    arg.extend(std::iter::repeat_n('\\', slashes / 2));
                    if slashes % 2 == 1 {
                        arg.push('"');
                        index += 1;
                    }
                } else {
                    arg.extend(std::iter::repeat_n('\\', slashes));
                }
                continue;
            }
            if ch == '"' {
                if quoted && chars.get(index + 1) == Some(&'"') {
                    arg.push('"');
                    index += 2;
                    continue;
                }
                quoted = !quoted;
                index += 1;
                continue;
            }
            arg.push(ch);
            index += 1;
        }
        args.push(arg);
    }
    args
}

/// Python `windows_processes`.
pub(super) async fn processes(
    powershell: std::path::PathBuf,
) -> Result<ProcessTable, UpdateFailure> {
    let context = || UpdateFailure::new("restart_context");
    let mut command = tokio::process::Command::new(powershell);
    command
        .args(crate::apps::ps_command(SCRIPT.to_owned()))
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .creation_flags(0x0800_0000);
    let output = tokio::time::timeout(std::time::Duration::from_secs(20), command.output())
        .await
        .map_err(|_| UpdateFailure::new("timeout"))?
        .map_err(|_| context())?;
    if !output.status.success() || output.stdout.len() > 2 * 1024 * 1024 {
        return Err(context());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let data: serde_json::Value =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|_| context())?;
    let rows = match &data["rows"] {
        serde_json::Value::Array(rows) => rows.clone(),
        serde_json::Value::Object(_) => vec![data["rows"].clone()],
        _ => return Err(context()),
    };
    let number = |row: &serde_json::Value, key: &str| {
        row[key]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
    };
    let pids: Vec<u32> = rows.iter().filter_map(|row| number(row, "pid")).collect();
    let owned = tokio::task::spawn_blocking(move || owned_pids(&pids))
        .await
        .map_err(|_| context())??;
    let mut items = Vec::new();
    for row in rows
        .iter()
        .filter(|row| number(row, "pid").is_some_and(|pid| owned.contains(&pid)))
        .take(2048)
    {
        let (Some(pid), Some(ppid), Some(exe), Some(identity)) = (
            number(row, "pid"),
            number(row, "ppid"),
            row["exe"].as_str(),
            row["identity"].as_str(),
        ) else {
            return Err(context());
        };
        items.push(Process {
            pid,
            ppid,
            uid: 0,
            exe: exe.to_owned(),
            identity: identity.to_owned(),
            args: arguments(row["args"].as_str().unwrap_or_default()),
            session: number(row, "session"),
            ..Process::default()
        });
    }
    let mut lineage = HashMap::new();
    for entry in data["tree"]
        .as_str()
        .unwrap_or_default()
        .split(';')
        .take(LINEAGE_LIMIT)
    {
        let parts: Vec<&str> = entry.split(',').collect();
        if parts.len() == 3
            && parts
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        {
            let (Ok(pid), Ok(parent), Ok(born)) = (
                parts[0].parse::<u32>(),
                parts[1].parse::<u32>(),
                parts[2].parse::<u64>(),
            ) else {
                continue;
            };
            lineage.insert(pid, (parent, (born != 0).then_some(born)));
        }
    }
    Ok(ProcessTable {
        items,
        lineage: Some(lineage),
    })
}

/// Python's Windows `live_contexts`: still running, at the same creation time.
pub(super) fn live(items: &[Process]) -> Vec<Process> {
    items
        .iter()
        .filter(|item| {
            let Some(process) = Handle::open(PROCESS_QUERY_LIMITED_INFORMATION, item.pid) else {
                return false;
            };
            let mut status = 0_u32;
            // SAFETY: status is a valid out pointer.
            let running = unsafe { GetExitCodeProcess(process.0, &mut status) } != 0
                && status == STILL_ACTIVE;
            running && creation(&process).as_deref() == Some(item.identity.as_str())
        })
        .cloned()
        .collect()
}

/// Force-stop one process after re-checking its identity (Python `terminate_cli` survivors).
pub(super) fn terminate(item: &Process) -> Result<(), UpdateFailure> {
    let failed = || UpdateFailure::new("restart_failed");
    let process = Handle::open(
        PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
        item.pid,
    )
    .ok_or_else(failed)?;
    if creation(&process).as_deref() != Some(item.identity.as_str()) {
        return Err(failed());
    }
    // SAFETY: a valid process handle opened with PROCESS_TERMINATE.
    if unsafe { TerminateProcess(process.0, 0) } == 0 {
        return Err(failed());
    }
    Ok(())
}

/// This process's desktop session id.
pub(super) fn own_session() -> Option<u32> {
    let mut session = 0_u32;
    // SAFETY: session is a valid out pointer.
    (unsafe { ProcessIdToSessionId(std::process::id(), &mut session) } != 0).then_some(session)
}

/// Python `windows_context(pid)`: (cwd, environment) read from the process parameters.
pub(super) fn context(pid: u32) -> Result<(String, Vec<(String, String)>), UpdateFailure> {
    use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
    let failed = || UpdateFailure::new("restart_context");
    let process =
        Handle::open(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, pid).ok_or_else(failed)?;
    let mut wow64 = 0;
    // SAFETY: wow64 is a valid out pointer. The supported CLIs are native x64; never guess a WOW64 PEB.
    if unsafe { IsWow64Process(process.0, &mut wow64) } == 0 || wow64 != 0 {
        return Err(failed());
    }
    let mut info = [0_usize; 6];
    let mut returned = 0_u32;
    // SAFETY: info is a PROCESS_BASIC_INFORMATION-sized buffer (six pointer-width fields).
    let status = unsafe {
        NtQueryInformationProcess(
            process.0,
            ProcessBasicInformation,
            info.as_mut_ptr().cast(),
            std::mem::size_of_val(&info) as u32,
            &mut returned,
        )
    };
    if status != 0 || info[1] == 0 {
        return Err(failed());
    }
    let read = |address: usize, size: usize| -> Result<Vec<u8>, UpdateFailure> {
        let mut buffer = vec![0_u8; size];
        let mut count = 0_usize;
        // SAFETY: buffer has `size` writable bytes; the address is the other process's.
        let ok = unsafe {
            ReadProcessMemory(
                process.0,
                address as *const _,
                buffer.as_mut_ptr().cast(),
                size,
                &mut count,
            )
        };
        if ok == 0 || count != size {
            return Err(failed());
        }
        Ok(buffer)
    };
    let word =
        |bytes: Vec<u8>| -> usize { usize::from_le_bytes(bytes[..8].try_into().unwrap_or([0; 8])) };
    let params = word(read(info[1] + 0x20, 8)?);
    let current = read(params + 0x38, 16)?;
    let length = usize::from(u16::from_le_bytes([current[0], current[1]]));
    let pointer = usize::from_le_bytes(current[8..16].try_into().unwrap_or([0; 8]));
    if length > 32_768 || pointer == 0 {
        return Err(failed());
    }
    let cwd = utf16(&read(pointer, length)?);
    let env_pointer = word(read(params + 0x80, 8)?);
    let mut data: Vec<u8> = Vec::new();
    for offset in (0..1024 * 1024).step_by(256) {
        data.extend(read(env_pointer + offset, 256)?);
        let from = data.len().saturating_sub(258);
        let end = (from..data.len().saturating_sub(3))
            .step_by(2)
            .find(|index| data[*index..*index + 4] == [0, 0, 0, 0]);
        if let Some(end) = end {
            let text = utf16(&data[..end]);
            let values = text
                .split('\0')
                .filter(|value| !value.starts_with('='))
                .filter_map(|value| {
                    value
                        .split_once('=')
                        .map(|(key, content)| (key.to_owned(), content.to_owned()))
                })
                .collect();
            return Ok((cwd, values));
        }
    }
    Err(failed())
}

fn utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Python `terminate_desktops`' Windows close: WM_CLOSE to every top-level window of these pids.
pub(super) fn close_windows(pids: &HashSet<u32>) {
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM},
        UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_CLOSE},
    };
    unsafe extern "system" fn visit(window: HWND, data: LPARAM) -> i32 {
        // SAFETY: data is the &HashSet passed to EnumWindows below, alive for the whole call.
        let pids = unsafe { &*(data as *const HashSet<u32>) };
        let mut pid = 0_u32;
        // SAFETY: window comes from EnumWindows; pid is a valid out pointer.
        unsafe { GetWindowThreadProcessId(window, &mut pid) };
        if pids.contains(&pid) {
            // SAFETY: posting a message has no memory effects here.
            unsafe { PostMessageW(window, WM_CLOSE, 0, 0) };
        }
        1
    }
    // SAFETY: the callback only reads the set through the pointer during this call.
    unsafe { EnumWindows(Some(visit), std::ptr::from_ref(pids) as LPARAM) };
}

static CONSOLE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Python `terminate_cli`'s graceful Windows step: CTRL_BREAK to each selected
/// process group, through its own console; never console-wide CTRL_C.
pub(super) fn ctrl_break(pids: &[u32]) {
    use windows_sys::Win32::System::Console::{
        AttachConsole, CTRL_BREAK_EVENT, FreeConsole, GenerateConsoleCtrlEvent,
        SetConsoleCtrlHandler,
    };
    let _guard = CONSOLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // SAFETY: console attach/detach calls with plain integer arguments.
    unsafe {
        FreeConsole();
        for pid in pids {
            if AttachConsole(*pid) != 0 {
                SetConsoleCtrlHandler(None, 1);
                GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, *pid);
                FreeConsole();
            }
        }
    }
}

/// Python `PrivatePipe`: a one-shot local named pipe only this user's SID may open.
pub(super) struct PrivatePipe(HANDLE);

// SAFETY: the pipe handle is used by one thread at a time (moved into the sender).
unsafe impl Send for PrivatePipe {}

impl PrivatePipe {
    pub(super) fn create(endpoint: &str) -> Option<Self> {
        use windows_sys::Win32::{
            Foundation::{INVALID_HANDLE_VALUE, LocalFree},
            Security::{
                Authorization::{
                    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                },
                PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
            },
            System::Pipes::{CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS},
        };
        // SAFETY: GetCurrentProcess returns a pseudo handle that needs no closing.
        let mine = token_user(unsafe { GetCurrentProcess() })?;
        let mut text: windows_sys::core::PWSTR = std::ptr::null_mut();
        // SAFETY: the buffer holds TOKEN_USER; text receives a LocalAlloc'd string freed below.
        let converted = unsafe {
            let user = &*mine.as_ptr().cast::<TOKEN_USER>();
            ConvertSidToStringSidW(user.User.Sid, &mut text)
        };
        if converted == 0 || text.is_null() {
            return None;
        }
        // SAFETY: text is a NUL-terminated wide string from ConvertSidToStringSidW.
        let sid = unsafe {
            let mut length = 0;
            while *text.add(length) != 0 {
                length += 1;
            }
            let value = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
            LocalFree(text.cast());
            value
        };
        let descriptor: Vec<u16> = format!("D:P(A;;GA;;;{sid})")
            .encode_utf16()
            .chain([0])
            .collect();
        let mut security: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: descriptor is NUL-terminated; security receives a LocalAlloc'd descriptor.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                descriptor.as_ptr(),
                1,
                &mut security,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return None;
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security,
            bInheritHandle: 0,
        };
        let name: Vec<u16> = endpoint.encode_utf16().chain([0]).collect();
        // One writer, byte mode, first instance only (FILE_FLAG_FIRST_PIPE_INSTANCE), no remote clients.
        // SAFETY: name is NUL-terminated and attributes outlive the call; security is freed after.
        let handle = unsafe {
            let handle = CreateNamedPipeW(
                name.as_ptr(),
                2 | 0x0008_0000,
                PIPE_REJECT_REMOTE_CLIENTS,
                1,
                2 * 1024 * 1024,
                0,
                25_000,
                &attributes,
            );
            LocalFree(security);
            handle
        };
        (handle != INVALID_HANDLE_VALUE).then_some(Self(handle))
    }

    /// Wait for the child, hand it the packet, and close.
    pub(super) fn send(self, data: &[u8]) -> bool {
        use windows_sys::Win32::{
            Storage::FileSystem::{FlushFileBuffers, WriteFile},
            System::Pipes::{ConnectNamedPipe, DisconnectNamedPipe},
        };
        const ERROR_PIPE_CONNECTED: i32 = 535;
        // SAFETY: a valid pipe handle; data and sent outlive the calls.
        unsafe {
            if ConnectNamedPipe(self.0, std::ptr::null_mut()) == 0
                && std::io::Error::last_os_error().raw_os_error() != Some(ERROR_PIPE_CONNECTED)
            {
                return false;
            }
            let mut sent = 0_u32;
            let Ok(length) = u32::try_from(data.len()) else {
                return false;
            };
            if WriteFile(
                self.0,
                data.as_ptr(),
                length,
                &mut sent,
                std::ptr::null_mut(),
            ) == 0
                || sent != length
            {
                return false;
            }
            FlushFileBuffers(self.0);
            DisconnectNamedPipe(self.0);
        }
        true
    }
}

impl Drop for PrivatePipe {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateNamedPipeW and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// Python `app_update_pipe.receive`: read the packet up to its newline.
pub(super) fn receive_pipe(endpoint: &str, prefix: &str) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let nonce = endpoint.strip_prefix(prefix)?;
    if nonce.len() != 32
        || !nonce
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let mut pipe = std::fs::File::open(endpoint).ok()?;
    let mut data = Vec::new();
    let mut chunk = vec![0_u8; 65_536];
    loop {
        let read = pipe.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        data.extend_from_slice(&chunk[..read]);
        if data.len() > 2 * 1024 * 1024 {
            return None;
        }
        if data.ends_with(b"\n") {
            break;
        }
    }
    Some(data)
}

/// Python `subprocess.list2cmdline`.
pub(super) fn list2cmdline(argv: &[String]) -> String {
    let mut line = String::new();
    for (index, arg) in argv.iter().enumerate() {
        if index > 0 {
            line.push(' ');
        }
        let quote = arg.is_empty() || arg.contains([' ', '\t']);
        if quote {
            line.push('"');
        }
        let mut slashes = 0;
        for ch in arg.chars() {
            match ch {
                '\\' => slashes += 1,
                '"' => {
                    line.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                    line.push('"');
                    slashes = 0;
                }
                _ => {
                    line.extend(std::iter::repeat_n('\\', slashes));
                    slashes = 0;
                    line.push(ch);
                }
            }
        }
        line.extend(std::iter::repeat_n(
            '\\',
            if quote { slashes * 2 } else { slashes },
        ));
        if quote {
            line.push('"');
        }
    }
    line
}
