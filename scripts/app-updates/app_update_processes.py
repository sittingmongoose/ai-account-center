"""Exact app-family process inventory and desktop restart contexts.

Arguments and environments stay in memory. Interactive CLI prompts are never
replayed. Only same-user executables belonging to the detected installation
qualify; a generic node/python/process-name match is never a control target.
"""

import ctypes
import dataclasses
import json
import os
import pathlib
import subprocess
import time

from app_update_common import UpdateFailure, command, environment, powershell


@dataclasses.dataclass
class Process:
    pid: int
    ppid: int
    uid: int
    exe: str
    identity: str
    args: list = dataclasses.field(default_factory=list, repr=False)
    cwd: str = dataclasses.field(default=None, repr=False)
    env: dict = dataclasses.field(default_factory=dict, repr=False)
    session: int = None


def linux_processes():
    values = []
    for path in pathlib.Path("/proc").iterdir():
        if not path.name.isdigit():
            continue
        try:
            if path.stat().st_uid != os.getuid():
                continue
            stat = (path / "stat").read_text()
            parts = stat[stat.rfind(")") + 2:].split()
            if parts[0] in ("Z", "X"):
                continue
            exe = os.readlink(path / "exe").removesuffix(" (deleted)")
            raw = (path / "cmdline").read_bytes()
            if len(raw) > 65536:
                continue
            args = [item.decode("utf-8", "surrogateescape") for item in raw.split(b"\0") if item]
            values.append(Process(int(path.name), int(parts[1]), os.getuid(), exe, parts[19], args))
        except (OSError, ValueError, IndexError):
            continue
    return values


def mac_context(pid):
    libc = ctypes.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
    mib = (ctypes.c_int * 3)(1, 49, pid)  # CTL_KERN, KERN_PROCARGS2, pid.
    size = ctypes.c_size_t(0)
    if libc.sysctl(mib, 3, None, ctypes.byref(size), None, 0) != 0 or size.value > 1024 * 1024:
        return [], {}
    buffer = ctypes.create_string_buffer(size.value)
    if libc.sysctl(mib, 3, buffer, ctypes.byref(size), None, 0) != 0:
        return [], {}
    argc = ctypes.c_int.from_buffer(buffer).value
    if not 0 < argc <= 4096:
        return [], {}
    data = buffer.raw[:size.value]
    start = data.find(b"\0", ctypes.sizeof(ctypes.c_int)) + 1
    while start < len(data) and data[start] == 0:
        start += 1
    arguments = []
    for _ in range(argc):
        end = data.find(b"\0", start)
        if end < 0:
            break
        arguments.append(data[start:end].decode("utf-8", "surrogateescape"))
        start = end + 1
    values = {}
    for value in data[start:].split(b"\0"):
        if b"=" in value:
            key, content = value.decode("utf-8", "surrogateescape").split("=", 1)
            values[key] = content
    return arguments, values


def mac_arguments(pid):
    return mac_context(pid)[0]


def mac_processes():
    output = command(["/bin/ps", "-A", "-o", "pid=,ppid=,uid=,lstart="], timeout=10, capture=True)
    libproc = ctypes.CDLL("/usr/lib/libproc.dylib")
    libproc.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
    values = []
    for line in output.splitlines():
        parts = line.split(None, 3)
        if len(parts) != 4:
            continue
        try:
            pid, ppid, uid = map(int, parts[:3])
        except ValueError:
            continue
        if uid != os.getuid():
            continue
        path = ctypes.create_string_buffer(4096)
        if libproc.proc_pidpath(pid, path, len(path)) <= 0:
            continue
        exe = path.value.decode("utf-8", "surrogateescape")
        values.append(Process(pid, ppid, uid, exe, parts[3]))
    return values


def windows_processes():
    # CIM returns arguments privately to this local helper, never to CCS.
    script = """$ErrorActionPreference='Stop';
$me=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value;
$rows=@(); foreach($p in Get-CimInstance Win32_Process){
 if($p.Name -notin @('ChatGPT.exe','Claude.exe','codex.exe','claude.exe','agy.exe','omp.exe','node.exe','muse.exe','muse-bin.exe')){continue};
 $o=Invoke-CimMethod -InputObject $p -MethodName GetOwnerSid -ErrorAction SilentlyContinue;
 if($o.Sid -ne $me -or -not $p.ExecutablePath){continue};
 try{$started=[Diagnostics.Process]::GetProcessById($p.ProcessId).StartTime.ToFileTimeUtc().ToString()}catch{continue};
 $rows+=@{pid=[int]$p.ProcessId;ppid=[int]$p.ParentProcessId;exe=$p.ExecutablePath;args=$p.CommandLine;identity=$started;session=[int]$p.SessionId}
}; ConvertTo-Json -InputObject @($rows) -Compress -Depth 3"""
    try:
        # This host-private process inventory can exceed a DTO's 64 KiB cap.
        # It stays in memory and is never emitted; final result stdout remains
        # under 64 KiB and excludes every argv/environment field.
        rows = json.loads(powershell(script, timeout=20, capture_limit=2 * 1024 * 1024))
        rows = rows if isinstance(rows, list) else [rows]
        values = []
        for row in rows[:2048]:
            args = windows_arguments(row.get("args") or "")
            values.append(Process(int(row["pid"]), int(row["ppid"]), 0, row["exe"], row["identity"], args, session=int(row["session"])))
        return values
    except (ValueError, KeyError, TypeError):
        raise UpdateFailure("restart_context") from None


def windows_arguments(value):
    if not value or len(value) > 65536:
        return []
    shell = ctypes.windll.shell32
    shell.CommandLineToArgvW.argtypes = [ctypes.c_wchar_p, ctypes.POINTER(ctypes.c_int)]
    shell.CommandLineToArgvW.restype = ctypes.POINTER(ctypes.c_wchar_p)
    count = ctypes.c_int()
    pointers = shell.CommandLineToArgvW(value, ctypes.byref(count))
    if not pointers:
        return []
    try:
        return [pointers[index] for index in range(count.value)]
    finally:
        ctypes.windll.kernel32.LocalFree(pointers)


def scan(platform):
    return windows_processes() if platform == "windows" else mac_processes() if platform == "mac" else linux_processes()


def is_under(exe, root):
    try:
        return os.path.commonpath([os.path.normcase(os.path.realpath(exe)), os.path.normcase(os.path.realpath(root))]) == os.path.normcase(os.path.realpath(root))
    except (ValueError, OSError):
        return False


def family(install, processes):
    active = os.path.normcase(os.path.realpath(install.path))
    root = install.package_root
    values = []
    for process in processes:
        exact = os.path.normcase(os.path.realpath(process.exe)) == active
        owned_package = root is not None and is_under(process.exe, root)
        node_module = install.manager == "npm" and root is not None and len(process.args) >= 2 and is_under(process.args[1], root)
        muse_binary = install.app_id == "muse-code" and pathlib.Path(process.exe).name.startswith("muse-bin-") and pathlib.Path(process.exe).parent == install.path.parent
        if exact or owned_package or node_module or muse_binary:
            values.append(process)
    # Some signed Codex desktop children live in its per-user staged runtime,
    # outside the MSIX root. Include only that fixed path AND desktop ancestry.
    if install.platform == "windows" and install.app_id == "codex-desktop":
        runtime = pathlib.Path(os.environ.get("LOCALAPPDATA", str(pathlib.Path.home() / "AppData/Local"))) / "OpenAI/Codex/bin"
        pids = {item.pid for item in values}
        changed = True
        while changed:
            changed = False
            for process in processes:
                if process.pid not in pids and process.ppid in pids and is_under(process.exe, runtime) and pathlib.Path(process.exe).name.lower() == "codex.exe":
                    values.append(process)
                    pids.add(process.pid)
                    changed = True
    return values


def main_contexts(install, processes):
    selected = family(install, processes)
    pids = {item.pid for item in selected}
    mains = [item for item in selected if item.ppid not in pids]
    if selected and not mains:
        raise UpdateFailure("restart_context")
    for item in mains:
        if install.platform == "mac":
            item.args = mac_arguments(item.pid)
        if not item.args:
            raise UpdateFailure("restart_context")
        if install.platform == "ubuntu":
            try:
                raw = pathlib.Path("/proc", str(item.pid), "environ").read_bytes()
                if len(raw) > 1024 * 1024:
                    raise UpdateFailure("restart_context")
                item.env = dict(value.decode("utf-8", "surrogateescape").split("=", 1) for value in raw.split(b"\0") if b"=" in value)
                item.cwd = os.readlink("/proc/" + str(item.pid) + "/cwd")
                if not item.env.get("DISPLAY") and not item.env.get("WAYLAND_DISPLAY"):
                    raise UpdateFailure("restart_context")
            except OSError:
                raise UpdateFailure("restart_context") from None
    return mains


def windows_context(pid):
    """Read this user's process parameters locally; never persist its environment."""
    from ctypes import wintypes
    kernel = ctypes.windll.kernel32
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.ReadProcessMemory.argtypes = [wintypes.HANDLE, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t)]
    kernel.ReadProcessMemory.restype = wintypes.BOOL
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel.IsWow64Process.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.BOOL)]
    native_query = ctypes.windll.ntdll.NtQueryInformationProcess
    native_query.argtypes = [wintypes.HANDLE, wintypes.ULONG, ctypes.c_void_p, wintypes.ULONG, ctypes.POINTER(wintypes.ULONG)]
    native_query.restype = ctypes.c_long
    handle = kernel.OpenProcess(0x0410, False, pid)
    if not handle:
        raise UpdateFailure("restart_context")
    try:
        info = (ctypes.c_void_p * 6)()
        returned = wintypes.ULONG()
        status = native_query(handle, 0, ctypes.byref(info), ctypes.sizeof(info), ctypes.byref(returned))
        if status != 0 or not info[1]:
            raise UpdateFailure("restart_context")
        def read(address, size):
            buffer = ctypes.create_string_buffer(size)
            count = ctypes.c_size_t()
            if not kernel.ReadProcessMemory(handle, ctypes.c_void_p(address), buffer, size, ctypes.byref(count)) or count.value != size:
                raise UpdateFailure("restart_context")
            return buffer.raw
        width = ctypes.sizeof(ctypes.c_void_p)
        # The supported installed CLIs are native x64; never guess a WOW64 PEB.
        wow64 = wintypes.BOOL()
        if not kernel.IsWow64Process(handle, ctypes.byref(wow64)) or wow64.value:
            raise UpdateFailure("restart_context")
        params = int.from_bytes(read(info[1] + 0x20, width), "little")
        current = read(params + 0x38, 16)
        length, pointer = int.from_bytes(current[:2], "little"), int.from_bytes(current[8:16], "little")
        if length > 32768 or not pointer:
            raise UpdateFailure("restart_context")
        cwd = read(pointer, length).decode("utf-16le")
        env_pointer = int.from_bytes(read(params + 0x80, width), "little")
        values, data = {}, bytearray()
        # Read bounded chunks so an inaccessible later heap page cannot erase
        # an already complete environment block. No arbitrary process memory is scanned.
        for offset in range(0, 1024 * 1024, 256):
            data.extend(read(env_pointer + offset, 256))
            end = next((index for index in range(max(0, len(data) - 258), len(data) - 3, 2) if data[index:index + 4] == b"\0\0\0\0"), None)
            if end is not None:
                text = data[:end].decode("utf-16le")
                for value in text.split("\0"):
                    if "=" in value and not value.startswith("="):
                        key, content = value.split("=", 1)
                        values[key] = content
                return cwd, values
        raise UpdateFailure("restart_context")
    finally:
        kernel.CloseHandle(handle)


def cli_contexts(install, processes):
    selected = family(install, processes)
    pids = {item.pid for item in selected}
    mains = [item for item in selected if item.ppid not in pids]
    for item in mains:
        if install.platform == "ubuntu":
            try:
                item.cwd = os.readlink("/proc/" + str(item.pid) + "/cwd")
                raw = pathlib.Path("/proc", str(item.pid), "environ").read_bytes()
                if len(raw) > 1024 * 1024:
                    raise UpdateFailure("restart_context")
                item.env = dict(value.decode("utf-8", "surrogateescape").split("=", 1) for value in raw.split(b"\0") if b"=" in value)
            except OSError:
                raise UpdateFailure("restart_context") from None
        elif install.platform == "mac":
            item.args, item.env = mac_context(item.pid)
            output = command(["/usr/sbin/lsof", "-a", "-p", str(item.pid), "-d", "cwd", "-Fn"], timeout=10, capture=True)
            item.cwd = next((line[1:] for line in output.splitlines() if line.startswith("n/")), None)
        else:
            item.cwd, item.env = windows_context(item.pid)
        if not item.cwd or not pathlib.Path(item.cwd).is_dir():
            raise UpdateFailure("restart_context")
    return mains, selected


def terminate_cli(install, targets, grace=5):
    live = live_contexts(install.platform, targets)
    if install.platform == "windows":
        # CTRL_BREAK is attempted only for each selected process group; unlike
        # console-wide CTRL_C it cannot target the parent PowerShell/Terminal.
        kernel = ctypes.windll.kernel32
        kernel.FreeConsole()
        for item in live:
            if kernel.AttachConsole(item.pid):
                kernel.SetConsoleCtrlHandler(None, True)
                kernel.GenerateConsoleCtrlEvent(1, item.pid)
                kernel.FreeConsole()
    else:
        for item in live:
            safe_signal(item, 15)
    deadline = time.monotonic() + grace
    while live_contexts(install.platform, targets) and time.monotonic() < deadline:
        time.sleep(.1)
    survivors = live_contexts(install.platform, targets)
    for item in survivors:
        if install.platform == "windows":
            from ctypes import wintypes
            kernel = ctypes.windll.kernel32
            kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
            kernel.OpenProcess.restype = wintypes.HANDLE
            kernel.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]
            kernel.CloseHandle.argtypes = [wintypes.HANDLE]
            handle = kernel.OpenProcess(0x1001, False, item.pid)
            if not handle:
                raise UpdateFailure("restart_failed")
            try:
                class FILETIME(ctypes.Structure):
                    _fields_ = [("low", wintypes.DWORD), ("high", wintypes.DWORD)]
                created, exited, system, user = FILETIME(), FILETIME(), FILETIME(), FILETIME()
                kernel.GetProcessTimes.argtypes = [wintypes.HANDLE, ctypes.POINTER(FILETIME), ctypes.POINTER(FILETIME), ctypes.POINTER(FILETIME), ctypes.POINTER(FILETIME)]
                if not kernel.GetProcessTimes(handle, ctypes.byref(created), ctypes.byref(exited), ctypes.byref(system), ctypes.byref(user)) or str((created.high << 32) | created.low) != item.identity:
                    raise UpdateFailure("restart_failed")
                if not ctypes.windll.kernel32.TerminateProcess(handle, 0):
                    raise UpdateFailure("restart_failed")
            finally:
                ctypes.windll.kernel32.CloseHandle(handle)
        else:
            safe_signal(item, 9)
    deadline = time.monotonic() + 5
    while live_contexts(install.platform, targets):
        if time.monotonic() >= deadline:
            raise UpdateFailure("restart_failed")
        time.sleep(.1)
    return len(survivors)


def same_process(left, right):
    return left.pid == right.pid and left.identity == right.identity and left.exe == right.exe


def live_contexts(platform, contexts):
    if platform == "windows":
        from ctypes import wintypes
        kernel = ctypes.windll.kernel32
        kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        kernel.OpenProcess.restype = wintypes.HANDLE
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        kernel.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
        class FILETIME(ctypes.Structure):
            _fields_ = [("low", wintypes.DWORD), ("high", wintypes.DWORD)]
        kernel.GetProcessTimes.argtypes = [wintypes.HANDLE, ctypes.POINTER(FILETIME), ctypes.POINTER(FILETIME), ctypes.POINTER(FILETIME), ctypes.POINTER(FILETIME)]
        remaining = []
        for item in contexts:
            handle = kernel.OpenProcess(0x1000, False, item.pid)
            if not handle:
                continue
            try:
                created, exited, system, user = FILETIME(), FILETIME(), FILETIME(), FILETIME()
                status = wintypes.DWORD()
                if kernel.GetExitCodeProcess(handle, ctypes.byref(status)) and status.value == 259 and kernel.GetProcessTimes(handle, ctypes.byref(created), ctypes.byref(exited), ctypes.byref(system), ctypes.byref(user)) and str((created.high << 32) | created.low) == item.identity:
                    remaining.append(item)
            finally:
                kernel.CloseHandle(handle)
        return remaining
    current = scan(platform)
    return [item for item in contexts if any(same_process(item, other) for other in current)]


def safe_signal(item, value):
    """Pin a Linux process handle so PID reuse cannot target another app."""
    import signal
    descriptor = None
    try:
        if sys_platform_linux() and hasattr(os, "pidfd_open") and hasattr(signal, "pidfd_send_signal"):
            descriptor = os.pidfd_open(item.pid)
            if not any(same_process(item, current) for current in linux_processes()):
                return
            signal.pidfd_send_signal(descriptor, value)
        elif any(same_process(item, current) for current in scan("mac" if not sys_platform_linux() else "ubuntu")):
            os.kill(item.pid, value)
    except ProcessLookupError:
        pass
    finally:
        if descriptor is not None:
            os.close(descriptor)


def sys_platform_linux():
    import sys
    return sys.platform.startswith("linux")


def desktop_arguments(context, platform):
    """Only existing data-directory flags are reapplied; never replay work arguments."""
    args = context.args[1:]
    found = []
    for index, arg in enumerate(args):
        if arg.startswith("--user-data-dir="):
            value = arg[len("--user-data-dir="):]
            if value and pathlib.Path(value).is_absolute():
                found = ["--user-data-dir=" + value]
        elif arg == "--user-data-dir" and index + 1 < len(args):
            value = args[index + 1]
            if pathlib.Path(value).is_absolute():
                found = ["--user-data-dir=" + value]
    # Chromium on Linux may rewrite argv0 to just its executable. Its existing
    # data directory is still supplied by the preserved process environment.
    return found


def terminate_desktops(install, contexts, grace=15):
    """Ask the captured desktop instances to quit and wait, gracefully only.

    A running desktop app is never force-stopped: whatever is still alive after
    the bounded wait is returned so the caller can stage the update or report an
    actionable quit request. A refused quit is an outcome, not an error.
    """
    targets = family(install, scan(install.platform))
    if install.platform == "mac":
        ctypes.CDLL("/System/Library/Frameworks/AppKit.framework/AppKit")
        objc = ctypes.CDLL("/usr/lib/libobjc.A.dylib")
        objc.objc_getClass.argtypes = [ctypes.c_char_p]
        objc.objc_getClass.restype = ctypes.c_void_p
        objc.sel_registerName.argtypes = [ctypes.c_char_p]
        objc.sel_registerName.restype = ctypes.c_void_p
        lookup = ctypes.CFUNCTYPE(ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_int)(("objc_msgSend", objc))
        terminate = ctypes.CFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)(("objc_msgSend", objc))
        for item in live_contexts(install.platform, contexts):
            instance = lookup(objc.objc_getClass(b"NSRunningApplication"), objc.sel_registerName(b"runningApplicationWithProcessIdentifier:"), item.pid)
            if instance:
                # A refusal leaves the item in the returned live set; the caller
                # stages the update instead of touching the running app.
                terminate(instance, objc.sel_registerName(b"terminate"))
    elif install.platform == "windows":
        session = ctypes.c_ulong()
        ctypes.windll.kernel32.ProcessIdToSessionId(os.getpid(), ctypes.byref(session))
        user32 = ctypes.windll.user32
        # Cross-session instances cannot receive this session's WM_CLOSE; they
        # stay in the returned live set for the staged-update path.
        live = {item.pid for item in live_contexts(install.platform, contexts) if item.session == session.value}
        callback_type = ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)
        def close_window(hwnd, _):
            pid = ctypes.c_ulong()
            user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
            if pid.value in live:
                user32.PostMessageW(hwnd, 0x0010, 0, 0)
            return True
        callback = callback_type(close_window)
        user32.EnumWindows(callback, None)
    else:
        for item in live_contexts(install.platform, contexts):
            safe_signal(item, 15)
    deadline = time.monotonic() + grace
    while live_contexts(install.platform, targets) and time.monotonic() < deadline:
        time.sleep(.2)
    return live_contexts(install.platform, targets)


def restart_desktops(install, contexts):
    restarted = 0
    for item in contexts:
        args = desktop_arguments(item, install.platform)
        if install.platform == "mac":
            command(["/usr/bin/open", "-n", "-a", install.path, "--args", *args], timeout=15)
        elif install.platform == "windows":
            subprocess.Popen([str(install.path), *args], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                             creationflags=subprocess.DETACHED_PROCESS | subprocess.CREATE_NEW_PROCESS_GROUP)
        else:
            subprocess.Popen([str(install.path), *args], cwd=item.cwd, env=environment(item.env), stdin=subprocess.DEVNULL,
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
        restarted += 1
    if contexts:
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            existing = family(install, scan(install.platform))
            pids = {item.pid for item in existing}
            if len([item for item in existing if item.ppid not in pids]) >= len(contexts):
                return restarted
            time.sleep(.3)
        raise UpdateFailure("restart_failed")
    return restarted
