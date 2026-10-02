"""No I/O at construction; compare an already hash-bound native file identity.

The runner hashes its guarded installed ELF before any provider operation. This
adapter verifies that exact file metadata and the actual owned /proc executable
inode before each normalization. It does not assert hostile same-user atomic CAS.
"""
from pathlib import Path
import os
import stat

from native_status_attestor import inspect_process


def file_identity(value):
    return (value.st_dev, value.st_ino, value.st_uid, value.st_mode,
            value.st_nlink, value.st_size, value.st_mtime_ns, value.st_ctime_ns)


class NativePublisherCheck:
    def __init__(self, path, preflight_identity, *, inspector=inspect_process,
                 proc_root=Path('/proc')):
        self.path = Path(path)
        self.preflight_identity = preflight_identity
        self.inspector = inspector
        self.proc_root = Path(proc_root)

    def file_current(self):
        try:return file_identity(self.path.lstat())==self.preflight_identity
        except OSError:return False

    def __call__(self, root):
        try:
            if self.inspector(root.pid) != root:
                return False
            current = self.path.lstat()
            if (not stat.S_ISREG(current.st_mode) or current.st_uid != os.getuid() or
                    current.st_nlink != 1 or file_identity(current) != self.preflight_identity):
                return False
            process_exe = self.proc_root / str(root.pid) / 'exe'
            executable = process_exe.stat()
            if ((executable.st_dev, executable.st_ino) != (current.st_dev, current.st_ino) or
                    (root.executable_device, root.executable_inode) != (current.st_dev, current.st_ino) or
                    root.executable != str(self.path) or os.readlink(process_exe) != str(self.path)):
                return False
            return (file_identity(self.path.lstat()) == self.preflight_identity and
                    self.inspector(root.pid) == root)
        except (OSError, KeyError):
            return False


def pin_native_file(path, expected_sha256):
    import hashlib
    path=Path(path)
    before=path.lstat()
    if (not stat.S_ISREG(before.st_mode) or before.st_uid!=os.getuid() or before.st_nlink!=1 or
            not before.st_mode&stat.S_IXUSR or not 0<before.st_size<=256*1024*1024):
        raise ValueError('native-publisher-unavailable')
    fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW)
    digest=hashlib.sha256()
    with os.fdopen(fd,'rb') as stream:
        if file_identity(os.fstat(stream.fileno()))!=file_identity(before):raise ValueError('native-publisher-changed')
        while block:=stream.read(65536):digest.update(block)
        if file_identity(os.fstat(stream.fileno()))!=file_identity(before):raise ValueError('native-publisher-changed')
    if file_identity(path.lstat())!=file_identity(before) or digest.hexdigest()!=expected_sha256:
        raise ValueError('native-publisher-changed')
    return file_identity(before)
