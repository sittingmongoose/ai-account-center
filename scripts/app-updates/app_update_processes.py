"""Exact app-family process inventory and desktop restart contexts.

Arguments and environments stay in memory. Interactive CLI prompts are never
replayed. Only same-user executables belonging to the detected installation
qualify; a generic node/python/process-name match is never a control target.
"""

import collections
import ctypes
import dataclasses
import json
import os
import pathlib
import re
import subprocess
import sys
import time

from app_update_common import UpdateFailure, command, environment, powershell

# T3's own server and desktop, by exact location. A CLI process started below
# one of them belongs to a T3 session: Update all never stops, relaunches or
# judges it. The standalone runtime is <versions>/<version>/t3 (Ubuntu, Nas1,
# the Mac); the desktop apps run their server inside the app itself.
T3_RUNTIME_VERSIONS = ".t3/runtime/versions"
T3_MAC_BUNDLE = pathlib.Path("/Applications/T3 Code (Nightly).app")
T3_WINDOWS_FOLDER = "Programs/t3code"

# A terminal device on Linux (a pts or a virtual console) and on the Mac (ttys*).
TTY_DEVICE = re.compile(r"/dev/(?:pts/[0-9]+|tty[A-Za-z0-9]*|console)")
# Windows keeps the whole process table, at most this many entries, for ancestry only.
LINEAGE_LIMIT = 16384


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


class ProcessTable(list):
    """Scanned processes; on Windows also `lineage`, every process's (parent PID, start time).

    The Windows scan lists only app-relevant processes in full. Its lineage covers
    every process, whatever its name, so T3 stays a visible ancestor however many
    other processes (a shell, Git Bash) sit between it and a CLI.
    """
    lineage = None


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
    # Anything running from the npm global tree counts whatever its name (a Muse native
    # helper, zcode-acp-martty's martty.exe): npm cannot replace those files either.
    # `tree` is every process as "pid,parent,start;": no name, path or arguments. It
    # keeps T3 a visible ancestor through any unlisted process (see ancestors).
    script = """$ErrorActionPreference='Stop';
$me=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value;
$npm=[IO.Path]::Combine($env:APPDATA,'npm','node_modules');
$rows=@(); $lineage=New-Object Text.StringBuilder; foreach($p in Get-CimInstance Win32_Process){
 $born=0; if($p.CreationDate){$born=$p.CreationDate.ToFileTimeUtc()};
 [void]$lineage.Append([string]$p.ProcessId+','+[string]$p.ParentProcessId+','+[string]$born+';');
 $tree=[bool]($p.ExecutablePath -and $p.ExecutablePath.StartsWith($npm,[StringComparison]::OrdinalIgnoreCase));
 if($p.Name -notin @('ChatGPT.exe','Claude.exe','codex.exe','claude.exe','agy.exe','omp.exe','node.exe','muse.exe','muse-bin.exe','T3 Code (Nightly).exe','t3-resource-monitor.exe','cursorsandbox.exe','rg.exe','OpenConsole.exe','elevate.exe','ZCode.exe','muse-acp.exe') -and -not $tree){continue};
 $o=Invoke-CimMethod -InputObject $p -MethodName GetOwnerSid -ErrorAction SilentlyContinue;
 if($o.Sid -ne $me -or -not $p.ExecutablePath){continue};
 try{$started=[Diagnostics.Process]::GetProcessById($p.ProcessId).StartTime.ToFileTimeUtc().ToString()}catch{continue};
 $rows+=@{pid=[int]$p.ProcessId;ppid=[int]$p.ParentProcessId;exe=$p.ExecutablePath;args=$p.CommandLine;identity=$started;session=[int]$p.SessionId}
}; ConvertTo-Json -InputObject @{rows=@($rows);tree=$lineage.ToString()} -Compress -Depth 4"""
    try:
        # This host-private process inventory can exceed a DTO's 64 KiB cap.
        # It stays in memory and is never emitted; final result stdout remains
        # under 64 KiB and excludes every argv/environment field.
        data = json.loads(powershell(script, timeout=20, capture_limit=2 * 1024 * 1024))
        rows = data["rows"] if isinstance(data["rows"], list) else [data["rows"]]
        values = ProcessTable()
        for row in rows[:2048]:
            args = windows_arguments(row.get("args") or "")
            values.append(Process(int(row["pid"]), int(row["ppid"]), 0, row["exe"], row["identity"], args, session=int(row["session"])))
        values.lineage = {}
        for entry in str(data.get("tree") or "").split(";")[:LINEAGE_LIMIT]:
            parts = entry.split(",")
            if len(parts) == 3 and all(part.isdigit() for part in parts):
                values.lineage[int(parts[0])] = (int(parts[1]), int(parts[2]) or None)
        return values
    except (ValueError, KeyError, TypeError, AttributeError):
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
        muse_binary = install.app_id == "muse-code" and os.path.normcase(pathlib.Path(process.exe).name).startswith("muse-bin-") and pathlib.Path(process.exe).parent == install.path.parent
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


def t3_servers(platform, processes):
    """PIDs running from T3's own server or desktop location on this computer."""
    home = pathlib.Path.home()
    if platform == "windows":
        local = pathlib.Path(os.environ.get("LOCALAPPDATA", str(home / "AppData/Local")))
        return {item.pid for item in processes if is_under(item.exe, local / T3_WINDOWS_FOLDER)}
    versions = os.path.realpath(home / T3_RUNTIME_VERSIONS)
    found = set()
    for item in processes:
        executable = pathlib.PurePath(os.path.realpath(item.exe))
        if ((executable.name == "t3" and str(executable.parent.parent) == versions) or
                (platform == "mac" and is_under(item.exe, T3_MAC_BUNDLE))):
            found.add(item.pid)
    return found


def ancestors(processes, item):
    """item's ancestors' PIDs, nearest first, from the scan's whole process table where it kept one.

    Windows reuses PIDs and never re-parents an orphan, so a parent PID whose
    process started after its child names an unrelated process: the chain ends
    there. A PID cycle ends it too, instead of looping.
    """
    table = getattr(processes, "lineage", None) or {row.pid: (row.ppid, None) for row in processes}
    chain, seen = [], {item.pid}
    current, started = item.ppid, table.get(item.pid, (None, None))[1]
    while current in table and current not in seen:
        parent, created = table[current]
        if started is not None and created is not None and created > started:
            break
        chain.append(current)
        seen.add(current)
        current, started = parent, created
    return chain


def t3_owned(install, processes):
    """App-family processes that T3 started: T3's server or desktop is an ancestor in this table.

    T3 owns their lifetime, so Update all never stops, relaunches or judges them.
    """
    selected = family(install, processes)
    servers = t3_servers(install.platform, processes) if selected else set()
    if not servers:
        return []
    return [item for item in selected if any(pid in servers for pid in ancestors(processes, item))]


def user_family(install, processes):
    """The app family without T3-owned processes: what a CLI relaunch is counted against."""
    owned = {item.pid for item in t3_owned(install, processes)}
    return [item for item in family(install, processes) if item.pid not in owned]


def vanished(platform, item):
    """True once item has exited or its PID names another process; False when unsure."""
    if platform != "ubuntu":
        return not live_contexts(platform, [item])
    try:
        stat = pathlib.Path("/proc", str(item.pid), "stat").read_text()
    except (FileNotFoundError, ProcessLookupError):
        return True
    except OSError:
        return False
    parts = stat[stat.rfind(")") + 2:].split()
    return len(parts) < 20 or parts[0] in ("Z", "X") or parts[19] != item.identity


def cli_context(platform, item):
    """Read one main process's cwd and environment (and Mac arguments) into item."""
    if platform == "ubuntu":
        item.cwd = os.readlink("/proc/" + str(item.pid) + "/cwd")
        raw = pathlib.Path("/proc", str(item.pid), "environ").read_bytes()
        if len(raw) > 1024 * 1024:
            raise UpdateFailure("restart_context")
        item.env = dict(value.decode("utf-8", "surrogateescape").split("=", 1) for value in raw.split(b"\0") if b"=" in value)
    elif platform == "mac":
        item.args, item.env = mac_context(item.pid)
        output = command(["/usr/sbin/lsof", "-a", "-p", str(item.pid), "-d", "cwd", "-Fn"], timeout=10, capture=True)
        item.cwd = next((line[1:] for line in output.splitlines() if line.startswith("n/")), None)
    else:
        item.cwd, item.env = windows_context(item.pid)


# Runs in its own short-lived interpreter: a process attached to another console
# receives that console's Ctrl+C and close events, which must never reach the helper.
# argv is "pid:start" pairs; it prints the PIDs, still at that start, whose console
# has a window the user sees: a classic console window, or the pseudo console window
# of a Windows Terminal tab (visible itself, owned by the terminal's window). A
# console made without a window (Node's windowsHide, CREATE_NO_WINDOW), a hidden one,
# none at all, or another session's pseudo console has none.
WINDOWS_CONSOLE_PROBE = """
import ctypes, json, sys
from ctypes import wintypes
kernel, user = ctypes.windll.kernel32, ctypes.windll.user32
kernel.GetConsoleWindow.restype = wintypes.HWND
kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
kernel.OpenProcess.restype = wintypes.HANDLE
kernel.CloseHandle.argtypes = [wintypes.HANDLE]
user.GetWindow.argtypes = [wintypes.HWND, wintypes.UINT]
user.GetWindow.restype = wintypes.HWND
user.IsWindowVisible.argtypes = [wintypes.HWND]
class FILETIME(ctypes.Structure):
    _fields_ = [('low', wintypes.DWORD), ('high', wintypes.DWORD)]
kernel.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(FILETIME)] * 4
def started(pid):
    handle = kernel.OpenProcess(0x1000, False, pid)
    if not handle:
        return None
    try:
        times = [FILETIME() for _ in range(4)]
        if not kernel.GetProcessTimes(handle, *[ctypes.byref(item) for item in times]):
            return None
        return str((times[0].high << 32) | times[0].low)
    finally:
        kernel.CloseHandle(handle)
kernel.SetConsoleCtrlHandler(None, True)
seen = []
for value in sys.argv[1:]:
    pid, identity = value.split(':', 1)
    pid = int(pid)
    kernel.FreeConsole()
    if not kernel.AttachConsole(pid):
        continue
    try:
        window = kernel.GetConsoleWindow()
        owner = user.GetWindow(window, 4) if window else None
        if window and (user.IsWindowVisible(window) or (owner and user.IsWindowVisible(owner))) and started(pid) == identity:
            seen.append(pid)
    finally:
        kernel.FreeConsole()
sys.stdout.write(json.dumps(seen))
"""


def terminal_attached(platform, items):
    """PIDs of items attached to a terminal the user has, so a relaunch can reopen them there.

    Ubuntu and the Mac: stdin and stdout are both a terminal device (a tmux pane,
    an SSH session, Terminal). Windows: its console has a visible window in this
    desktop session (a console window or a Windows Terminal tab). Anything else --
    pipes or sockets (T3, an Agent SDK or ACP harness, `claude -p` from a script),
    /dev/null (a service, cron), no visible console -- and anything that cannot be
    read is not attached: such a process is left running, never stopped.
    """
    if not items:
        return set()
    if platform == "windows":
        return windows_terminal_attached(items)
    return {item.pid for item in items if (mac_terminal_attached if platform == "mac" else linux_terminal_attached)(item)}


def linux_terminal_attached(item):
    try:
        stat = pathlib.Path("/proc", str(item.pid), "stat").read_text()
        if stat[stat.rfind(")") + 2:].split()[19] != item.identity:
            return False
        return all(TTY_DEVICE.fullmatch(os.readlink("/proc/%d/fd/%d" % (item.pid, fd))) for fd in (0, 1))
    except (OSError, IndexError):
        return False


def mac_terminal_attached(item):
    # One process per call: lsof fails the whole call when any listed PID has exited.
    try:
        output = command(["/usr/sbin/lsof", "-a", "-p", str(item.pid), "-d", "0,1", "-F", "n"], timeout=10, capture=True)
    except UpdateFailure:
        return False
    names, descriptor = {}, None
    for line in output.splitlines():
        if line.startswith("f"):
            descriptor = line[1:]
        elif line.startswith("n") and descriptor in ("0", "1"):
            names[descriptor] = line[1:]
    return all(TTY_DEVICE.fullmatch(names.get(fd, "")) for fd in ("0", "1"))


def windows_terminal_attached(items):
    # Window handles belong to one session: only this desktop session's processes are probed.
    session = ctypes.c_ulong()
    if not ctypes.windll.kernel32.ProcessIdToSessionId(os.getpid(), ctypes.byref(session)):
        return set()
    local = {item.pid: item for item in items if item.session == session.value}
    python = pathlib.Path(sys.executable).with_name("python.exe")
    if not local or not python.is_file():
        return set()
    try:
        found = json.loads(command([python, "-I", "-c", WINDOWS_CONSOLE_PROBE, *("%d:%s" % (pid, item.identity) for pid, item in local.items())],
                                   timeout=20, capture=True))
    except (UpdateFailure, ValueError):
        return set()
    return {pid for pid in found if type(pid) is int and pid in local} if isinstance(found, list) else set()


def descendants(roots, items):
    """roots plus every item below them through parent links among items."""
    found, grown = set(roots), bool(roots)
    while grown:
        grown = False
        for item in items:
            if item.pid not in found and item.ppid in found:
                found.add(item.pid)
                grown = True
    return found


CliInstances = collections.namedtuple("CliInstances", "contexts targets background")


def cli_instances(install, processes):
    """The user's terminal instances (main processes to relaunch, processes to stop) and the background sessions.

    Only a main process attached to a terminal (see terminal_attached) is a
    terminal instance: it is stopped and reopened idle in a new terminal. Every
    other family process is a background session -- T3's own, a `claude -p` from
    a script or cron, another harness's, a service's -- and has no terminal to
    come back to: it is never read, stopped or relaunched and never fails the
    readiness check. A terminal instance that exits between the scan and its
    context read is dropped with its descendants; one that still runs but cannot
    be read, or whose working directory is gone, fails closed.
    """
    selected = family(install, processes)
    owned = {item.pid for item in t3_owned(install, processes)}
    user = [item for item in selected if item.pid not in owned]
    pids = {item.pid for item in user}
    mains = [item for item in user if item.ppid not in pids]
    attached = terminal_attached(install.platform, mains)
    background = owned | descendants({item.pid for item in mains if item.pid not in attached}, user)
    contexts, gone = [], set()
    for item in (item for item in mains if item.pid in attached):
        try:
            cli_context(install.platform, item)
            readable = bool(item.cwd) and pathlib.Path(item.cwd).is_dir()
        except (UpdateFailure, OSError):
            readable = False
        if readable:
            contexts.append(item)
        elif vanished(install.platform, item):
            gone.add(item.pid)
        else:
            raise UpdateFailure("restart_context")
    gone = descendants(gone, user)
    return CliInstances(contexts, [item for item in user if item.pid not in gone and item.pid not in background],
                        [item for item in selected if item.pid in background])


def cli_contexts(install, processes):
    """(main processes to relaunch, processes to stop) for the user's own terminal CLI instances."""
    return tuple(cli_instances(install, processes)[:2])


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
            if not instance or not terminate(instance, objc.sel_registerName(b"terminate")):
                raise UpdateFailure("restart_context")
    elif install.platform == "windows":
        session = ctypes.c_ulong()
        ctypes.windll.kernel32.ProcessIdToSessionId(os.getpid(), ctypes.byref(session))
        if any(item.session != session.value for item in contexts):
            raise UpdateFailure("restart_context")
        user32 = ctypes.windll.user32
        live = {item.pid for item in live_contexts(install.platform, contexts)}
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
    # Quit first, then stop only the captured app family if its close operation
    # hides the window or leaves app-owned helpers running. Never its terminal.
    return terminate_cli(install, targets, grace=2)


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
