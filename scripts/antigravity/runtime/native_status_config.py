"""Offline reversible statusLine patch proposal; no live path is invoked.

Patch only the top-level statusLine value. All other native settings bytes stay
identical. Restore requires exact patched bytes, never overwrites a later edit.
"""
from __future__ import annotations
from dataclasses import dataclass
import json
import os
from pathlib import Path
import shlex
import stat
import tempfile

from native_status_attestor import StatusError, bounded_json, fail, sha256

MAX_SETTINGS = 256 * 1024


@dataclass(frozen=True)
class StatusConfigProposal:
    original: bytes
    patched: bytes
    original_command: str | None
    helper_command: str

    @property
    def original_fingerprint(self): return sha256(self.original)
    @property
    def patched_fingerprint(self): return sha256(self.patched)

    def restored_bytes(self, current: bytes):
        if current != self.patched: fail('status-config-restore-conflict')
        return self.original


def _value_span(text):
    """Find exact top-level JSON value spans; no regexp matching private content."""
    decoder = json.JSONDecoder()
    position = 0
    def skip(pos):
        while pos < len(text) and text[pos] in ' \t\r\n': pos += 1
        return pos
    position = skip(position)
    if text[position:position + 1] != '{': fail('status-config-object-required')
    position = skip(position + 1)
    while text[position:position + 1] != '}':
        try: key, end = decoder.raw_decode(text, position)
        except ValueError: fail('status-config-invalid')
        if type(key) is not str: fail('status-config-invalid')
        position = skip(end)
        if text[position:position + 1] != ':': fail('status-config-invalid')
        start = skip(position + 1)
        try: _, end = decoder.raw_decode(text, start)
        except ValueError: fail('status-config-invalid')
        if key == 'statusLine': return start, end
        position = skip(end)
        if text[position:position + 1] == ',': position = skip(position + 1)
        elif text[position:position + 1] != '}': fail('status-config-invalid')
    return None


def prepare_status_config(original: bytes, *, python_path: str, helper_path: str,
                          socket_path: str, original_command_file: str):
    settings = bounded_json(original, MAX_SETTINGS)
    for path in (python_path, helper_path, socket_path, original_command_file):
        if (type(path) is not str or not path.startswith('/') or '\x00' in path or
                any(ord(char) < 32 for char in path)):
            fail('status-config-helper-path-invalid')
    previous = settings.get('statusLine')
    original_command = None
    if 'statusLine' in settings:
        if (type(previous) is not dict or previous.get('type') != 'command' or
                type(previous.get('command')) is not str or not previous['command'] or
                len(previous['command']) > 4096 or '\x00' in previous['command']):
            fail('status-config-existing-hook-unsupported')
        if previous.get('enabled') is False: fail('status-config-existing-hook-disabled')
        # Existing statusLine settings retain all values except the multiplexed
        # command. The original command receives exactly its prior native stdin.
        original_command = previous['command']
        block = dict(previous)
    else:
        block = {'type': 'command', 'enabled': True, 'stack_with_default': True}
    command = ' '.join(shlex.quote(item) for item in
                       (python_path, '-I', helper_path, '--socket', socket_path,
                        '--original-command-file', original_command_file))
    block['command'] = command
    text = original.decode('utf-8')
    span = _value_span(text)
    replacement = json.dumps(block, ensure_ascii=False, separators=(',', ':'))
    if span:
        patched = text[:span[0]] + replacement + text[span[1]:]
    else:
        end = text.rfind('}')
        addition = (',' if settings else '') + '"statusLine":' + replacement
        patched = text[:end] + addition + text[end:]
    candidate = patched.encode('utf-8')
    parsed = bounded_json(candidate, MAX_SETTINGS)
    if {key: value for key, value in parsed.items() if key != 'statusLine'} != settings:
        expected = {key: value for key, value in settings.items() if key != 'statusLine'}
        if {key: value for key, value in parsed.items() if key != 'statusLine'} != expected:
            fail('status-config-unrelated-settings-changed')
    return StatusConfigProposal(original, candidate, original_command, command)


class TemporaryConfigTransaction:
    """Explicit candidate, used only with disposable fixture paths in this gate.

    Production native path authorization is separate and not provided by this
    module. Inode, ownership, mode and byte comparisons guard both operations.
    """
    def __init__(self, path: Path, proposal: StatusConfigProposal):
        self.path, self.proposal = Path(path), proposal
        self.original_stat = None
        self.patched_inode = None
        self.patch_committed = False

    def _safe_read(self):
        try:
            parent = self.path.parent.lstat()
            if (not stat.S_ISDIR(parent.st_mode) or parent.st_uid != os.getuid() or
                    stat.S_IMODE(parent.st_mode) & 0o022 or
                    self.path.parent.resolve() != self.path.parent):
                fail('status-config-directory-unsafe')
            info = self.path.lstat()
            if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_nlink != 1):
                fail('status-config-file-unsafe')
            descriptor = os.open(self.path, os.O_RDONLY | os.O_NOFOLLOW)
            with os.fdopen(descriptor, 'rb') as stream:
                opened = os.fstat(stream.fileno())
                if (opened.st_dev, opened.st_ino) != (info.st_dev, info.st_ino):
                    fail('status-config-file-changed')
                raw = stream.read(MAX_SETTINGS + 1)
            if len(raw) > MAX_SETTINGS: fail('status-config-size')
            return info, raw
        except StatusError: raise
        except OSError: fail('status-config-read-failed')

    def _replace(self, expected_inode, expected_bytes, replacement, mode, times=None,
                 on_committed=None):
        # Narrow compare-before-restore guard. Same-user concurrent edits cannot
        # be blindly overwritten: check leaf identity and exact bytes again.
        info, raw = self._safe_read()
        if (info.st_dev, info.st_ino) != expected_inode or raw != expected_bytes:
            fail('status-config-write-conflict')
        descriptor, temporary = tempfile.mkstemp(prefix='.status-config-', dir=self.path.parent)
        try:
            with os.fdopen(descriptor, 'wb') as stream:
                stream.write(replacement); stream.flush(); os.fsync(stream.fileno())
                os.fchmod(stream.fileno(), mode)
            info_again, raw_again = self._safe_read()
            if (info_again.st_dev, info_again.st_ino) != expected_inode or raw_again != expected_bytes:
                fail('status-config-write-conflict')
            committed_info = Path(temporary).lstat()
            committed_identity = (committed_info.st_dev, committed_info.st_ino)
            os.replace(temporary, self.path)
            # Capture *our* temporary inode immediately at commit, before any
            # subsequent utime/directory-fsync can raise. Never adopt a foreign
            # leaf inode after an intervening edit as our committed write.
            if on_committed is not None:
                on_committed(committed_identity)
            if times is not None: os.utime(self.path, ns=times, follow_symlinks=False)
            parent = os.open(self.path.parent, os.O_RDONLY | os.O_DIRECTORY)
            try: os.fsync(parent)
            finally: os.close(parent)
        except StatusError: raise
        except OSError: fail('status-config-write-failed')
        finally:
            try: Path(temporary).unlink()
            except FileNotFoundError: pass
        # Return only the inode committed by this operation. A foreign leaf
        # replacement during successful directory sync cannot become ours.
        return committed_identity

    def apply(self):
        if self.original_stat is not None: fail('status-config-already-applied')
        info, raw = self._safe_read()
        if raw != self.proposal.original: fail('status-config-apply-conflict')
        self.original_stat = info
        def committed(inode):
            self.patched_inode = inode
            self.patch_committed = True
        self.patched_inode = self._replace((info.st_dev, info.st_ino), raw,
                                          self.proposal.patched, stat.S_IMODE(info.st_mode),
                                          on_committed=committed)

    def restore(self):
        if self.original_stat is None or self.patched_inode is None: fail('status-config-not-applied')
        info = self.original_stat
        self._replace(self.patched_inode, self.proposal.patched, self.proposal.original,
                      stat.S_IMODE(info.st_mode), (info.st_atime_ns, info.st_mtime_ns))
        self.patched_inode = None
