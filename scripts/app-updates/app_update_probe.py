#!/usr/bin/env python3
"""Read-only deployment probe; only booleans/counts cross stdout."""
import ctypes
import json
import pathlib
import sys
from app_update_common import UpdateFailure
from app_update_processes import cli_contexts, family, scan, windows_context
from app_updates import detect_cli


def main():
    platform = 'windows' if sys.platform == 'win32' else 'mac' if sys.platform == 'darwin' else 'ubuntu'
    proof = {'platform': platform}
    try:
        rows = scan(platform)
        install = detect_cli('omp', platform)
        proof.update(scanWorked=True, processCount=len(rows), ompInstalled=install is not None)
        if install is not None:
            targets = family(install, rows)
            pids = {item.pid for item in targets}
            mains = [item for item in targets if item.ppid not in pids]
            proof.update(mainCount=len(mains), familyCount=len(targets))
            if platform == 'windows':
                captured = []
                session = ctypes.c_ulong()
                ctypes.windll.kernel32.ProcessIdToSessionId(__import__('os').getpid(), ctypes.byref(session))
                for item in mains:
                    try:
                        cwd, env = windows_context(item.pid)
                        captured.append({'cwdVisibleInProbeSession': pathlib.Path(cwd).is_dir(), 'cwdIsDriveRoot': pathlib.Path(cwd).anchor == cwd, 'environmentKeyCount': len(env), 'sourceSession': item.session, 'probeSession': session.value})
                    except UpdateFailure:
                        captured.append({'captureWorked': False})
                proof['windowsParameters'] = captured
            try:
                contexts, _ = cli_contexts(install, rows)
                proof['contexts'] = [{'cwdReadable': pathlib.Path(item.cwd).is_dir(), 'environmentKeyCount': len(item.env)} for item in contexts]
                proof['contextsWorked'] = True
            except UpdateFailure:
                proof['contextsWorked'] = False
                if platform == 'windows':
                    proof['memoryDiagnostics'] = [memory_probe(item.pid) for item in mains]
    except Exception as error:
        proof.update(safeExceptionType=type(error).__name__, messageCode=getattr(error, 'code', 'probe_failed'))
    print(json.dumps(proof, separators=(',', ':')))


def memory_probe(pid):
    from ctypes import wintypes
    kernel = ctypes.windll.kernel32
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.ReadProcessMemory.argtypes = [wintypes.HANDLE, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t)]
    kernel.IsWow64Process.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.BOOL)]
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    query = ctypes.windll.ntdll.NtQueryInformationProcess
    query.argtypes = [wintypes.HANDLE, wintypes.ULONG, ctypes.c_void_p, wintypes.ULONG, ctypes.POINTER(wintypes.ULONG)]
    query.restype = ctypes.c_long
    proof = {}
    handle = kernel.OpenProcess(0x410, False, pid)
    proof['openWorked'] = bool(handle)
    if not handle:
        return proof
    try:
        info = (ctypes.c_void_p * 6)()
        returned = wintypes.ULONG()
        proof['queryStatus'] = query(handle, 0, ctypes.byref(info), ctypes.sizeof(info), ctypes.byref(returned))
        wow = wintypes.BOOL()
        proof['wowCheckWorked'] = bool(kernel.IsWow64Process(handle, ctypes.byref(wow)))
        proof['wow64'] = bool(wow.value)
        def read(address, size, stage):
            buffer, count = ctypes.create_string_buffer(size), ctypes.c_size_t()
            worked = kernel.ReadProcessMemory(handle, ctypes.c_void_p(address), buffer, size, ctypes.byref(count))
            proof[stage] = {'ok': bool(worked), 'requestedBytes': size, 'readBytes': count.value}
            return buffer.raw
        if info[1]:
            width = ctypes.sizeof(ctypes.c_void_p)
            params = int.from_bytes(read(info[1] + 0x20, width, 'parameterPointer'), 'little')
            current = read(params + 0x38, 16, 'cwdRecord')
            length, pointer = int.from_bytes(current[:2], 'little'), int.from_bytes(current[8:16], 'little')
            proof['cwdLength'] = length
            if length < 32768 and pointer:
                cwd = read(pointer, length, 'cwdValue').decode('utf-16le')
                proof['cwdReadable'] = pathlib.Path(cwd).is_dir()
            env = int.from_bytes(read(params + 0x80, width, 'environmentPointer'), 'little')
            proof['environmentNonempty'] = bool(env)
            if env:
                read(env, 256, 'firstEnvironmentChunk')
    finally:
        kernel.CloseHandle(handle)
    return proof


if __name__ == '__main__':
    main()
