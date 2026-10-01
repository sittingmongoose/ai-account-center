"""One-shot local Windows named pipe, restricted to the current user's SID."""
import ctypes
import os
import re
from ctypes import wintypes

PREFIX = r'\\.\pipe\ccs-update-terminal-'


def valid_endpoint(value):
    return isinstance(value, str) and value.startswith(PREFIX) and re.fullmatch(r'[0-9a-f]{32}', value[len(PREFIX):]) is not None


class PrivatePipe:
    def __init__(self, nonce):
        self.endpoint = PREFIX + nonce
        if not valid_endpoint(self.endpoint): raise ValueError('Invalid endpoint.')
        kernel, advapi = ctypes.windll.kernel32, ctypes.windll.advapi32
        kernel.GetCurrentProcess.restype = wintypes.HANDLE
        advapi.OpenProcessToken.argtypes = [wintypes.HANDLE, wintypes.DWORD, ctypes.POINTER(wintypes.HANDLE)]
        advapi.GetTokenInformation.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD)]
        advapi.ConvertSidToStringSidW.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_wchar_p)]
        advapi.ConvertStringSecurityDescriptorToSecurityDescriptorW.argtypes = [ctypes.c_wchar_p, wintypes.DWORD, ctypes.POINTER(ctypes.c_void_p), ctypes.POINTER(wintypes.DWORD)]
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        kernel.LocalFree.argtypes = [ctypes.c_void_p]
        token = wintypes.HANDLE()
        if not advapi.OpenProcessToken(kernel.GetCurrentProcess(), 8, ctypes.byref(token)): raise OSError('Pipe access setup failed.')
        try:
            size = wintypes.DWORD()
            advapi.GetTokenInformation(token, 1, None, 0, ctypes.byref(size))
            data = ctypes.create_string_buffer(size.value)
            if not advapi.GetTokenInformation(token, 1, data, size, ctypes.byref(size)): raise OSError('Pipe access setup failed.')
            sid = ctypes.c_void_p.from_buffer(data).value
            text = ctypes.c_wchar_p()
            if not advapi.ConvertSidToStringSidW(sid, ctypes.byref(text)): raise OSError('Pipe access setup failed.')
            try: descriptor = 'D:P(A;;GA;;;' + text.value + ')'
            finally: kernel.LocalFree(ctypes.cast(text, ctypes.c_void_p))
        finally: kernel.CloseHandle(token)
        security = ctypes.c_void_p()
        if not advapi.ConvertStringSecurityDescriptorToSecurityDescriptorW(descriptor, 1, ctypes.byref(security), None): raise OSError('Pipe access setup failed.')
        class ATTRIBUTES(ctypes.Structure):
            _fields_ = [('length', wintypes.DWORD), ('descriptor', ctypes.c_void_p), ('inherit', wintypes.BOOL)]
        attributes = ATTRIBUTES(ctypes.sizeof(ATTRIBUTES), security, False)
        kernel.CreateNamedPipeW.argtypes = [ctypes.c_wchar_p, wintypes.DWORD, wintypes.DWORD, wintypes.DWORD, wintypes.DWORD, wintypes.DWORD, wintypes.DWORD, ctypes.POINTER(ATTRIBUTES)]
        kernel.CreateNamedPipeW.restype = wintypes.HANDLE
        try:
            # One writer, byte mode, first-instance-only, reject remote clients.
            self.handle = kernel.CreateNamedPipeW(self.endpoint, 2 | 0x00080000, 8, 1, 2 * 1024 * 1024, 0, 25000, ctypes.byref(attributes))
            if self.handle == wintypes.HANDLE(-1).value: raise OSError('Pipe creation failed.')
        finally: kernel.LocalFree(security)
        self.kernel = kernel
        kernel.ConnectNamedPipe.argtypes = [wintypes.HANDLE, ctypes.c_void_p]
        kernel.WriteFile.argtypes = [wintypes.HANDLE, ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD), ctypes.c_void_p]
        kernel.DisconnectNamedPipe.argtypes = [wintypes.HANDLE]

    def send(self, data):
        if not self.kernel.ConnectNamedPipe(self.handle, None) and self.kernel.GetLastError() != 535: raise OSError('Pipe connection failed.')
        buffer = ctypes.create_string_buffer(data)
        sent = wintypes.DWORD()
        if not self.kernel.WriteFile(self.handle, buffer, len(data), ctypes.byref(sent), None) or sent.value != len(data): raise OSError('Pipe delivery failed.')
        # Flush waits for the connected child to receive the private packet.
        self.kernel.FlushFileBuffers.argtypes = [wintypes.HANDLE]
        self.kernel.FlushFileBuffers(self.handle)
        self.kernel.DisconnectNamedPipe(self.handle)

    def close(self):
        self.kernel.CloseHandle(self.handle)


def receive(endpoint):
    if not valid_endpoint(endpoint): raise ValueError('Invalid endpoint.')
    descriptor = os.open(endpoint, os.O_RDONLY | os.O_BINARY)
    try:
        data = bytearray()
        while True:
            chunk = os.read(descriptor, 65536)
            if not chunk: break
            data.extend(chunk)
            if len(data) > 2 * 1024 * 1024: raise ValueError('Oversized context.')
            # Packet JSON is ASCII escaped and newline terminated; stop before
            # DisconnectNamedPipe reports ERROR_BROKEN_PIPE rather than EOF.
            if data.endswith(b'\n'): break
        return data
    finally: os.close(descriptor)
