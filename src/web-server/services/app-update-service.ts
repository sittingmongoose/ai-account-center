import { execFile, spawn } from 'child_process';
import { createHash, randomUUID } from 'crypto';
import fs from 'fs';
import path from 'path';
import { getCcsDir } from '../../utils/config-manager';
import { ConfigError } from '../../errors/error-types';

import {
  EXPECTED_RESULTS,
  MESSAGES,
  PLATFORMS,
  MAX_OUTPUT,
  UPDATE_APP_LABELS,
  record,
  skipped,
  unknown,
  normalizeAppUpdateResults,
  type UpdateAppId,
  type UpdatePlatform,
  type AppUpdateResult,
  type AppUpdateJob,
} from './app-update-contract';
export { UPDATE_APP_LABELS, normalizeAppUpdateResults } from './app-update-contract';
export type {
  UpdateAppId,
  UpdatePlatform,
  UpdateResultStatus,
  AppUpdateResult,
  AppUpdateJob,
} from './app-update-contract';
const MAX_HOST_DURATION_MS = 20 * 60 * 1000;

export class AppUpdateBusyError extends Error {
  constructor() {
    super('An app update is already running.');
    this.name = 'AppUpdateBusyError';
  }
}

export function appUpdateInvocation(platform: UpdatePlatform): { binary: string; args: string[] } {
  const local = path.resolve(__dirname, '../../../scripts/app-updates/app_updates.py');
  if (platform === 'ubuntu')
    return { binary: '/usr/bin/python3', args: [local, '--apply', '--platform', 'ubuntu'] };
  const host = platform === 'mac' ? 'jared-mac' : 'jared-windows';
  let command = '/usr/bin/python3 "$HOME/.ccs/app-updates/app_updates.py" --apply --platform mac';
  if (platform === 'windows') {
    const script = [
      "$ErrorActionPreference='Stop'",
      "$env:PYTHONUTF8='1'",
      "$env:PYTHONIOENCODING='utf-8'",
      "$helper=[IO.Path]::Combine($HOME,'.ccs','app-updates','app_updates.py')",
      '& python.exe $helper --apply --platform windows',
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

function runHost(platform: UpdatePlatform): Promise<string> {
  const command = appUpdateInvocation(platform);
  return new Promise((resolve, reject) => {
    execFile(
      command.binary,
      command.args,
      {
        encoding: 'utf8',
        timeout: MAX_HOST_DURATION_MS,
        maxBuffer: MAX_OUTPUT,
        windowsHide: true,
        env: {
          ...process.env,
          PYTHONUTF8: '1',
          PYTHONIOENCODING: 'utf-8',
          MUSE_NO_AUTO_UPDATE: '1',
          AGY_CLI_DISABLE_AUTO_UPDATE: 'true',
          DISABLE_AUTOUPDATER: '1',
        },
      },
      (error, stdout) => (error ? reject(new Error('App update host failed.')) : resolve(stdout))
    );
  });
}

/** The helper files every remote host must run from ~/.ccs/app-updates. */
const HELPER_FILES = [
  'app_updates.py',
  'app_update_common.py',
  'app_update_desktop.py',
  'app_update_processes.py',
  'app_update_terminal.py',
  'app_update_terminal_child.py',
  'app_update_pipe.py',
  'app_update_probe.py',
  'app_update_confirmed_codex.py',
  'app_update_codex.cjs',
] as const;
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
const MAC_EXTRACT =
  '/bin/mkdir -p "$HOME/.ccs/app-updates" && /usr/bin/chmod 700 "$HOME/.ccs/app-updates" && /usr/bin/tar -x -f - -C "$HOME/.ccs/app-updates"';
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

function localHelperChecksums(): Record<string, string> {
  const source = path.resolve(__dirname, '../../../scripts/app-updates');
  const values: Record<string, string> = {};
  for (const name of HELPER_FILES)
    values[name] = createHash('sha256')
      .update(fs.readFileSync(path.join(source, name)))
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

function pushHelpers(host: string, extract: string): Promise<void> {
  // lib is ES2020, so the executor form is the available API here (as in runHost).
  return new Promise((resolve, reject) => {
    const archive = spawn(
      'tar',
      [
        '-c',
        '-f',
        '-',
        '-C',
        path.resolve(__dirname, '../../../scripts/app-updates'),
        ...HELPER_FILES,
      ],
      { stdio: ['ignore', 'pipe', 'ignore'], windowsHide: true }
    );
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
    remote.on('close', (code) => {
      clearTimeout(timer);
      if (code === 0) resolve();
      else reject(new Error('Helper sync failed.'));
    });
    archive.stdout.pipe(remote.stdin);
  });
}

/**
 * Aligns a remote host's deployed helpers with this build before invoking it.
 * Hosts keep their own copies under ~/.ccs/app-updates; a stale copy would
 * silently run old updater logic no matter what the server ships. Checksum-
 * gated, so an up-to-date host pays one hash query. Best effort: a sync
 * failure leaves the deployed helpers untouched and the run proceeds.
 */
export async function syncRemoteHelpers(platform: UpdatePlatform): Promise<void> {
  if (platform === 'ubuntu') return;
  const local = localHelperChecksums();
  const host = platform === 'mac' ? 'jared-mac' : 'jared-windows';
  let deployed = '';
  try {
    deployed =
      platform === 'mac'
        ? await sshText(
            host,
            `/usr/bin/shasum -a 256 ${HELPER_FILES.map((name) => `"$HOME/.ccs/app-updates/${name}"`).join(' ')} 2>/dev/null; exit 0`
          )
        : await sshText(host, WINDOWS_HASH_QUERY);
  } catch {
    /* An unreachable host is reported by the run itself; treat it as stale. */
  }
  const remote = parseDeployedChecksums(deployed);
  if (HELPER_FILES.every((name) => remote[name] === local[name])) return;
  await pushHelpers(host, platform === 'mac' ? MAC_EXTRACT : WINDOWS_EXTRACT);
}

export interface AppUpdateDependencies {
  runHost(platform: UpdatePlatform): Promise<string>;
  /** Aligns deployed remote helpers with this build; production wires the real sync. */
  sync(platform: UpdatePlatform): Promise<void>;
  now(): number;
  id(): string;
  ccsDir?: string;
  persist?: boolean;
}

/** Starts work only in response to the explicit authenticated POST action. */
export class AppUpdateService {
  private job: AppUpdateJob | null = null;
  private readonly deps: AppUpdateDependencies;
  private readonly directory: string;
  private lockHeld = false;

  constructor(overrides: Partial<AppUpdateDependencies> = {}) {
    this.deps = {
      runHost,
      sync: async () => {},
      now: Date.now,
      id: randomUUID,
      persist: true,
      ...overrides,
    };
    this.directory = path.join(path.resolve(this.deps.ccsDir ?? getCcsDir()), 'app-updates');
    this.restore();
  }

  getStatus(): { job: AppUpdateJob | null } {
    if (!this.lockHeld) this.restore();
    return { job: this.job ? structuredClone(this.job) : null };
  }

  start(): { job: AppUpdateJob } {
    if (!this.lockHeld) this.restore();
    if (this.job?.state === 'running') throw new AppUpdateBusyError();
    this.acquireLock();
    this.job = {
      id: this.deps.id(),
      state: 'running',
      startedAt: new Date(this.deps.now()).toISOString(),
      finishedAt: null,
      activePlatform: null,
      cancelRequested: false,
      results: [],
      expectedResults: EXPECTED_RESULTS,
    };
    try {
      this.save();
    } catch (error) {
      this.job = null;
      this.releaseLock();
      throw error;
    }
    const response = this.getStatus().job as AppUpdateJob;
    void this.execute();
    return { job: response };
  }

  /**
   * Acknowledges a cancel at once. The host batch running now is never killed;
   * it finishes, then every queued app is reported as skipped. Idempotent:
   * a second cancel changes nothing. Never promises an undo.
   */
  cancel(): { job: AppUpdateJob | null; cancelling: boolean; notOwner?: true } {
    if (!this.lockHeld) this.restore();
    if (this.job?.state !== 'running') return { job: this.getStatus().job, cancelling: false };
    // A running job this process restored from disk belongs to another dashboard process (a deploy overlap): its
    // flag would never reach the owner's loop, and the owner's next save would erase it. Refuse instead of
    // acknowledging a cancel that cannot be honoured.
    if (this.deps.persist !== false && !this.lockHeld) {
      return { job: this.getStatus().job, cancelling: false, notOwner: true };
    }
    this.job.cancelRequested = true;
    try {
      this.save();
    } catch {
      /* The in-memory flag still stops queued work; persistence is display-only. */
    }
    return { job: this.getStatus().job, cancelling: true };
  }

  private async execute(): Promise<void> {
    if (!this.job) return;
    try {
      for (const platform of PLATFORMS) {
        if (this.job.cancelRequested) {
          this.job.results.push(
            ...(Object.keys(UPDATE_APP_LABELS) as UpdateAppId[]).map((id) => skipped(platform, id))
          );
          this.save();
          continue;
        }
        this.job.activePlatform = platform;
        this.save();
        // Ubuntu runs the helpers inside this installed build; only the remote
        // hosts carry deployed copies that can go stale. Skipping the await
        // also keeps start() launching the first host synchronously.
        if (platform !== 'ubuntu') {
          try {
            await this.deps.sync(platform);
          } catch {
            /* Best effort: the deployed helpers stay as they are. */
          }
        }
        try {
          const output = await this.deps.runHost(platform);
          this.job.results.push(...normalizeAppUpdateResults(output, platform));
        } catch {
          // The host never answered, so no check ran: unknown, not failed.
          this.job.results.push(
            ...(Object.keys(UPDATE_APP_LABELS) as UpdateAppId[]).map((id) =>
              unknown(platform, id, 'host_unknown')
            )
          );
        }
        this.save();
      }
      this.job.state = this.job.results.some(
        (result) =>
          result.status === 'failed' ||
          result.status === 'restart_failed' ||
          result.status === 'unknown'
      )
        ? 'failed'
        : 'completed';
    } catch {
      this.job.state = 'failed';
    } finally {
      // A cancel that landed after the last app changed no result; leave no trace.
      if (!this.job.results.some((result) => result.status === 'skipped'))
        this.job.cancelRequested = false;
      this.job.activePlatform = null;
      this.job.finishedAt = new Date(this.deps.now()).toISOString();
      try {
        this.save();
      } catch {
        /* Never expose filesystem errors to clients. */
      }
      this.releaseLock();
    }
  }

  private acquireLock(): void {
    if (this.deps.persist === false) return;
    fs.mkdirSync(this.directory, { recursive: true, mode: 0o700 });
    const lock = path.join(this.directory, 'dashboard-update.lock');
    const create = (): void => {
      const descriptor = fs.openSync(lock, 'wx', 0o600);
      try {
        fs.writeFileSync(descriptor, JSON.stringify({ pid: process.pid }));
      } finally {
        fs.closeSync(descriptor);
      }
      this.lockHeld = true;
    };
    try {
      create();
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'EEXIST') {
        const observed = fs.statSync(lock);
        if (this.lockOwnerAlive()) throw new AppUpdateBusyError();
        try {
          const current = fs.statSync(lock);
          if (current.dev !== observed.dev || current.ino !== observed.ino)
            throw new AppUpdateBusyError();
          fs.unlinkSync(lock);
          create();
          return;
        } catch {
          throw new AppUpdateBusyError();
        }
      }
      throw new ConfigError('App update state could not be saved safely.');
    }
  }

  private releaseLock(): void {
    if (!this.lockHeld) return;
    this.lockHeld = false;
    try {
      fs.unlinkSync(path.join(this.directory, 'dashboard-update.lock'));
    } catch {
      /* The helper has its own execution lock too. */
    }
  }

  private save(): void {
    if (this.deps.persist === false) return;
    const target = path.join(this.directory, 'dashboard-job.json');
    const temporary = path.join(this.directory, `dashboard-job.${process.pid}.tmp`);
    fs.writeFileSync(temporary, JSON.stringify({ job: this.job }), { mode: 0o600 });
    fs.renameSync(temporary, target);
    fs.chmodSync(target, 0o600);
  }

  private lockOwnerAlive(): boolean {
    try {
      const file = path.join(this.directory, 'dashboard-update.lock');
      if (fs.statSync(file).size > 128) return true;
      const owner = record(JSON.parse(fs.readFileSync(file, 'utf8')))?.pid;
      if (typeof owner !== 'number' || !Number.isSafeInteger(owner) || owner < 1) return true;
      try {
        process.kill(owner, 0);
        return true;
      } catch (error) {
        return (error as NodeJS.ErrnoException).code !== 'ESRCH';
      }
    } catch (error) {
      return (error as NodeJS.ErrnoException).code !== 'ENOENT';
    }
  }

  private restore(): void {
    if (this.deps.persist === false || this.lockHeld) return;
    try {
      const file = path.join(this.directory, 'dashboard-job.json');
      if (fs.statSync(file).size > MAX_OUTPUT) return;
      const raw = record(record(JSON.parse(fs.readFileSync(file, 'utf8')))?.job);
      if (
        !raw ||
        typeof raw.id !== 'string' ||
        !/^[a-f0-9-]{36}$/i.test(raw.id) ||
        !['running', 'completed', 'failed'].includes(raw.state as string) ||
        typeof raw.startedAt !== 'string' ||
        raw.startedAt.length > 40 ||
        !Number.isFinite(Date.parse(raw.startedAt)) ||
        !Array.isArray(raw.results) ||
        raw.results.length > EXPECTED_RESULTS
      )
        return;
      const results: AppUpdateResult[] = [];
      for (const value of raw.results) {
        const row = record(value);
        if (
          !row ||
          typeof row.appId !== 'string' ||
          !Object.prototype.hasOwnProperty.call(UPDATE_APP_LABELS, row.appId) ||
          !PLATFORMS.includes(row.platform as UpdatePlatform)
        )
          continue;
        const code =
          Object.entries(MESSAGES).find(([, message]) => message === row.message)?.[0] ??
          'helper_invalid';
        const restored = normalizeAppUpdateResults(
          JSON.stringify({ results: [{ ...row, messageCode: code }] }),
          row.platform as UpdatePlatform
        ).find((result) => result.appId === row.appId);
        if (restored) results.push(restored);
      }
      const abandoned = raw.state === 'running' && !this.lockOwnerAlive();
      this.job = {
        id: raw.id,
        state: abandoned ? 'failed' : (raw.state as AppUpdateJob['state']),
        startedAt: new Date(raw.startedAt).toISOString(),
        finishedAt: abandoned
          ? new Date(this.deps.now()).toISOString()
          : typeof raw.finishedAt === 'string' &&
              raw.finishedAt.length <= 40 &&
              Number.isFinite(Date.parse(raw.finishedAt))
            ? new Date(raw.finishedAt).toISOString()
            : null,
        activePlatform:
          !abandoned && PLATFORMS.includes(raw.activePlatform as UpdatePlatform)
            ? (raw.activePlatform as UpdatePlatform)
            : null,
        cancelRequested: raw.cancelRequested === true,
        results,
        expectedResults:
          typeof raw.expectedResults === 'number' &&
          Number.isSafeInteger(raw.expectedResults) &&
          raw.expectedResults >= results.length &&
          raw.expectedResults <= EXPECTED_RESULTS
            ? raw.expectedResults
            : null,
      };
    } catch {
      /* An absent/corrupt result never triggers or replays an update. */
    }
  }
}

const services = new Map<string, AppUpdateService>();
export function getAppUpdateService(): AppUpdateService {
  const scope = path.resolve(getCcsDir());
  let service = services.get(scope);
  if (!service) {
    service = new AppUpdateService({ ccsDir: scope, sync: syncRemoteHelpers });
    services.set(scope, service);
  }
  return service;
}
