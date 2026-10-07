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
  skipped,
  unknown,
  isUpdateAppId,
  normalizeAppUpdateResults,
  normalizeAppUpdateRow,
  messageFor,
  type MessageCode,
  type UpdateAppId,
  type UpdatePlatform,
  type AppUpdateResult,
  type AppUpdateJob,
  type AppUpdateHostProgress,
  APP_UPDATE_PHASES,
  type AppUpdatePhase,
} from './app-update-contract';
export { UPDATE_APP_LABELS, normalizeAppUpdateResults } from './app-update-contract';
export type {
  UpdateAppId,
  UpdatePlatform,
  UpdateResultStatus,
  AppUpdateResult,
  AppUpdateJob,
  AppUpdateHostProgress,
} from './app-update-contract';
import { runHost, syncRemoteHelpers, type HostRunControl } from './app-update-hosts';
export {
  antigravityReviewedArgument,
  appUpdateInvocation,
  parseDeployedChecksums,
  syncRemoteHelpers,
  MAC_EXTRACT,
} from './app-update-hosts';
export type { HostRunControl } from './app-update-hosts';
/**
 * Hard bound for one computer's whole run (helper sync included). The helpers
 * stop starting apps after 15 minutes and the Windows coordinator gives up
 * after 16; past this the host process is stopped and every app without a
 * result reads "Timed out" instead of the job waiting forever.
 */
const HOST_DEADLINE_MS = 18 * 60 * 1000;

export class AppUpdateBusyError extends Error {
  constructor() {
    super('An app update is already running.');
    this.name = 'AppUpdateBusyError';
  }
}

export interface AppUpdateDependencies {
  runHost(platform: UpdatePlatform, control: HostRunControl): Promise<string>;
  /** Aligns deployed remote helpers with this build; production wires the real sync. */
  sync(platform: UpdatePlatform): Promise<void>;
  now(): number;
  id(): string;
  ccsDir?: string;
  persist?: boolean;
  /** Hard bound for one computer's run; tests shorten it. */
  hostDeadlineMs?: number;
}

function waitingHosts(): Record<UpdatePlatform, AppUpdateHostProgress> {
  return {
    ubuntu: { state: 'waiting', currentApp: null, phase: null },
    mac: { state: 'waiting', currentApp: null, phase: null },
    windows: { state: 'waiting', currentApp: null, phase: null },
  };
}

/** Restores saved host progress only when every field is one of the fixed values. */
function restoreHosts(value: unknown): Record<UpdatePlatform, AppUpdateHostProgress> | null {
  const raw = record(value);
  if (!raw) return null;
  const hosts = waitingHosts();
  for (const platform of PLATFORMS) {
    const host = record(raw[platform]);
    if (
      !host ||
      !['waiting', 'running', 'done'].includes(host.state as string) ||
      !(host.currentApp === null || isUpdateAppId(host.currentApp)) ||
      !(host.phase === null || APP_UPDATE_PHASES.includes(host.phase as AppUpdatePhase))
    )
      return null;
    hosts[platform] = {
      state: host.state as AppUpdateHostProgress['state'],
      currentApp: host.currentApp as UpdateAppId | null,
      phase: host.phase as AppUpdateHostProgress['phase'],
    };
    if (typeof host.phaseSince === 'string' && !Number.isNaN(Date.parse(host.phaseSince)))
      hosts[platform].phaseSince = new Date(Date.parse(host.phaseSince)).toISOString();
  }
  return hosts;
}

/** Starts work only in response to the explicit authenticated POST action. */
export class AppUpdateService {
  private job: AppUpdateJob | null = null;
  private readonly deps: AppUpdateDependencies;
  private readonly directory: string;
  private lockHeld = false;
  /** How to forward a cancel to each host helper that is running now. */
  private readonly cancelHandlers = new Map<UpdatePlatform, () => void>();

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
      hosts: waitingHosts(),
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
   * Acknowledges a cancel at once. Nothing running is killed: each computer
   * finishes the app it is on now, then reports every app it has not started
   * as skipped. Idempotent: a second cancel changes nothing. Never promises an undo.
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
    const first = !this.job.cancelRequested;
    this.job.cancelRequested = true;
    if (first)
      for (const handler of this.cancelHandlers.values()) {
        try {
          handler();
        } catch {
          /* A host that cannot hear the cancel just finishes its apps. */
        }
      }
    try {
      this.save();
    } catch {
      /* The in-memory flag still stops queued work; persistence is display-only. */
    }
    return { job: this.getStatus().job, cancelling: true };
  }

  /**
   * Every computer runs at the same time; each one still updates its own apps
   * one at a time. The job ends when the slowest computer finishes or hits
   * its hard deadline, so one stuck host never holds the others back.
   */
  private async execute(): Promise<void> {
    const job = this.job;
    if (!job) return;
    try {
      await Promise.all(PLATFORMS.map((platform) => this.runPlatform(job, platform)));
      job.state = job.results.some(
        (result) =>
          result.status === 'failed' ||
          result.status === 'restart_failed' ||
          result.status === 'unknown'
      )
        ? 'failed'
        : 'completed';
    } catch {
      job.state = 'failed';
    } finally {
      // A cancel that landed after the last app changed no result; leave no trace.
      if (!job.results.some((result) => result.status === 'skipped')) job.cancelRequested = false;
      job.activePlatform = null;
      job.finishedAt = new Date(this.deps.now()).toISOString();
      this.trySave();
      this.releaseLock();
    }
  }

  private hostsOf(job: AppUpdateJob): Record<UpdatePlatform, AppUpdateHostProgress> {
    if (!job.hosts) job.hosts = waitingHosts();
    return job.hosts;
  }

  private async runPlatform(job: AppUpdateJob, platform: UpdatePlatform): Promise<void> {
    const host = this.hostsOf(job)[platform];
    if (job.cancelRequested) {
      this.fillMissing(job, platform, (appId) => skipped(platform, appId));
      this.hostDone(job, platform);
      return;
    }
    host.state = 'running';
    host.phase = 'checking';
    host.phaseSince = new Date(this.deps.now()).toISOString();
    this.refreshActive(job);
    this.trySave();
    let abort: (() => void) | undefined;
    let timedOut = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const deadline = new Promise<never>((_, reject) => {
      timer = setTimeout(() => {
        timedOut = true;
        reject(new Error('App update host timed out.'));
      }, this.deps.hostDeadlineMs ?? HOST_DEADLINE_MS);
      timer.unref?.();
    });
    deadline.catch(() => {});
    const control: HostRunControl = {
      stateDirectory: this.directory,
      onEvent: (event) => this.hostEvent(job, platform, event),
      setCancel: (handler) => {
        this.cancelHandlers.set(platform, handler);
        if (job.cancelRequested) handler();
      },
      setAbort: (handler) => {
        abort = handler;
      },
    };
    const work = async (): Promise<string | null> => {
      // Ubuntu runs the helpers inside this installed build; only the remote
      // hosts carry deployed copies that can go stale. Skipping the await
      // also keeps start() launching the Ubuntu helper synchronously.
      if (platform !== 'ubuntu') {
        try {
          await this.deps.sync(platform);
        } catch {
          /* Best effort: the deployed helpers stay as they are. */
        }
        if (timedOut) return null;
        if (job.cancelRequested) return null;
      }
      return this.deps.runHost(platform, control);
    };
    try {
      const output = await Promise.race([work(), deadline]);
      if (output === null) this.fillMissing(job, platform, (appId) => skipped(platform, appId));
      else this.mergeFinal(job, platform, output);
    } catch {
      if (timedOut) {
        try {
          abort?.();
        } catch {
          /* The process may already be gone. */
        }
      }
      // Apps that reported keep their real rows; the rest never answered.
      this.fillMissing(job, platform, (appId) =>
        unknown(platform, appId, timedOut ? 'host_timeout' : 'host_unknown')
      );
    } finally {
      if (timer) clearTimeout(timer);
      this.cancelHandlers.delete(platform);
      this.hostDone(job, platform);
    }
  }

  private hostDone(job: AppUpdateJob, platform: UpdatePlatform): void {
    const host = this.hostsOf(job)[platform];
    host.state = 'done';
    host.currentApp = null;
    host.phase = null;
    delete host.phaseSince;
    this.refreshActive(job);
    this.trySave();
  }

  private refreshActive(job: AppUpdateJob): void {
    job.activePlatform =
      PLATFORMS.find((platform) => job.hosts?.[platform].state === 'running') ?? null;
  }

  /** Live progress from one host: which app it is on, and each row as soon as it is known. */
  private hostEvent(job: AppUpdateJob, platform: UpdatePlatform, event: Record<string, unknown>) {
    const host = job.hosts?.[platform];
    if (this.job !== job || job.state !== 'running' || host?.state !== 'running') return;
    if (event.event === 'app') {
      if (!(event.appId === null || isUpdateAppId(event.appId))) return;
      if (!APP_UPDATE_PHASES.includes(event.phase as AppUpdatePhase)) return;
      if (host.currentApp !== event.appId || host.phase !== event.phase)
        host.phaseSince = new Date(this.deps.now()).toISOString();
      host.currentApp = event.appId;
      host.phase = event.phase as AppUpdatePhase;
    } else if (event.event === 'result') {
      const row = record(event.result);
      if (!row || !isUpdateAppId(row.appId) || this.has(job, platform, row.appId)) return;
      job.results.push(normalizeAppUpdateRow(row, row.appId, platform));
      if (host.currentApp === row.appId) host.currentApp = null;
    } else return;
    this.trySave();
  }

  /**
   * The helper's final document is authoritative for every app it names, except
   * that a malformed final row never replaces a well-formed streamed one.
   */
  private mergeFinal(job: AppUpdateJob, platform: UpdatePlatform, output: string): void {
    let payload: Record<string, unknown> | undefined;
    try {
      payload = record(JSON.parse(output));
    } catch {
      payload = undefined;
    }
    if (Array.isArray(payload?.results)) {
      for (const row of normalizeAppUpdateResults(output, platform)) {
        const index = job.results.findIndex(
          (value) => value.platform === platform && value.appId === row.appId
        );
        if (index < 0) job.results.push(row);
        else if (row.message !== MESSAGES.helper_invalid) job.results[index] = row;
      }
    }
    this.fillMissing(job, platform, (appId) => failure(platform, appId, 'helper_invalid'));
  }

  private has(job: AppUpdateJob, platform: UpdatePlatform, appId: UpdateAppId): boolean {
    return job.results.some((row) => row.platform === platform && row.appId === appId);
  }

  private fillMissing(
    job: AppUpdateJob,
    platform: UpdatePlatform,
    make: (appId: UpdateAppId) => AppUpdateResult
  ): void {
    for (const appId of Object.keys(UPDATE_APP_LABELS) as UpdateAppId[])
      if (!this.has(job, platform, appId)) job.results.push(make(appId));
  }

  private trySave(): void {
    try {
      this.save();
    } catch {
      /* Never expose filesystem errors to clients; the in-memory job stays right. */
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
        // Saved rows keep only their words; map them back through messageFor so a
        // row whose words name the app ("Quit Codex Desktop to finish its update")
        // restores as itself instead of as helper_invalid.
        const appId = row.appId as UpdateAppId;
        const code =
          (Object.keys(MESSAGES) as MessageCode[]).find(
            (candidate) => messageFor(candidate, appId) === row.message
          ) ?? 'helper_invalid';
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
        hosts: abandoned ? null : restoreHosts(raw.hosts),
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
