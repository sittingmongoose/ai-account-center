import { execFile, spawn, type ChildProcess } from 'child_process';
import { createHash } from 'crypto';
import fs from 'fs';
import path from 'path';
import { deflateSync } from 'zlib';
import { BinaryError } from '../../errors/error-types';
import { getCcsDir } from '../../utils/config-manager';
import { MAX_OUTPUT, record, type UpdatePlatform } from './app-update-contract';
import { defaultNativeReleaseFile, readNativeRelease } from '../../antigravity/native-version';
import { HOST_OS, NAS1_SSH_ALIAS, helperPlatform } from './dashboard-hosts';

/**
 * How the dashboard reaches each computer's fixed update helper: the local
 * Ubuntu run, ssh to the fixed Mac, Windows and Nas1 aliases below, live
 * progress streaming and the checksum-gated helper sync. No configurable
 * hosts, commands or paths.
 */

/** The remote computers: every update computer except the local Ubuntu one. */
type RemotePlatform = Exclude<UpdatePlatform, 'ubuntu'>;

/** The fixed ssh aliases of the Mac, Windows and Nas1 computers. */
export const APP_UPDATE_SSH_HOSTS: Readonly<Record<RemotePlatform, string>> = Object.freeze({
  mac: 'jared-mac',
  windows: 'jared-windows',
  nas1: NAS1_SSH_ALIAS,
});

/** Total progress output accepted from one host; a runaway helper is stopped. */
const MAX_STREAM = 1024 * 1024;

/**
 * The Antigravity CLI versions that passed a switching review, comma-joined
 * for the helper's --agy-reviewed (it holds any newer build). Empty when the
 * packaged release file is unusable: the helper then holds the update.
 * Versions are strictly validated (digits, dots, one -tag), so they are inert
 * on every command line below.
 */
export function antigravityReviewedArgument(releaseFile = defaultNativeReleaseFile()): string {
  return readNativeRelease(releaseFile)
    .reviewed.map((entry) => entry.version)
    .join(',');
}

export function appUpdateInvocation(
  platform: UpdatePlatform,
  reviewed = antigravityReviewedArgument(),
  stateDirectory = path.join(getCcsDir(), 'app-updates')
): { binary: string; args: string[] } {
  const local = path.resolve(__dirname, '../../../scripts/app-updates/app_updates.py');
  const review = /^[0-9A-Za-z_.,-]{1,4096}$/.test(reviewed) ? reviewed : '';
  if (platform === 'ubuntu')
    return {
      binary: '/usr/bin/python3',
      args: [
        local,
        '--apply',
        '--platform',
        'ubuntu',
        '--state-dir',
        path.resolve(stateDirectory),
        '--dashboard-job',
        ...(review ? ['--agy-reviewed', review] : []),
      ],
    };
  // Each remote host has its own alias and its own --platform; the helper checks
  // that value against its native OS, so Nas1 (a second Ubuntu) passes 'ubuntu'.
  // The remote helper keeps its state in its own ~/.ccs/app-updates: no
  // --state-dir or --dashboard-job, which belong to the local dashboard job.
  const host = APP_UPDATE_SSH_HOSTS[platform];
  const helperOs = helperPlatform(platform);
  const reviewArgument = review ? ` --agy-reviewed '${review}'` : '';
  // AAC_UPDATE_PROGRESS asks the helper for line-by-line progress; a helper
  // that predates it ignores the variable and prints one final document.
  let command = `AAC_UPDATE_PROGRESS=1 /usr/bin/python3 "$HOME/.ccs/app-updates/app_updates.py" --apply --platform ${helperOs}${reviewArgument}`;
  if (HOST_OS[platform] === 'windows') {
    const script = [
      "$ErrorActionPreference='Stop'",
      "$env:PYTHONUTF8='1'",
      "$env:PYTHONIOENCODING='utf-8'",
      "$env:AAC_UPDATE_PROGRESS='1'",
      "$helper=[IO.Path]::Combine($HOME,'.ccs','app-updates','app_updates.py')",
      `& python.exe $helper --apply --platform ${helperOs}${reviewArgument}`,
      'exit $LASTEXITCODE',
    ].join('; ');
    command = `powershell.exe -NoProfile -NonInteractive -EncodedCommand ${Buffer.from(script, 'utf16le').toString('base64')}`;
  }
  return {
    binary: 'ssh',
    args: [
      '-T',
      '-o',
      'BatchMode=yes',
      '-o',
      'ConnectTimeout=5',
      '-o',
      'ConnectionAttempts=1',
      '-o',
      'ServerAliveInterval=10',
      '-o',
      'ServerAliveCountMax=2',
      '--',
      host,
      command,
    ],
  };
}

/**
 * How the service follows one host's helper: live progress events in, a cancel
 * and a stop out. A runner that has neither (a test double) simply ignores it.
 */
export interface HostRunControl {
  /** Resolved directory holding this dashboard's job and exclusive lock. */
  stateDirectory?: string;
  /** One parsed progress line ({"event": "app"|"result", ...}) from the helper. */
  onEvent(event: Record<string, unknown>): void;
  /** Registers how to forward a cancel to the running helper. */
  setCancel(handler: () => void): void;
  /** Registers how to stop the host process once its deadline passes. */
  setAbort(handler: () => void): void;
}

/**
 * Runs one host's fixed helper and streams its progress. Resolves with the
 * final {"results": [...]} line; rejects when the host never produced one.
 * stdin stays open so a "cancel" line can reach the helper (over ssh too).
 */
export function runHost(platform: UpdatePlatform, control?: HostRunControl): Promise<string> {
  const command = appUpdateInvocation(platform, undefined, control?.stateDirectory);
  return runHelperProcess(
    command.binary,
    command.args,
    {
      ...updaterEnvironment(process.env),
      PYTHONUTF8: '1',
      PYTHONIOENCODING: 'utf-8',
      AAC_UPDATE_PROGRESS: '1',
      MUSE_NO_AUTO_UPDATE: '1',
      AGY_CLI_DISABLE_AUTO_UPDATE: 'true',
      DISABLE_AUTOUPDATER: '1',
    },
    control
  );
}

export function updaterEnvironment(env: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  return Object.fromEntries(
    Object.entries(env).filter(([key]) => {
      const name = key.toUpperCase();
      return (
        name !== 'CCS_SESSION_SECRET' &&
        !name.startsWith('CCS_DASHBOARD_') &&
        !name.endsWith('_PASSWORD_HASH')
      );
    })
  );
}

export class HelperExecutionError extends Error {
  constructor(readonly code: 'host_unknown' | 'helper_invalid') {
    super(
      code === 'host_unknown'
        ? 'App update transport failed.'
        : 'The helper returned no final result.'
    );
  }
}

/** Follows one helper process line by line (exported for the fake-helper tests). */
export function runHelperProcess(
  binary: string,
  args: string[],
  env: NodeJS.ProcessEnv,
  control?: HostRunControl
): Promise<string> {
  return new Promise((resolve, reject) => {
    let child: ChildProcess;
    try {
      child = spawn(binary, args, {
        stdio: ['pipe', 'pipe', 'ignore'],
        windowsHide: true,
        env: updaterEnvironment(env),
      });
    } catch {
      reject(new HelperExecutionError(binary === 'ssh' ? 'host_unknown' : 'helper_invalid'));
      return;
    }
    let pending = '';
    let received = 0;
    let final: string | null = null;
    let settled = false;
    let forceKill: ReturnType<typeof setTimeout> | undefined;
    const consume = (line: string): void => {
      const text = line.trim();
      if (!text || text.length > MAX_OUTPUT) return;
      let value: Record<string, unknown> | undefined;
      try {
        value = record(JSON.parse(text));
      } catch {
        return;
      }
      if (!value) return;
      if (typeof value.event === 'string') {
        try {
          control?.onEvent(value);
        } catch {
          /* Progress is display-only; the final document still decides. */
        }
      } else if (Array.isArray(value.results)) final = text;
    };
    const settle = (transportFailed = false): void => {
      if (settled) return;
      settled = true;
      if (forceKill) clearTimeout(forceKill);
      if (final !== null) resolve(final);
      else
        reject(
          new HelperExecutionError(
            transportFailed && binary === 'ssh' ? 'host_unknown' : 'helper_invalid'
          )
        );
    };
    child.stdout?.setEncoding('utf8');
    child.stdout?.on('data', (chunk: string) => {
      received += chunk.length;
      if (received > MAX_STREAM) {
        child.kill('SIGKILL');
        return;
      }
      pending += chunk;
      for (let index = pending.indexOf('\n'); index >= 0; index = pending.indexOf('\n')) {
        consume(pending.slice(0, index));
        pending = pending.slice(index + 1);
      }
      if (pending.length > MAX_OUTPUT) pending = '';
    });
    child.stdin?.on('error', () => {});
    child.on('error', () => settle(true));
    child.on('close', (code) => {
      consume(pending);
      pending = '';
      settle(code === 255);
    });
    control?.setCancel(() => {
      if (child.stdin && !child.stdin.destroyed) child.stdin.write('cancel\n');
    });
    control?.setAbort(() => {
      child.kill('SIGTERM');
      forceKill = setTimeout(() => child.kill('SIGKILL'), 5000);
      forceKill.unref?.();
    });
  });
}

/**
 * The Codex stop/start runtime app_update_codex.cjs loads on a Linux host
 * without AAC. Generated into dist/app-updates by bun run build:server.
 */
const CODEX_RUNTIME_HELPER = 'app_update_codex_runtime.cjs';
/** The helper files every remote host must run from ~/.ccs/app-updates. */
export const HELPER_FILES = [
  'app_updates.py',
  'app_update_common.py',
  'app_update_desktop.py',
  'app_update_t3.py',
  'app_update_processes.py',
  'app_update_terminal.py',
  'app_update_terminal_child.py',
  'app_update_pipe.py',
  'app_update_probe.py',
  'app_update_confirmed_codex.py',
  'app_update_zcode.py',
  'app_update_codex.cjs',
  CODEX_RUNTIME_HELPER,
] as const;
/** The package this build runs from (the checkout under tests). */
const PACKAGE_ROOT = path.resolve(__dirname, '../../..');

/**
 * Where each helper lives in this build: the source helpers in
 * scripts/app-updates, the generated runtime in dist/app-updates. A missing
 * file fails the sync instead of deploying a partial set.
 */
function localHelperFolders(root: string): Array<{ directory: string; names: string[] }> {
  const folders = [
    {
      directory: path.join(root, 'scripts', 'app-updates'),
      names: HELPER_FILES.filter((name) => name !== CODEX_RUNTIME_HELPER),
    },
    { directory: path.join(root, 'dist', 'app-updates'), names: [CODEX_RUNTIME_HELPER] },
  ];
  for (const { directory, names } of folders)
    for (const name of names)
      if (!fs.existsSync(path.join(directory, name)))
        throw new BinaryError(
          name === CODEX_RUNTIME_HELPER
            ? 'The Codex update runtime is not built; run bun run build:server.'
            : `The app update helper ${name} is missing.`,
          path.join(directory, name)
        );
  return folders;
}

/** tar arguments for one flat archive of every helper (one -C per folder). */
export function helperArchiveArgs(root = PACKAGE_ROOT): string[] {
  return [
    '-c',
    '-f',
    '-',
    ...localHelperFolders(root).flatMap(({ directory, names }) => ['-C', directory, ...names]),
  ];
}
const HELPER_QUERY_TIMEOUT_MS = 30_000;
const HELPER_PUSH_TIMEOUT_MS = 90_000;
const SSH_SYNC_OPTIONS = [
  '-T',
  '-o',
  'BatchMode=yes',
  '-o',
  'ConnectTimeout=5',
  '-o',
  'ConnectionAttempts=1',
];
/** Stage a complete immutable generation, then atomically replace only its launcher. */
export function helperPublishScript(checksums: Record<string, string>): string {
  return `import hashlib, json, os, pathlib, re, runpy, shutil, subprocess, sys, tarfile, tempfile, uuid
root = pathlib.Path.home() / '.ccs/app-updates'
root.mkdir(parents=True, exist_ok=True, mode=0o700)
expected = json.loads(${JSON.stringify(JSON.stringify(checksums))})
if set(expected) != set(${JSON.stringify(HELPER_FILES)}) or not all(re.fullmatch('[a-f0-9]{64}', value) for value in expected.values()):
    raise ValueError('Incomplete helper manifest')
stage = pathlib.Path(tempfile.mkdtemp(prefix='.helpers-stage-', dir=root))
launcher = root / ('.launcher-' + uuid.uuid4().hex)
try:
    if os.name == 'nt':
        # Python 3.13 creates mode-0700 directories for the elevated token's
        # owner (Administrators). The Limited interactive task needs the real
        # user's SID on this directory and every extracted helper instead.
        sid = subprocess.check_output(['powershell.exe', '-NoProfile', '-NonInteractive', '-Command', '[Security.Principal.WindowsIdentity]::GetCurrent().User.Value'], stdin=subprocess.DEVNULL, text=True, encoding='ascii', timeout=15, creationflags=0x08000000).strip()
        if not re.fullmatch(r'S-1-\\d+(?:-\\d+)+', sid):
            raise ValueError('Invalid helper user identity')
        for folder in (root, stage):
            subprocess.run(['icacls.exe', str(folder), '/grant:r', '*' + sid + ':(OI)(CI)F'], check=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15, creationflags=0x08000000)
    with tarfile.open(fileobj=sys.stdin.buffer, mode='r|') as archive:
        seen = set()
        for entry in archive:
            if entry.name not in expected or entry.name in seen or not entry.isfile() or entry.size > 16 * 1024 * 1024:
                raise ValueError('Invalid helper archive')
            seen.add(entry.name)
            with archive.extractfile(entry) as source:
                (stage / entry.name).write_bytes(source.read())
    if seen != set(expected) or any(hashlib.sha256((stage / name).read_bytes()).hexdigest() != digest for name, digest in expected.items()):
        raise ValueError('Helper checksum mismatch')
    generation = 'helpers-' + uuid.uuid4().hex
    os.rename(stage, root / generation)
    launcher.write_text('# aac-helpers: ' + generation + '\\n# aac-helpers-format: 2\\nimport pathlib, runpy, sys\\nroot = pathlib.Path(__file__).resolve().parent / ' + repr(generation) + '\\nsys.path.insert(0, str(root))\\nrunpy.run_path(str(root / "app_updates.py"), run_name="__main__")\\n', encoding='utf-8')
    launcher.chmod(0o600)
    os.replace(launcher, root / 'app_updates.py')
finally:
    shutil.rmtree(stage, ignore_errors=True)
    launcher.unlink(missing_ok=True)
`;
}

function pythonCommand(platform: RemotePlatform, script: string): string {
  // The script travels as its own base64 argument: no quote inside the code, because Windows
  // PowerShell strips embedded double quotes when it calls a native program.
  // Windows OpenSSH defaults to cmd.exe, whose 8191-character limit also covers
  // the outer UTF-16 PowerShell encoding. Compress before either encoding so a
  // complete checksum manifest fits; stdin stays available for the binary tar.
  const windows = HOST_OS[platform] === 'windows';
  const contents = Buffer.from(script);
  const encoded = (windows ? deflateSync(contents) : contents).toString('base64');
  const code = windows
    ? 'import base64,zlib,sys;exec(zlib.decompress(base64.b64decode(sys.argv[1])))'
    : 'import base64,sys;exec(base64.b64decode(sys.argv[1]))';
  if (!windows) return `/usr/bin/python3 -c '${code}' ${encoded}`;
  const ps = `& python.exe -c '${code}' ${encoded}; exit $LASTEXITCODE`;
  return `powershell.exe -NoProfile -NonInteractive -EncodedCommand ${Buffer.from(ps, 'utf16le').toString('base64')}`;
}
export const POSIX_EXTRACT = pythonCommand('mac', helperPublishScript({}));
export const MAC_EXTRACT = POSIX_EXTRACT;

/** Parses "<sha256>  <name-or-path>" lines; keyed by basename, hex lowercased. */
export function parseDeployedChecksums(output: string): Record<string, string> {
  const values: Record<string, string> = {};
  for (const line of output.split('\n')) {
    const parts = line.trim().split(/\s+/);
    if (parts.length < 2 || !/^[a-fA-F0-9]{64}$/.test(parts[0])) continue;
    const name = parts[parts.length - 1].split(/[\\/]/).pop();
    if (name) values[name] = parts[0].toLowerCase();
  }
  return values;
}

export function localHelperChecksums(root = PACKAGE_ROOT): Record<string, string> {
  const values: Record<string, string> = {};
  for (const { directory, names } of localHelperFolders(root))
    for (const name of names)
      values[name] = createHash('sha256')
        .update(fs.readFileSync(path.join(directory, name)))
        .digest('hex');
  return values;
}

function sshText(host: string, command: string): Promise<string> {
  return new Promise((resolve, reject) => {
    execFile(
      'ssh',
      [...SSH_SYNC_OPTIONS, '--', host, command],
      {
        encoding: 'utf8',
        timeout: HELPER_QUERY_TIMEOUT_MS,
        maxBuffer: 1024 * 1024,
        windowsHide: true,
      },
      (error, stdout) => (error ? reject(error) : resolve(stdout))
    );
  });
}

function pushHelpers(host: string, extract: string, root = PACKAGE_ROOT): Promise<void> {
  // lib is ES2020, so the executor form is the available API here (as in runHost).
  return new Promise((resolve, reject) => {
    let args: string[];
    try {
      args = helperArchiveArgs(root);
    } catch (error) {
      reject(error);
      return;
    }
    const archive = spawn('tar', args, {
      stdio: ['ignore', 'pipe', 'ignore'],
      windowsHide: true,
    });
    const remote = spawn('ssh', [...SSH_SYNC_OPTIONS, '--', host, extract], {
      stdio: ['pipe', 'ignore', 'ignore'],
      windowsHide: true,
    });
    const timer = setTimeout(() => {
      archive.kill('SIGKILL');
      remote.kill('SIGKILL');
      reject(new Error('Helper sync timed out.'));
    }, HELPER_PUSH_TIMEOUT_MS);
    const fail = (error: Error): void => {
      clearTimeout(timer);
      archive.kill('SIGKILL');
      remote.kill('SIGKILL');
      reject(error);
    };
    archive.on('error', fail);
    remote.on('error', fail);
    remote.stdin.on('error', fail);
    archive.stdout.on('error', fail);
    // Both ends must succeed: a tar that stopped early sent a partial set.
    let open = 2;
    let failed = false;
    const closed = (code: number | null): void => {
      if (code !== 0) failed = true;
      if (--open > 0) return;
      clearTimeout(timer);
      if (failed) reject(new Error('Helper sync failed.'));
      else resolve();
    };
    archive.on('close', closed);
    remote.on('close', closed);
    archive.stdout.pipe(remote.stdin);
  });
}

/**
 * The fixed command that prints "<sha256>  <path>" for each deployed helper (a
 * missing file prints nothing). Python follows the selected helper generation
 * on every remote host, including Windows through encoded PowerShell.
 */
export function helperHashQuery(platform: RemotePlatform): string {
  return pythonCommand(
    platform,
    `import hashlib, pathlib, re
root = pathlib.Path.home() / '.ccs/app-updates'
try:
    with (root / 'app_updates.py').open(encoding='utf-8') as launcher:
        first = launcher.readline(128).strip()
        format_marker = launcher.readline(128).strip()
    # Identical helper bytes do not prove an old Windows generation is
    # readable by the Limited task. Republish once with the explicit user ACL.
    if ${HOST_OS[platform] === 'windows' ? 'True' : 'False'} and format_marker != '# aac-helpers-format: 2':
        raise SystemExit(0)
    if re.fullmatch(r'# aac-helpers: helpers-[a-f0-9]{32}', first):
        root = root / first.split()[-1]
except OSError:
    pass
for name in ${JSON.stringify(HELPER_FILES)}:
    try:
        print(hashlib.sha256((root / name).read_bytes()).hexdigest() + '  ' + name)
    except OSError:
        pass
`
  );
}

/** The fixed command that verifies and publishes the pushed helper archive. */
export function helperExtract(
  platform: RemotePlatform,
  checksums: Record<string, string> = {}
): string {
  return pythonCommand(platform, helperPublishScript(checksums));
}

/**
 * Aligns a remote host's deployed helpers with this build before invoking it.
 * Hosts (Mac, Windows and Nas1) keep their own copies under ~/.ccs/app-updates;
 * a stale copy would silently run old updater logic no matter what the server
 * ships. An up-to-date host pays one hash query. A failed sync leaves the
 * previous generation intact and prevents this host run.
 */
export async function syncRemoteHelpers(platform: UpdatePlatform): Promise<void> {
  if (platform === 'ubuntu') return;
  const local = localHelperChecksums();
  const host = APP_UPDATE_SSH_HOSTS[platform];
  let deployed = '';
  try {
    deployed = await sshText(host, helperHashQuery(platform));
  } catch {
    /* Retry through a verified push; if it fails this host never runs. */
  }
  const remote = parseDeployedChecksums(deployed);
  if (HELPER_FILES.every((name) => remote[name] === local[name])) return;
  await pushHelpers(host, helperExtract(platform, local));
}
