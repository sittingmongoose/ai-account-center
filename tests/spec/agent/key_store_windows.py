#!/usr/bin/env python3
"""Windows-seat DPAPI runtime parity. No network, real stores, or file deletion.

    py -3 tests/spec/agent/key_store_windows.py target/debug/aac-agent.exe

The fixture directory is retained for the lead. Ciphertexts are randomized;
compare decrypted bytes against Python's writer using the same CurrentUser
identity and AAC/account-key/v1 entropy, and verify wrong entropy is refused.
"""
import ctypes
from ctypes import wintypes
import datetime as dt
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def main():
    if os.name != 'nt':
        raise SystemExit('Run this test on a Windows seat as the owning user')
    sys.path.insert(0, str(Path(__file__).resolve().parents[3] / 'scripts/account-usage'))
    import key_store

    class Blob(ctypes.Structure):
        _fields_ = [('cbData', wintypes.DWORD), ('pbData', ctypes.POINTER(ctypes.c_ubyte))]

    unprotect = ctypes.windll.crypt32.CryptUnprotectData
    unprotect.argtypes = [ctypes.POINTER(Blob), ctypes.c_void_p, ctypes.POINTER(Blob), ctypes.c_void_p,
                          ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(Blob)]
    unprotect.restype = wintypes.BOOL
    ctypes.windll.kernel32.LocalFree.argtypes = [ctypes.c_void_p]
    ctypes.windll.kernel32.LocalFree.restype = ctypes.c_void_p

    def decrypt(raw, entropy):
        buf = (ctypes.c_ubyte * len(raw)).from_buffer_copy(raw)
        salt = (ctypes.c_ubyte * len(entropy)).from_buffer_copy(entropy)
        source, extra, output = Blob(len(raw), buf), Blob(len(entropy), salt), Blob()
        if not unprotect(ctypes.byref(source), None, ctypes.byref(extra), None, None, 1, ctypes.byref(output)):
            return None
        try:
            return ctypes.string_at(output.pbData, output.cbData)
        finally:
            ctypes.windll.kernel32.LocalFree(output.pbData)

    binary = str(Path(sys.argv[1]).resolve())
    root = Path(tempfile.mkdtemp(prefix='aac-dpapi-runtime-'))
    print('Retained fixtures:', root)
    now = dt.datetime(2026, 1, 2, 3, 4, 5, tzinfo=dt.timezone.utc)
    count = 0
    for provider in ('zai', 'kimi-code', 'opencode-go'):
        rust_home, python_home = root / (provider+'-rust'), root / (provider+'-python')
        rust_home.mkdir(); python_home.mkdir()
        secret = 'fixture-dpapi-secret'
        args = {'action': 'put', 'provider': provider, 'keyId': '12345678', 'secret': secret,
                'home': str(rust_home), 'nowMs': int(now.timestamp()*1000)}
        request = {'protocol': 1, 'verb': 'key-store', 'id': provider, 'deadlineMs': 15000, 'args': args}
        env = os.environ.copy()
        env.update(HOME=str(rust_home), USERPROFILE=str(rust_home), CCS_HOME=str(rust_home / '.ccs'))
        process = subprocess.run([binary], input=json.dumps(request)+'\n', text=True,
                                 capture_output=True, check=True, timeout=30, env=env)
        event = json.loads(process.stdout)
        assert event['type'] == 'result' and event['id'] == provider and event['protocol'] == 1
        reference = key_store.put(provider, '12345678', secret, home=str(python_home), windows=True, now=now)
        assert event['value'] == reference, (provider, 'reply')
        assert secret not in process.stdout + process.stderr
        relative = Path('.ccs/account-usage/keys') / (provider+'-12345678.dpapi')
        actual, expected = (rust_home / relative).read_bytes(), (python_home / relative).read_bytes()
        plaintext = decrypt(actual, b'AAC/account-key/v1')
        assert plaintext is not None and plaintext == decrypt(expected, b'AAC/account-key/v1'), (provider, 'plaintext')
        assert json.loads(plaintext)['secret'] == secret
        assert decrypt(actual, b'wrong-entropy') is None, (provider, 'entropy not enforced')
        assert not (rust_home / relative.with_suffix('.json')).exists(), (provider, 'plaintext file')
        count += 1
        print('PASS', provider)
    print(f'{count} Windows DPAPI runtime parity cases passed')


if __name__ == '__main__':
    main()
