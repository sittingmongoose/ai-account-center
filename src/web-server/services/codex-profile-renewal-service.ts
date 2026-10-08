import * as path from 'path';
import {
  getCodexProfileRenewalStatus,
  isCodexProfileRenewalEnabled,
  renewDueCodexProfiles,
  type CodexProfileRenewalOptions,
  type CodexProfileRenewalProfileStatus,
  type CodexRenewalCycle,
} from '../../codex-auth/codex-profile-renewal';
import { CODEX_RENEWAL_MESSAGES } from '../../codex-auth/codex-renewal-types';
import { getCcsDir, runWithScopedConfigDir } from '../../utils/config-manager';
import { createLogger } from '../../services/logging';

const logger = createLogger('codex-renewal');

const FIRST_DELAY_MS = 3 * 60_000;
const FIRST_JITTER_MS = 2 * 60_000;
const CYCLE_MS = 6 * 60 * 60_000;
const CYCLE_JITTER_MS = 30 * 60_000;
const MIN_RETRY_DELAY_MS = 5 * 60_000;
const STATUS_TTL_MS = 15_000;

export interface CodexProfileRenewalServiceStatus {
  enabled: boolean;
  running: boolean;
  /** Profile being renewed right now, if any. */
  renewing: string | null;
  lastCycleAt: string | null;
  nextCycleAt: string | null;
  message: string | null;
  profiles: CodexProfileRenewalProfileStatus[];
}

export interface CodexProfileRenewalServiceDeps {
  ccsDir?: string;
  env?: NodeJS.ProcessEnv;
  now?: () => number;
  random?: () => number;
  /** Passed to every cycle and status read (test homes, fetch seam, log sink). */
  renewal?: CodexProfileRenewalOptions;
  runCycle?: (options: CodexProfileRenewalOptions) => Promise<CodexRenewalCycle>;
}

/**
 * Renews idle saved Codex logins in the background: first cycle 3-5 minutes
 * after start, then every 6 hours +/- 30 minutes, sooner when a backoff or a
 * busy activation lock asks for a retry (never under 5 minutes). One cycle at a
 * time; stop() lets a refresh in flight finish but starts no further profile.
 */
export class CodexProfileRenewalService {
  readonly ccsDir: string;
  private timer?: ReturnType<typeof setTimeout>;
  private running = false;
  private generation = 0;
  private inFlight: Promise<CodexRenewalCycle | null> | null = null;
  private renewing: string | null = null;
  private lastCycleAt: string | null = null;
  private nextCycleAt: string | null = null;
  private lastError: string | null = null;
  private statusCache: { value: CodexProfileRenewalServiceStatus; at: number } | null = null;

  constructor(private readonly deps: CodexProfileRenewalServiceDeps = {}) {
    this.ccsDir = path.resolve(deps.ccsDir ?? getCcsDir());
  }

  private now(): number {
    return (this.deps.now ?? Date.now)();
  }

  private random(): number {
    return (this.deps.random ?? Math.random)();
  }

  private enabled(): boolean {
    return isCodexProfileRenewalEnabled(this.deps.env ?? this.deps.renewal?.env ?? process.env);
  }

  start(): void {
    if (this.running) return;
    if (!this.enabled()) {
      try {
        logger.info('codex.renewal', CODEX_RENEWAL_MESSAGES.disabled, { outcome: 'disabled' });
      } catch {
        // Logging is best effort.
      }
      return;
    }
    this.running = true;
    this.schedule(FIRST_DELAY_MS + Math.round(FIRST_JITTER_MS * this.random()));
  }

  stop(): void {
    this.running = false;
    this.generation++;
    clearTimeout(this.timer);
    this.timer = undefined;
    this.nextCycleAt = null;
  }

  private schedule(delay: number): void {
    clearTimeout(this.timer);
    this.nextCycleAt = new Date(this.now() + delay).toISOString();
    this.timer = setTimeout(() => {
      this.timer = undefined;
      void this.runCycle().then((cycle) => {
        if (this.running) this.schedule(this.nextDelay(cycle));
      });
    }, delay);
    this.timer.unref?.();
  }

  private nextDelay(cycle: CodexRenewalCycle | null): number {
    const regular = CYCLE_MS + Math.round((this.random() * 2 - 1) * CYCLE_JITTER_MS);
    const retryAt = cycle?.retryAt ? Date.parse(cycle.retryAt) : NaN;
    if (!Number.isFinite(retryAt)) return regular;
    return Math.min(regular, Math.max(MIN_RETRY_DELAY_MS, retryAt - this.now()));
  }

  /** Coalesced: concurrent callers share one cycle. Never rejects. */
  runCycle(): Promise<CodexRenewalCycle | null> {
    if (this.inFlight) return this.inFlight;
    const generation = this.generation;
    const options: CodexProfileRenewalOptions = {
      ...this.deps.renewal,
      ...(this.deps.env ? { env: this.deps.env } : {}),
      ...(this.deps.now ? { now: this.deps.now } : {}),
      ...(this.deps.random ? { random: this.deps.random } : {}),
      shouldContinue: () => generation === this.generation && this.enabled(),
      onRenewing: (name) => {
        this.renewing = name;
        this.statusCache = null;
      },
    };
    this.inFlight = runWithScopedConfigDir(this.ccsDir, () =>
      (this.deps.runCycle ?? renewDueCodexProfiles)(options)
    )
      .then((cycle) => {
        this.lastCycleAt = cycle.finishedAt;
        this.lastError = null;
        return cycle;
      })
      .catch(() => {
        // Registry or file failures carry no secrets, but only a fixed line is kept.
        this.lastError = 'The last renewal check could not read the saved Codex profiles.';
        try {
          logger.warn('codex.renewal', this.lastError, { outcome: 'cycle_failed' });
        } catch {
          // Logging is best effort.
        }
        return null;
      })
      .finally(() => {
        this.inFlight = null;
        this.renewing = null;
        this.statusCache = null;
      });
    return this.inFlight;
  }

  /** Dashboard DTO; local reads only, cached for 15 seconds. */
  async getStatus(): Promise<CodexProfileRenewalServiceStatus> {
    const now = this.now();
    if (this.statusCache && now - this.statusCache.at < STATUS_TTL_MS) {
      return this.withLive(this.statusCache.value);
    }
    let profiles: CodexProfileRenewalProfileStatus[] = [];
    let message = this.lastError;
    try {
      profiles = await runWithScopedConfigDir(
        this.ccsDir,
        () =>
          getCodexProfileRenewalStatus({
            ...this.deps.renewal,
            ...(this.deps.env ? { env: this.deps.env } : {}),
            ...(this.deps.now ? { now: this.deps.now } : {}),
          }).profiles
      );
    } catch {
      message = 'Saved Codex profiles could not be read for renewal status.';
    }
    const value: CodexProfileRenewalServiceStatus = {
      enabled: this.enabled(),
      running: this.running,
      renewing: null,
      lastCycleAt: this.lastCycleAt,
      nextCycleAt: this.nextCycleAt,
      message,
      profiles,
    };
    this.statusCache = { value, at: now };
    return this.withLive(value);
  }

  private withLive(value: CodexProfileRenewalServiceStatus): CodexProfileRenewalServiceStatus {
    const renewing = this.renewing;
    return {
      ...value,
      running: this.running,
      renewing,
      lastCycleAt: this.lastCycleAt,
      nextCycleAt: this.nextCycleAt,
      profiles: value.profiles.map((profile) =>
        profile.name === renewing
          ? {
              ...profile,
              state: 'renewing',
              reason: 'renewing',
              message: CODEX_RENEWAL_MESSAGES.renewing,
            }
          : profile
      ),
    };
  }
}

const services = new Map<string, CodexProfileRenewalService>();

/** One service per CCS scope, like the automatic-switch monitor. */
export function getCodexProfileRenewalService(): CodexProfileRenewalService {
  const scope = path.resolve(getCcsDir());
  let service = services.get(scope);
  if (!service) {
    service = new CodexProfileRenewalService({ ccsDir: scope });
    services.set(scope, service);
  }
  return service;
}
