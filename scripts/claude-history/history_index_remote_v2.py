"""Fixed private Claude index collector/checker and reviewed Node append bridge.

Source-only candidate. No request selects a local path, executable or command.
Collect buffers are PRIVATE SSH pipe data; never display or log this stdout.
No authentication, cookies, transcript contents, GUI or resume APIs are used.
"""
import base64
import ctypes
import fnmatch
import hashlib
import itertools
import json
import math
import os
from pathlib import Path, PurePosixPath
import plistlib
import re
import shlex
import stat
import struct
import subprocess
import sys

PROFILES = frozenset(('platyr', 'gmail', 'party', 'me'))
PLATFORMS = frozenset(('mac', 'windows'))
MODES = frozenset(('closed-check', 'collect', 'protected-check', 'verify-transcripts', 'append', 'snapshot-cleanup'))
UUID = re.compile(r'[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}')
NAME = re.compile(r'local_[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\.json')
SHA = re.compile(r'[0-9a-f]{64}')
SNAPSHOT_DIR = re.compile(r'\.history-index-snapshot-[0-9a-f]{32}')
SNAPSHOT_PROTECTED = re.compile(r'protected-[0-9]+\.bin')
SNAPSHOT_MANIFEST = 'snapshot-manifest.json'
SNAPSHOT_KEEP = 3
SNAPSHOT_DELETE_CAP = 200
MAX_FILE = 2_000_000
MAX_TOTAL = 16_000_000
MAX_PIPE = 24 * 1024 * 1024
TRANSACTION_SHA = '0b3b4cbb14db144457684bf7683d53bbbc38529d4b92c9c6590ab089eff0c132'
NATIVE = {
    'mac': ('2.19675.0', '.vite/build/index.chunk-DPWtnchX.js', '102d49173311cfcba1bfa62abc5ea986797f5b97ec16caa3784ee65cd19774ac'),
    'windows': ('2.19675.0.0', '.vite/build/index.chunk-DmADLdv3.js', '492242eb2c352f60a886031275b7a37fd2fc791946be98b8c8fdd4bbfc41ded6'),
}
sha = lambda raw: hashlib.sha256(raw).hexdigest()
text_sha = lambda text: sha(text.encode('utf-8'))

class Refused(Exception):
    pass

def refuse(code='unavailable'):
    raise Refused(code)

def encode(value):
    return json.dumps(value, separators=(',', ':'), ensure_ascii=False, allow_nan=False).encode('utf-8')

def strict_keys(value, required, optional=()):
    if not isinstance(value, dict) or not set(required) <= set(value) or not set(value) <= set(required) | set(optional):
        refuse('invalid_request')

def clean_text(value, limit=4096):
    if not isinstance(value, str) or not value or len(value) > limit or any(ord(c) < 32 or ord(c) == 127 for c in value):
        refuse('invalid_policy')
    return value

def validate_policy(profile, policy):
    strict_keys(policy, ('version', 'enabled', 'sourcePlatform', 'identity', 'project', 'ssh'))
    if policy['version'] != 1 or policy['enabled'] is not True or profile not in ('platyr', 'gmail'):
        refuse('policy_unavailable')
    if policy['sourcePlatform'] != ('mac' if profile == 'platyr' else 'windows'):
        refuse('policy_direction_invalid')
    strict_keys(policy['identity'], ('accountUuid', 'organizationUuid'))
    for value in policy['identity'].values():
        if not isinstance(value, str) or not UUID.fullmatch(value):
            refuse('invalid_policy')
    strict_keys(policy['project'], ('cwd', 'originCwd', 'transcriptRoot'))
    if policy['project']['cwd'] != '/mnt/Cursor/PuppetMaster' or policy['project']['originCwd'] != policy['project']['cwd']:
        refuse('project_policy_unavailable')
    remote = PurePosixPath(clean_text(policy['project']['transcriptRoot']))
    if not remote.is_absolute() or str(remote) != policy['project']['transcriptRoot'] or '..' in remote.parts or '\\' in str(remote):
        refuse('invalid_policy')
    strict_keys(policy['ssh'], ('mac', 'windows'))
    for endpoint in policy['ssh'].values():
        strict_keys(endpoint, ('alias', 'hostname', 'username', 'port'))
        for key in ('alias', 'hostname', 'username'):
            clean_text(endpoint[key], 255)
        if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]*', endpoint['alias']) or not isinstance(endpoint['port'], int) or isinstance(endpoint['port'], bool) or not 1 <= endpoint['port'] <= 65535:
            refuse('invalid_policy')
    return policy

def fixed_root(home, platform, profile):
    if platform == 'mac':
        return home / 'Library' / 'Application Support' / ('Claude' if profile == 'platyr' else 'Claude-' + profile)
    if profile == 'gmail':
        return home / 'AppData' / 'Local' / 'Packages' / 'Claude_pzs8sxrjxfjjc' / 'LocalCache' / 'Roaming' / 'Claude'
    return home / 'AppData' / 'Roaming' / ('Claude-' + profile)

def directory_identity(path):
    for component in (path, *path.parents):
        info = component.lstat()
        if not stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode) or getattr(info, 'st_file_attributes', 0) & getattr(stat, 'FILE_ATTRIBUTE_REPARSE_POINT', 0x400):
            refuse('unsafe_directory')
    info = path.lstat()
    if info.st_dev <= 0 or info.st_ino <= 0:
        refuse('unknown_directory_identity')
    return {'dev': str(info.st_dev), 'ino': str(info.st_ino)}

def read_regular(path, optional=False, limit=MAX_FILE, allow_owned_staging_link=False):
    directory_identity(path.parent)
    try:
        before = path.lstat()
    except FileNotFoundError:
        if optional:
            return None
        raise
    if not stat.S_ISREG(before.st_mode) or before.st_size > limit or before.st_nlink not in ((1, 2) if allow_owned_staging_link else (1,)):
        refuse('unsafe_file')
    fd = os.open(path, os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0))
    try:
        opened = os.fstat(fd)
        if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino) or not stat.S_ISREG(opened.st_mode):
            refuse('file_changed')
        with os.fdopen(fd, 'rb', closefd=False) as handle:
            raw = handle.read(limit + 1)
        after = path.lstat()
        if len(raw) > limit or (after.st_dev, after.st_ino) != (opened.st_dev, opened.st_ino):
            refuse('file_changed')
        return raw
    finally:
        os.close(fd)

def object_from(raw):
    value = json.loads(raw.decode('utf-8-sig'))
    if not isinstance(value, dict):
        refuse('metadata_invalid')
    return value

def endpoint_from_config(raw, expected):
    alias = expected['alias']
    blocks, selected, fields = [], False, {}
    for line in raw.decode('utf-8-sig').splitlines() + ['Host __ccs_end__']:
        line = line.strip()
        if not line or line.startswith('#'):
            continue
        pieces = line.split(None, 1)
        if len(pieces) != 2:
            refuse('ssh_configuration_unknown')
        key, value = pieces[0].lower(), pieces[1].strip().strip('"')
        if key in ('include', 'match', 'proxycommand', 'proxyjump', 'localcommand', 'remotecommand', 'permitlocalcommand'):
            refuse('ssh_configuration_unknown')
        if key == 'host':
            if selected:
                blocks.append(fields)
            patterns = value.split()
            if any(pattern.startswith('!') for pattern in patterns):
                refuse('ssh_configuration_unknown')
            if any(fnmatch.fnmatchcase(alias, pattern) for pattern in patterns if not pattern.startswith('!')) and alias not in patterns:
                refuse('ssh_configuration_unknown')
            selected, fields = alias in patterns, {}
        elif selected:
            if key in ('proxycommand', 'proxyjump', 'localcommand', 'remotecommand', 'permitlocalcommand'):
                refuse('ssh_configuration_unknown')
            if key in ('hostname', 'user', 'port'):
                if key in fields:
                    refuse('ssh_configuration_unknown')
                fields[key] = value
    if len(blocks) != 1:
        refuse('endpoint_unverified')
    fields = blocks[0]
    fields.setdefault('port', '22')
    if fields != {'hostname': expected['hostname'], 'user': expected['username'], 'port': str(expected['port'])}:
        refuse('endpoint_unverified')
    return {'hostnameSha256': text_sha(fields['hostname']), 'usernameSha256': text_sha(fields['user']), 'portSha256': text_sha(fields['port'])}

def windows_powershell(script):
    executable = Path(os.environ.get('SystemRoot', 'C:/Windows')) / 'System32/WindowsPowerShell/v1.0/powershell.exe'
    script = '[Console]::InputEncoding=[Text.UTF8Encoding]::new($false);[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false);' + script
    encoded = base64.b64encode(script.encode('utf-16le')).decode('ascii')
    result = subprocess.run([str(executable), '-NoProfile', '-NonInteractive', '-EncodedCommand', encoded], capture_output=True, timeout=8)
    if result.returncode or len(result.stdout) > 1_048_576:
        refuse('process_state_unknown')
    return result.stdout.decode('utf-8-sig').strip()

def mac_arguments(pid):
    library = ctypes.CDLL('/usr/lib/libSystem.B.dylib', use_errno=True)
    library.sysctl.argtypes = [ctypes.POINTER(ctypes.c_int), ctypes.c_uint, ctypes.c_void_p, ctypes.POINTER(ctypes.c_size_t), ctypes.c_void_p, ctypes.c_size_t]
    library.sysctl.restype = ctypes.c_int
    mib, size = (ctypes.c_int * 3)(1, 49, pid), ctypes.c_size_t()
    if library.sysctl(mib, 3, None, ctypes.byref(size), None, 0) or not 0 < size.value <= 262144:
        refuse('process_state_unknown')
    buffer = ctypes.create_string_buffer(size.value)
    if library.sysctl(mib, 3, buffer, ctypes.byref(size), None, 0):
        refuse('process_state_unknown')
    raw = buffer.raw[:size.value]
    argc = int.from_bytes(raw[:4], sys.byteorder)
    if not 0 < argc < 1024:
        refuse('process_state_unknown')
    offset = raw.index(b'\0', 4) + 1
    while offset < len(raw) and raw[offset] == 0:
        offset += 1
    return [piece.decode('utf-8') for piece in raw[offset:].split(b'\0')[:argc]]

def windows_arguments(command):
    import ctypes.wintypes as wt
    shell, kernel = ctypes.WinDLL('shell32', use_last_error=True), ctypes.WinDLL('kernel32', use_last_error=True)
    shell.CommandLineToArgvW.argtypes, shell.CommandLineToArgvW.restype = [wt.LPCWSTR, ctypes.POINTER(ctypes.c_int)], ctypes.POINTER(wt.LPWSTR)
    kernel.LocalFree.argtypes, kernel.LocalFree.restype = [ctypes.c_void_p], ctypes.c_void_p
    count = ctypes.c_int()
    pointer = shell.CommandLineToArgvW(command, ctypes.byref(count))
    if not pointer or not 0 < count.value < 1024:
        refuse('process_state_unknown')
    try:
        return [pointer[index] for index in range(count.value)]
    finally:
        kernel.LocalFree(ctypes.cast(pointer, ctypes.c_void_p))

def local_closed(platform, profile, root):
    argv_rows = []
    if platform == 'mac':
        result = subprocess.run(['/usr/bin/pgrep', '-x', 'Claude'], capture_output=True, timeout=5)
        if result.returncode not in (0, 1):
            refuse('process_state_unknown')
        for value in result.stdout.decode('ascii').split():
            if not value.isdigit():
                refuse('process_state_unknown')
            pid = int(value)
            try:
                argv_rows.append(mac_arguments(pid))
            except Refused:
                try:
                    os.kill(pid, 0)
                except ProcessLookupError:
                    continue
                raise
        default = 'platyr'
    else:
        raw = windows_powershell('$ErrorActionPreference="Stop"; Get-CimInstance Win32_Process -Filter "Name=\'Claude.exe\'" | Select-Object ProcessId,CommandLine | ConvertTo-Json -Compress')
        entries = json.loads(raw) if raw else []
        entries = entries if isinstance(entries, list) else [entries]
        for row in entries:
            if not isinstance(row, dict) or not isinstance(row.get('CommandLine'), str):
                refuse('process_state_unknown')
            argv_rows.append(windows_arguments(row['CommandLine']))
        default = 'gmail'
    target_identity = directory_identity(root)
    for argv in argv_rows:
        directories = []
        for index, value in enumerate(argv):
            if value.startswith('--user-data-dir='):
                directories.append(value.split('=', 1)[1])
            elif value == '--user-data-dir':
                if index + 1 >= len(argv):
                    refuse('process_state_unknown')
                directories.append(argv[index + 1])
        if len(directories) > 1:
            refuse('process_state_unknown')
        if directories:
            argument = directories[0]
            if not argument or not Path(argument).is_absolute() or any(ord(c) < 32 or ord(c) == 127 for c in argument):
                refuse('process_state_unknown')
            try:
                argument_identity = directory_identity(Path(argument))
            except (OSError, Refused):
                refuse('process_state_unknown')
            if argument_identity == target_identity:
                return False
        if not directories and profile == default:
            return False
    return True

def native_guard(platform):
    version, member, expected_sha = NATIVE[platform]
    if platform == 'mac':
        bundle = Path('/Applications/Claude.app')
        metadata = plistlib.loads(read_regular(bundle / 'Contents/Info.plist'))
        actual_version = metadata.get('CFBundleShortVersionString')
        archive = bundle / 'Contents/Resources/app.asar'
    else:
        raw = windows_powershell('$ErrorActionPreference="Stop"; @(Get-AppxPackage -Name Claude | Select-Object Name,Version,PackageFullName,InstallLocation) | ConvertTo-Json -Compress')
        packages = json.loads(raw) if raw else []
        packages = packages if isinstance(packages, list) else [packages]
        if len(packages) != 1 or packages[0].get('PackageFullName') != 'Claude_2.19675.0.0_x64__pzs8sxrjxfjjc':
            refuse('native_guard_unverified')
        actual_version = packages[0].get('Version')
        if packages[0].get('InstallLocation') != 'C:\\Program Files\\WindowsApps\\Claude_2.19675.0.0_x64__pzs8sxrjxfjjc':
            refuse('native_guard_unverified')
        archive = Path(packages[0]['InstallLocation']) / 'app/resources/app.asar'
    directory_identity(archive.parent)
    before = archive.lstat()
    if not stat.S_ISREG(before.st_mode) or before.st_size > 1_000_000_000:
        refuse('native_guard_unverified')
    with archive.open('rb') as handle:
        _, header_size, _, json_size = struct.unpack('<4I', handle.read(16))
        if not 0 < json_size < 20_000_000 or header_size < json_size:
            refuse('native_guard_unverified')
        entry = json.loads(handle.read(json_size))
        for name in member.split('/'):
            entry = entry['files'][name]
        if entry.get('unpacked') or entry.get('link') or not 0 < entry['size'] < 10_000_000:
            refuse('native_guard_unverified')
        handle.seek(8 + header_size + int(entry['offset']))
        actual_sha = sha(handle.read(entry['size']))
    after = archive.lstat()
    if (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns) != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns) or actual_version != version or actual_sha != expected_sha:
        refuse('native_guard_unverified')
    return {'version': actual_version, 'managerSha256': actual_sha, 'warmGuardVerified': True, 'autoResumeGuardVerified': True}

def validate_wire_records(records):
    if not isinstance(records, list) or len(records) > 200:
        refuse('registry_invalid')
    decoded, ids, cli_ids, total = [], set(), set(), 0
    for record in records:
        strict_keys(record, ('name', 'base64', 'sha256'))
        if not isinstance(record['name'], str) or not NAME.fullmatch(record['name']) or not isinstance(record['base64'], str) or not isinstance(record['sha256'], str) or not SHA.fullmatch(record['sha256']):
            refuse('record_invalid')
        raw = base64.b64decode(record['base64'], validate=True)
        total += len(raw)
        if len(raw) > MAX_FILE or total > MAX_TOTAL or sha(raw) != record['sha256']:
            refuse('record_invalid')
        value = object_from(raw)
        sid, cid = value.get('sessionId'), value.get('cliSessionId')
        if not isinstance(sid, str) or sid + '.json' != record['name'] or not isinstance(cid, str) or not UUID.fullmatch(cid):
            refuse('record_invalid')
        if sid in ids or cid in cli_ids:
            refuse('identity_collision')
        ids.add(sid); cli_ids.add(cid)
        decoded.append((record, raw, value))
    return decoded

def validate_neutral(records, policy, platform):
    required = {'sessionId', 'cliSessionId', 'cwd', 'originCwd', 'createdAt', 'lastActivityAt', 'lastFocusedAt', 'title', 'isArchived', 'permissionMode', 'importedFrom', 'resumeConfirmed', 'remoteControlAutoEligible', 'sshConfig', 'sshRemoteTranscriptPath'}
    optional = {'model', 'effort', 'completedTurns'}
    for _, _, value in validate_wire_records(records):
        strict_keys(value, required, optional)
        if value['cwd'] != policy['project']['cwd'] or value['originCwd'] != policy['project']['originCwd'] or value['sshConfig'] != {'sshHost': policy['ssh'][platform]['alias']}:
            refuse('record_invalid')
        if value['lastFocusedAt'] != 0 or value['permissionMode'] != 'default' or value['importedFrom'] != 'local-1p-code' or value['resumeConfirmed'] is not False or value['remoteControlAutoEligible'] is not False or not isinstance(value['isArchived'], bool):
            refuse('record_invalid')
        for key in ('createdAt', 'lastActivityAt'):
            number = value[key]
            if isinstance(number, bool) or not isinstance(number, (int, float)) or not math.isfinite(number) or number < 0:
                refuse('record_invalid')
        clean_text(value['title'], 1000)
        reference = PurePosixPath(clean_text(value['sshRemoteTranscriptPath']))
        root = PurePosixPath(policy['project']['transcriptRoot'])
        try:
            relative = reference.relative_to(root)
        except ValueError:
            refuse('record_invalid')
        if str(reference) != value['sshRemoteTranscriptPath'] or len(relative.parts) != 2 or '..' in relative.parts or '\\' in str(reference) or relative.name != value['cliSessionId'] + '.jsonl':
            refuse('record_invalid')
        if 'model' in value and (not isinstance(value['model'], str) or not re.fullmatch(r'claude-[A-Za-z0-9._-]{1,100}', value['model'])):
            refuse('record_invalid')
        if 'effort' in value and value['effort'] not in ('low', 'medium', 'high', 'max', 'xhigh'):
            refuse('record_invalid')
        if 'completedTurns' in value and (isinstance(value['completedTurns'], bool) or not isinstance(value['completedTurns'], int) or not 0 <= value['completedTurns'] <= 1_000_000):
            refuse('record_invalid')

class BoundProfile:
    def __init__(self, profile, platform, policy, home=None, closed=None, native=None):
        self.profile, self.platform, self.policy = profile, platform, validate_policy(profile, policy)
        self.home = Path.home() if home is None else Path(home)
        self.root = fixed_root(self.home, platform, profile)
        self.closed = closed or (lambda: local_closed(platform, profile, self.root))
        self.native = native or (lambda: native_guard(platform))

    def bind(self):
        directory_identity(self.root)
        cfg_raw = read_regular(self.root / 'config.json')
        cfg = object_from(cfg_raw)
        account = cfg.get('lastKnownAccountUuid')
        expected = self.policy['identity']
        if not isinstance(account, str) or not UUID.fullmatch(account) or account.lower() != expected['accountUuid'].lower():
            refuse('identity_mismatch')
        account_root = self.root / 'claude-code-sessions' / account
        directory_identity(account_root)
        entries = list(itertools.islice(account_root.iterdir(), 201))
        if len(entries) > 200:
            refuse('registry_invalid')
        orgs = [p for p in entries if UUID.fullmatch(p.name)]
        if len(orgs) != 1 or orgs[0].name.lower() != expected['organizationUuid'].lower():
            refuse('identity_mismatch')
        registry = orgs[0]
        directory_identity(registry)
        configs_raw = read_regular(self.root / 'ssh_configs.json')
        configs = object_from(configs_raw).get('configs')
        alias = self.policy['ssh'][self.platform]['alias']
        if not isinstance(configs, list) or len([row for row in configs if isinstance(row, dict) and row.get('sshHost') == alias]) != 1:
            refuse('endpoint_unverified')
        user_ssh = read_regular(self.home / '.ssh/config')
        endpoint = endpoint_from_config(user_ssh, self.policy['ssh'][self.platform])
        return registry, {'accountSha256': text_sha(account.lower()), 'orgSha256': text_sha(registry.name.lower())}, endpoint

    def snapshot_cleanup(self):
        # Retention for the writer's per-transaction snapshots: keep the newest SNAPSHOT_KEEP
        # snapshot folders in this profile root, delete older ones this helper's writer created.
        # Only folders named exactly by the writer's pattern are candidates, and only when every
        # entry inside is the manifest or a protected-N.bin regular file; anything else is skipped
        # and reported, never deleted. The identity bind runs first, so a wrong root refuses.
        self.bind()
        candidates = []
        skipped = []
        for entry in itertools.islice(self.root.iterdir(), 1024):
            if not SNAPSHOT_DIR.fullmatch(entry.name):
                continue
            try:
                info = entry.lstat()
            except OSError:
                skipped.append(entry.name)
                continue
            if not stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode):
                skipped.append(entry.name)
                continue
            try:
                children = list(itertools.islice(entry.iterdir(), 64))
            except OSError:
                skipped.append(entry.name)
                continue
            owned = True
            for child in children:
                try:
                    child_info = child.lstat()
                except OSError:
                    owned = False
                    break
                if (
                    not stat.S_ISREG(child_info.st_mode)
                    or stat.S_ISLNK(child_info.st_mode)
                    or child_info.st_nlink != 1
                    or (
                        child.name != SNAPSHOT_MANIFEST
                        and not SNAPSHOT_PROTECTED.fullmatch(child.name)
                    )
                ):
                    owned = False
                    break
            if not owned or not any(child.name == SNAPSHOT_MANIFEST for child in children):
                skipped.append(entry.name)
                continue
            candidates.append((info.st_mtime, entry))
        candidates.sort(key=lambda item: item[0], reverse=True)
        kept = [entry.name for _, entry in candidates[:SNAPSHOT_KEEP]]
        deleted = []
        for _, entry in candidates[SNAPSHOT_KEEP:]:
            if len(deleted) >= SNAPSHOT_DELETE_CAP:
                skipped.append(entry.name)
                continue
            try:
                children = sorted(entry.iterdir(), key=lambda child: child.name)
                if any(
                    child.name != SNAPSHOT_MANIFEST
                    and not SNAPSHOT_PROTECTED.fullmatch(child.name)
                    for child in children
                ):
                    skipped.append(entry.name)
                    continue
                for child in children:
                    child_info = child.lstat()
                    if not stat.S_ISREG(child_info.st_mode) or child_info.st_nlink != 1:
                        raise OSError('snapshot entry changed')
                    child.unlink()
                entry.rmdir()
            except OSError:
                skipped.append(entry.name)
                continue
            deleted.append(entry.name)
        return {'kept': kept, 'deleted': deleted, 'skipped': sorted(skipped)}

    def protection(self, registry):
        files, buffers = [], {}
        for relative, path in [('config.json', self.root / 'config.json'), ('ssh_configs.json', self.root / 'ssh_configs.json'), ('ssh-remote-server-state.json', self.root / 'ssh-remote-server-state.json'), ('scheduled-tasks.json', registry / 'scheduled-tasks.json'), ('ssh-user-config', self.home / '.ssh/config')]:
            raw = read_regular(path, optional=True)
            files.append({'relative': relative, 'present': raw is not None, 'sha256': sha(raw) if raw is not None else None})
            buffers[relative] = raw
        waiting = registry / 'waiting-input'
        try:
            directory_identity(waiting)
            entries = list(itertools.islice(waiting.iterdir(), 201))
            if len(entries) > 200:
                refuse('pending_state_unknown')
            waiting_state = {'present': True, 'entryNameHashes': sorted(text_sha(p.name) for p in entries), 'directory': directory_identity(waiting)}
        except FileNotFoundError:
            waiting_state = {'present': False, 'entryNameHashes': []}
        state = {'files': files, 'directories': {'profile': directory_identity(self.root), 'registry': directory_identity(registry)}, 'waitingState': waiting_state}
        state['stamp'] = sha(encode(state))
        return state, buffers

    def collect(self, owned_candidates=()):
        registry, identity, endpoint = self.bind()
        before, _ = self.protection(registry)
        names = self.record_names(registry)
        records = []
        owned_names = {row['name'] for row in owned_candidates}
        for name in names:
            if not NAME.fullmatch(name):
                refuse('record_invalid')
            raw = read_regular(registry / name, allow_owned_staging_link=name in owned_names)
            records.append({'name': name, 'base64': base64.b64encode(raw).decode('ascii'), 'sha256': sha(raw)})
        decoded = validate_wire_records(records)
        stable = all(read_regular(registry / record['name'], allow_owned_staging_link=record['name'] in owned_names) == raw for record, raw, _ in decoded)
        after_names = self.record_names(registry)
        after, buffers = self.protection(registry)
        _, after_identity, after_endpoint = self.bind()
        stable = stable and names == after_names and identity == after_identity and endpoint == after_endpoint
        pending_keys = ('pendingFirstStart', 'pendingMessages', 'armedWork', 'armedWorkAtQuit', 'query', 'inputStream', 'resumeSeed', 'heldAcrossLoss')
        no_pending = not after['waitingState']['entryNameHashes'] and all(not any(value.get(k) not in (None, False, '', [], {}) for k in pending_keys) for _, _, value in decoded)
        tasks = buffers['scheduled-tasks.json']
        no_scheduled = tasks is None or object_from(tasks).get('scheduledTasks') == []
        no_scheduled = no_scheduled and all(value.get('scheduledTaskId') in (None, '') for _, _, value in decoded)
        revision = sha(encode([[row['name'], row['sha256']] for row in records]))
        return {'profileId': self.profile, 'platform': self.platform, 'identity': identity, 'records': records, 'revision': revision, 'snapshotStable': stable, 'endpoint': endpoint, 'nativeGuard': self.native(), 'noPendingInput': no_pending, 'noScheduledWork': no_scheduled, 'protectedSnapshotStable': before == after, 'protectedSnapshot': after, 'localProfileClosed': self.closed(), 'pendingInputState': 'no_waiting_records_or_saved_pending_fields' if no_pending else 'waiting_or_saved_pending_state_unknown'}

    @staticmethod
    def record_names(registry):
        entries = list(itertools.islice(registry.iterdir(), 1025))
        if len(entries) > 1024:
            refuse('registry_invalid')
        names = sorted(p.name for p in entries if p.name.startswith('local_') and p.name.endswith('.json'))
        if len(names) > 200:
            refuse('registry_invalid')
        return names

    def unchanged(self, expected, candidates=()):
        if not isinstance(expected, dict) or expected.get('profileId') != self.profile or expected.get('platform') != self.platform or expected.get('snapshotStable') is not True or expected.get('protectedSnapshotStable') is not True:
            refuse('target_state_changed')
        baseline = validate_wire_records(expected.get('records'))
        allowed = validate_wire_records(list(candidates))
        validate_neutral(list(candidates), self.policy, self.platform)
        if expected.get('revision') != sha(encode([[r['name'], r['sha256']] for r in sorted(expected['records'], key=lambda row: row['name'])])):
            refuse('target_state_changed')
        if {v['cliSessionId'] for _, _, v in baseline} & {v['cliSessionId'] for _, _, v in allowed}:
            refuse('identity_collision')
        if self.closed() is not True:
            refuse('target_opened')
        current = self.collect(owned_candidates=candidates)
        if current['localProfileClosed'] is not True or current['snapshotStable'] is not True or current['protectedSnapshotStable'] is not True or current['noPendingInput'] is not True or current['noScheduledWork'] is not True:
            refuse('target_state_changed')
        for key in ('identity', 'endpoint', 'nativeGuard', 'protectedSnapshot'):
            if current[key] != expected.get(key):
                refuse('target_state_changed')
        permitted = {record['name']: raw for record, raw, _ in baseline}
        for record, raw, _ in allowed:
            if record['name'] in permitted:
                refuse('identity_collision')
            permitted[record['name']] = raw
        current_rows = validate_wire_records(current['records'])
        current_names = {record['name'] for record, _, _ in current_rows}
        if any(record['name'] not in current_names for record, _, _ in baseline):
            refuse('target_state_changed')
        for record, raw, _ in current_rows:
            if permitted.get(record['name']) != raw:
                refuse('target_state_changed')
        if self.closed() is not True:
            refuse('target_opened')
        return True

    def verify_transcripts(self, records):
        self.bind()
        references = []
        root = PurePosixPath(self.policy['project']['transcriptRoot'])
        for _, _, value in validate_wire_records(records):
            path = value.get('sshRemoteTranscriptPath')
            if not isinstance(path, str):
                refuse('transcript_metadata_unverified')
            actual = PurePosixPath(path)
            try:
                relative = actual.relative_to(root)
            except ValueError:
                refuse('transcript_metadata_unverified')
            if str(actual) != path or len(relative.parts) != 2 or '..' in relative.parts or '\\' in path or relative.name != value['cliSessionId'] + '.jsonl':
                refuse('transcript_metadata_unverified')
            references.append(path)
        probe = "import json,stat,sys;from pathlib import Path\np=json.loads(sys.stdin.read());ok=True\nfor name in p['paths']:\n try:\n  f=Path(name);s=f.lstat();ok=ok and stat.S_ISREG(s.st_mode) and s.st_nlink==1 and all(stat.S_ISDIR(x.lstat().st_mode) and not stat.S_ISLNK(x.lstat().st_mode) for x in (f.parent,*f.parent.parents))\n except OSError:ok=False\nprint(json.dumps({'verified':ok}))\n"
        command = '/usr/bin/python3 -B -c ' + shlex.quote(probe)
        alias = self.policy['ssh'][self.platform]['alias']
        result = subprocess.run(['ssh', '-T', '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes', '-o', 'ConnectTimeout=5', '-o', 'ConnectionAttempts=1', '-o', 'ControlMaster=no', '-o', 'ControlPath=none', '-o', 'PermitLocalCommand=no', '-o', 'ClearAllForwardings=yes', '--', alias, command], input=encode({'paths': references}), capture_output=True, timeout=12)
        if result.returncode or len(result.stdout) > 128 or json.loads(result.stdout) != {'verified': True}:
            refuse('transcript_metadata_unverified')
        self.bind()
        return True

def append_receipt(raw):
    def unique_pairs(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                refuse('transaction_receipt_unavailable')
            result[key] = value
        return result
    receipt = json.loads(raw, object_pairs_hook=unique_pairs)
    required = ('schemaVersion', 'status', 'createdCount', 'recoveryRequired', 'ownedFilesRolledBack', 'snapshotBindingVerified', 'privateSnapshotsPreserved', 'profileApplyEndpointImplemented', 'vendorResumeImplemented', 'distributedLeaseImplemented')
    strict_keys(receipt, required, ('createdCountBeforeRefusal', 'protectedBytesUnchanged'))
    if receipt['schemaVersion'] != 2 or receipt['status'] not in ('created_metadata', 'refused'):
        refuse('transaction_receipt_unavailable')
    for name in ('createdCount', 'createdCountBeforeRefusal'):
        if name in receipt and (type(receipt[name]) is not int or not 0 <= receipt[name] <= 200):
            refuse('transaction_receipt_unavailable')
    for name in required[3:]:
        if type(receipt[name]) is not bool:
            refuse('transaction_receipt_unavailable')
    if any(receipt[name] for name in ('profileApplyEndpointImplemented', 'vendorResumeImplemented', 'distributedLeaseImplemented')):
        refuse('transaction_receipt_unavailable')
    if receipt['status'] == 'created_metadata':
        if receipt['createdCount'] < 1 or receipt.get('protectedBytesUnchanged') is not True or receipt['recoveryRequired'] or not receipt['snapshotBindingVerified'] or not receipt['privateSnapshotsPreserved']:
            refuse('transaction_receipt_unavailable')
    elif receipt['createdCount'] != 0:
        refuse('transaction_receipt_unavailable')
    return receipt

def append(bound, request):
    expected, records = request.get('expectedTarget'), request.get('records')
    bound.unchanged(expected, records)
    transaction = request.get('transactionSource')
    if not isinstance(transaction, str) or sha(transaction.encode('utf-8')) != TRANSACTION_SHA:
        refuse('transaction_source_unverified')
    helper_source = globals().get('__ccs_history_helper_source__')
    bridge_source = globals().get('__ccs_history_node_bridge_source__')
    if not isinstance(helper_source, str) or not isinstance(bridge_source, str):
        refuse('packaged_bridge_unavailable')
    node = Path('/opt/homebrew/bin/node' if bound.platform == 'mac' else 'C:/Program Files/nodejs/node.exe')
    if not node.is_file():
        refuse('node_unavailable')
    registry, _, _ = bound.bind()
    _, buffers = bound.protection(registry)
    protected = [{'name': name, 'base64': base64.b64encode(buffers[name]).decode('ascii') if buffers[name] is not None else None} for name in ('config.json', 'ssh_configs.json', 'ssh-remote-server-state.json')]
    python = str(Path(sys.executable).resolve())
    python_hash = sha(read_regular(Path(python), limit=64 * 1024 * 1024))
    payload = {'schemaVersion': 2, 'profileId': bound.profile, 'platform': bound.platform, 'policy': bound.policy, 'expectedTarget': expected, 'profileRoot': str(bound.root), 'registryRoot': str(registry), 'protected': protected, 'records': records, 'transactionSource': transaction, 'guardSource': helper_source, 'guardSourceSha256': sha(helper_source.encode()), 'pythonExecutable': python, 'pythonExecutableSha256': python_hash}
    code = "const e=JSON.parse(require('node:fs').readFileSync(0,'utf8'));const m={exports:{}};const r=n=>require(n);r.main={};require('node:vm').runInThisContext('(function(require,module,exports){\\n'+e.bridgeSource+'\\n})')(r,m,m.exports);process.stdout.write(JSON.stringify(m.exports.runBridge(e.request)));"
    result = subprocess.run([str(node), '-e', code], input=encode({'bridgeSource': bridge_source, 'request': payload}), capture_output=True, timeout=25)
    if result.returncode != 0 or len(result.stdout) > 65536:
        refuse('transaction_receipt_unavailable')
    return append_receipt(result.stdout)

def dispatch(request):
    strict_keys(request, ('mode', 'profileId', 'platform', 'policy'), ('expectedEmail', 'expectedTarget', 'records', 'transactionSource'))
    mode, profile, platform = request['mode'], request['profileId'], request['platform']
    if mode not in MODES or profile not in PROFILES or platform not in PLATFORMS or (platform == 'mac') != (sys.platform == 'darwin') or (platform == 'windows') != (sys.platform == 'win32'):
        refuse('invalid_request')
    if 'transactionSource' in request and mode != 'append':
        refuse('invalid_request')
    if 'expectedEmail' in request:
        clean_text(request['expectedEmail'], 254)
    bound = BoundProfile(profile, platform, request['policy'])
    if mode == 'closed-check':
        bound.bind()
        return {'closed': bound.closed() is True}
    if mode == 'collect':
        return bound.collect()
    if mode == 'protected-check':
        return {'unchanged': bound.unchanged(request.get('expectedTarget'), request.get('records', []))}
    if mode == 'verify-transcripts':
        return {'verified': bound.verify_transcripts(request.get('records'))}
    if mode == 'snapshot-cleanup':
        return bound.snapshot_cleanup()
    return append(bound, request)

def main():
    request = None
    try:
        raw = sys.stdin.buffer.read(MAX_PIPE + 1)
        if not 0 < len(raw) <= MAX_PIPE:
            refuse('invalid_request')
        request = json.loads(raw)
        result = dispatch(request)
    except Exception:
        # No exception body, process argument, private path or private record escapes.
        mode = request.get('mode') if isinstance(request, dict) else None
        result = {'closed': False} if mode == 'closed-check' else {'unchanged': False} if mode == 'protected-check' else {'verified': False} if mode == 'verify-transcripts' else {'status': 'refused', 'reason': 'unavailable', 'createdCount': 0}
        if mode == 'append':
            result['recoveryRequired'] = True
    if isinstance(request, dict) and request.get('mode') == 'append':
        # All paths reach this point only after subprocess.run has returned or
        # killed AND waited for the sole mutating Node child on its timeout.
        # An outer SSH timeout/lost reply carries no such acknowledgement.
        result['writerQuiescent'] = True
    sys.stdout.buffer.write(encode(result))

if __name__ == '__main__':
    main()
