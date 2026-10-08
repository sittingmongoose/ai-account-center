import { execFile, spawn, type ChildProcess } from 'child_process';
import { createHash } from 'crypto';
import fs from 'fs';
import path from 'path';
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
      ...process.env,
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
        env,
      });
    } catch {
      reject(new Error('App update host failed.'));
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
    const settle = (): void => {
      if (settled) return;
      settled = true;
      if (forceKill) clearTimeout(forceKill);
      if (final !== null) resolve(final);
      else reject(new Error('App update host failed.'));
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
    child.on('error', settle);
    child.on('close', () => {
      consume(pending);
      pending = '';
      settle();
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
// macOS ships mkdir and chmod in /bin and tar in /usr/bin. (/usr/bin/chmod
// does not exist there: the chain stopped before tar and every Mac run used
// the stale Oct-2 helpers.) Ubuntu's merged /usr has the same three paths, so
// the Mac and Nas1 share this extract. Each path is asserted in the tests.
export const POSIX_EXTRACT =
  '/bin/mkdir -p "$HOME/.ccs/app-updates" && /bin/chmod 700 "$HOME/.ccs/app-updates" && /usr/bin/tar -x -f - -C "$HOME/.ccs/app-updates"';
/** The extract's name before Nas1 shared it; kept for the Mac sync tests. */
export const MAC_EXTRACT = POSIX_EXTRACT;
// The Windows sshd runs cmd.exe, so the extract script must travel as an
// encoded powershell command; tar.exe reads the archive from the ssh stdin.
const WINDOWS_EXTRACT = `powershell.exe -NoProfile -NonInteractive -EncodedCommand ${Buffer.from(
  "$ErrorActionPreference='Stop'; $d=[IO.Path]::Combine($HOME,'.ccs','app-updates'); " +
    'New-Item -ItemType Directory -Force -Path $d | Out-Null; tar.exe -x -f - -C $d; exit $LASTEXITCODE',
  'utf16le'
).toString('base64')}`;

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

const WINDOWS_HASH_QUERY = `powershell.exe -NoProfile -NonInteractive -EncodedCommand ${Buffer.from(
  "$ErrorActionPreference='SilentlyContinue'; $d=[IO.Path]::Combine($HOME,'.ccs','app-updates'); " +
    `foreach($n in @(${HELPER_FILES.map((name) => `'${name}'`).join(',')})){ ` +
    '$p=[IO.Path]::Combine($d,$n); if(Test-Path -LiteralPath $p -PathType Leaf){ ' +
    "$h=(Get-FileHash -LiteralPath $p -Algorithm SHA256).Hash; if($h){ Write-Output ($h.ToLowerInvariant()+'  '+$n) } } }; exit 0",
  'utf16le'
).toString('base64')}`;

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
      reject(error);
    };
    archive.on('error', fail);
    remote.on('error', fail);
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
 * missing file prints nothing). macOS ships shasum, Ubuntu (Nas1) ships
 * coreutils' sha256sum, and Windows hashes through PowerShell.
 */
export function helperHashQuery(platform: RemotePlatform): string {
  if (HOST_OS[platform] === 'windows') return WINDOWS_HASH_QUERY;
  const tool = HOST_OS[platform] === 'mac' ? '/usr/bin/shasum -a 256' : '/usr/bin/sha256sum';
  return `${tool} ${HELPER_FILES.map((name) => `"$HOME/.ccs/app-updates/${name}"`).join(' ')} 2>/dev/null; exit 0`;
}

/** The fixed command that unpacks the pushed helper archive on a remote host. */
export function helperExtract(platform: RemotePlatform): string {
  return HOST_OS[platform] === 'windows' ? WINDOWS_EXTRACT : POSIX_EXTRACT;
}

/**
 * Aligns a remote host's deployed helpers with this build before invoking it.
 * Hosts (Mac, Windows and Nas1) keep their own copies under ~/.ccs/app-updates;
 * a stale copy would silently run old updater logic no matter what the server
 * ships. Checksum-gated, so an up-to-date host pays one hash query. Best
 * effort: a sync failure leaves the deployed helpers untouched and the run proceeds.
 */
export async function syncRemoteHelpers(platform: UpdatePlatform): Promise<void> {
  if (platform === 'ubuntu') return;
  const local = localHelperChecksums();
  const host = APP_UPDATE_SSH_HOSTS[platform];
  let deployed = '';
  try {
    deployed = await sshText(host, helperHashQuery(platform));
  } catch {
    /* An unreachable host is reported by the run itself; treat it as stale. */
  }
  const remote = parseDeployedChecksums(deployed);
  if (HELPER_FILES.every((name) => remote[name] === local[name])) return;
  await pushHelpers(host, helperExtract(platform));
}
