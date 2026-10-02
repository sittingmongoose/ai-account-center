import { execFile } from 'child_process';
import { randomUUID } from 'crypto';
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
  failure,
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

export interface AppUpdateDependencies {
  runHost(platform: UpdatePlatform): Promise<string>;
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
    this.deps = { runHost, now: Date.now, id: randomUUID, persist: true, ...overrides };
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

  private async execute(): Promise<void> {
    if (!this.job) return;
    try {
      for (const platform of PLATFORMS) {
        this.job.activePlatform = platform;
        this.save();
        try {
          const output = await this.deps.runHost(platform);
          this.job.results.push(...normalizeAppUpdateResults(output, platform));
        } catch {
          this.job.results.push(
            ...(Object.keys(UPDATE_APP_LABELS) as UpdateAppId[]).map((id) =>
              failure(platform, id, 'host_unavailable')
            )
          );
        }
        this.save();
      }
      this.job.state = this.job.results.some(
        (result) => result.status === 'failed' || result.status === 'restart_failed'
      )
        ? 'failed'
        : 'completed';
    } catch {
      this.job.state = 'failed';
    } finally {
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
    service = new AppUpdateService({ ccsDir: scope });
    services.set(scope, service);
  }
  return service;
}
