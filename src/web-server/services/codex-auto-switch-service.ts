import * as fs from 'fs';
import * as path from 'path';
import { createHash, randomBytes } from 'crypto';
import { ConfigError } from '../../errors/error-types';
import {
  activateCodexProfile,
  CodexActivationError,
  getActivationCodexHome,
} from '../../codex-auth/activate-codex-profile';
import { decodeIdToken } from '../../codex-auth/decode-id-token';
import { resolveCodexProfileDir } from '../../codex-auth/codex-profile-paths';
import {
  getCodexAuthProfilesSummary,
  invalidateCodexAuthProfilesCache,
} from '../../codex-auth/codex-auth-dashboard-service';
import type { CodexAuthProfilesSummary } from '../../codex-auth/codex-auth-dashboard-service';
import { getCcsDir, runWithScopedConfigDir } from '../../utils/config-manager';
import { createLogger } from '../../services/logging';
import { getCodexProfileQuotaRows } from '../usage/native-quota-collector';
import type { BarSummaryRow } from '../routes/bar-routes';

const logger = createLogger('codex-auto-switch');

const DEFAULT_THRESHOLD_PERCENT = 5;
const POLL_MS = 60_000;
const MAX_QUOTA_AGE_MS = 630_000;
const ERROR_BACKOFF_MS = 300_000;

export type CodexAutoSwitchOutcome =
  | 'disabled'
  | 'scheduled'
  | 'healthy'
  | 'no_quota'
  | 'no_candidate'
  | 'waiting_idle'
  | 'switching'
  | 'switched'
  | 'error';

export interface CodexAutoSwitchStatus {
  enabled: boolean;
  /** Remaining usage percentage that triggers a switch. */
  thresholdPercent: number;
  /** The same threshold in % used (100 - thresholdPercent), the unit Antigravity uses. */
  thresholdUsedPercent: number;
  pollIntervalSeconds: number;
  outcome: CodexAutoSwitchOutcome;
  message: string;
  activationInProgress: boolean;
  lastCheckedAt?: string;
  lastSwitchedAt?: string;
  /** Profile the monitor chose but could not switch to yet. Present only for waiting_idle. */
  candidate?: string;
}

interface AutoSwitchConfig {
  enabled: boolean;
  thresholdPercent?: number;
}

export interface CodexAutoSwitchSettings {
  enabled?: boolean;
  /** Remaining usage percentage; the UI displays 100 minus this value as used. */
  thresholdPercent?: number;
}

function isThreshold(value: unknown): value is number {
  return typeof value === 'number' && Number.isInteger(value) && value >= 1 && value <= 99;
}

export function isCodexAutoSwitchSettings(value: unknown): value is CodexAutoSwitchSettings {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const settings = value as Record<string, unknown>;
  const keys = Object.keys(settings);
  return (
    keys.length > 0 &&
    keys.every((key) => key === 'enabled' || key === 'thresholdPercent') &&
    (!keys.includes('enabled') || typeof settings.enabled === 'boolean') &&
    (!keys.includes('thresholdPercent') || isThreshold(settings.thresholdPercent))
  );
}

export interface CodexAutoSwitchAuthSnapshot {
  live: { fingerprint: string; email?: string; accountId?: string } | null;
  profiles: Record<string, string | null>;
}

export interface CodexAutoSwitchDeps {
  ccsDir?: string;
  readConfig?: () => AutoSwitchConfig;
  writeConfig?: (config: AutoSwitchConfig) => void;
  getSummary?: () => Promise<CodexAuthProfilesSummary>;
  getRows?: (names: string[]) => Promise<BarSummaryRow[]>;
  activate?: (name: string) => Promise<unknown>;
  /** Private fingerprints are only compared in memory, never returned to the browser. */
  getAuthSnapshot?: (names: string[]) => CodexAutoSwitchAuthSnapshot;
  now?: () => number;
  /** Outcome-trail sink; profile names only, never tokens or fingerprints. Tests inject a recorder. */
  log?: (
    level: 'info' | 'warn',
    event: string,
    message: string,
    context: Record<string, unknown>
  ) => void;
}

/** Pure private parser seam: quota's workspace header must match the decoded saved login. */
export function getCodexAutoSwitchAuthSnapshotFromContents(
  liveContent: Buffer | null,
  profileContents: Record<string, Buffer | null>
): CodexAutoSwitchAuthSnapshot {
  const parse = (content: Buffer | null): CodexAutoSwitchAuthSnapshot['live'] => {
    if (!content) return null;
    try {
      const value = JSON.parse(content.toString('utf8')) as {
        tokens?: { id_token?: unknown; account_id?: unknown };
      };
      const identity =
        typeof value.tokens?.id_token === 'string' ? decodeIdToken(value.tokens.id_token) : {};
      const workspace = value.tokens?.account_id;
      if (
        !identity.email ||
        typeof workspace !== 'string' ||
        !workspace ||
        workspace !== identity.account_id
      )
        return null;
      return {
        fingerprint: createHash('sha256').update(content).digest('hex'),
        email: identity.email,
        accountId: workspace,
      };
    } catch {
      return null;
    }
  };
  return {
    live: parse(liveContent),
    profiles: Object.fromEntries(
      Object.entries(profileContents).map(([name, content]) => [
        name,
        parse(content)?.fingerprint ?? null,
      ])
    ),
  };
}

function readAuthSnapshot(names: string[]): CodexAutoSwitchAuthSnapshot {
  const read = (file: string): Buffer | null => {
    try {
      if (fs.statSync(file).size > 1_048_576) return null;
      return fs.readFileSync(file);
    } catch {
      return null;
    }
  };
  const content = read(path.join(getActivationCodexHome(), 'auth.json'));
  const profiles = Object.fromEntries(
    names.map((name) => {
      const saved = read(path.join(resolveCodexProfileDir(name), 'auth.json'));
      return [name, saved];
    })
  );
  return getCodexAutoSwitchAuthSnapshotFromContents(content, profiles);
}

const messages: Record<CodexAutoSwitchOutcome, string> = {
  disabled: 'Automatic Codex account switching is disabled.',
  scheduled: 'Automatic Codex account switching is enabled; the next check is scheduled.',
  healthy: 'The active Codex account has enough remaining quota.',
  no_quota: 'Waiting for fresh provider usage for the active Codex account.',
  no_candidate: 'No other authenticated Codex account has enough remaining quota to switch.',
  waiting_idle: 'Waiting for Codex to finish active work before switching accounts.',
  switching: 'A Codex account activation is in progress; disabling cannot interrupt it.',
  switched: 'The shared Codex login switched to an account with available usage.',
  error: 'Automatic Codex switching could not complete safely; it will retry later.',
};

function readConfigFile(ccsDir: string): AutoSwitchConfig {
  const file = path.join(ccsDir, 'codex-auto-switch.json');
  try {
    const stat = fs.lstatSync(file);
    if (!stat.isFile() || stat.size > 4096)
      throw new ConfigError('Invalid automatic switching config');
    const value = JSON.parse(fs.readFileSync(file, 'utf8')) as Record<string, unknown>;
    if (
      !value ||
      typeof value.enabled !== 'boolean' ||
      value.version !== 1 ||
      (value.thresholdPercent !== undefined && !isThreshold(value.thresholdPercent)) ||
      value.pollIntervalSeconds !== POLL_MS / 1000 ||
      Object.keys(value).some(
        (key) => !['version', 'enabled', 'thresholdPercent', 'pollIntervalSeconds'].includes(key)
      )
    ) {
      throw new ConfigError('Invalid automatic switching config');
    }
    return {
      enabled: value.enabled,
      thresholdPercent: (value.thresholdPercent as number | undefined) ?? DEFAULT_THRESHOLD_PERCENT,
    };
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT')
      return { enabled: false, thresholdPercent: DEFAULT_THRESHOLD_PERCENT };
    throw new ConfigError('Automatic switching config could not be read safely');
  }
}

function writeConfigFile(ccsDir: string, config: AutoSwitchConfig): void {
  fs.mkdirSync(ccsDir, { recursive: true, mode: 0o700 });
  const file = path.join(ccsDir, 'codex-auto-switch.json');
  const temporary = `${file}.${process.pid}.${randomBytes(8).toString('hex')}.tmp`;
  try {
    fs.writeFileSync(
      temporary,
      JSON.stringify(
        {
          version: 1,
          enabled: config.enabled,
          thresholdPercent: config.thresholdPercent ?? DEFAULT_THRESHOLD_PERCENT,
          pollIntervalSeconds: POLL_MS / 1000,
        },
        null,
        2
      ) + '\n',
      { flag: 'wx', mode: 0o600 }
    );
    fs.renameSync(temporary, file);
  } finally {
    fs.rmSync(temporary, { force: true });
  }
}

/** Why a quota row cannot decide a switch. Drives the plain-words message, never shown raw. */
type RemainingRejection = 'missing' | 'not_network' | 'stale' | 'reset_passed' | 'no_windows';

type RemainingAssessment = { remaining: number } | { reason: RemainingRejection };

/** Network provenance and account identity are mandatory; local session logs cannot decide. */
function assessRemaining(
  row: BarSummaryRow | undefined,
  profile: string,
  now: number
): RemainingAssessment {
  if (!row || row.profile !== profile) return { reason: 'missing' };
  if (
    row.surface !== 'ccsx' ||
    row.provider !== 'codex' ||
    row.is_subscription !== true ||
    row.quotaSource !== 'network' ||
    row.quotaStatus !== 'ok' ||
    row.needsReauth
  )
    return { reason: 'not_network' };
  const fetchedAt = Date.parse(row.fetchedAt);
  if (
    !Number.isFinite(fetchedAt) ||
    now - fetchedAt > MAX_QUOTA_AGE_MS ||
    fetchedAt - now > 30_000
  ) {
    return { reason: 'stale' };
  }
  const windows = (row.quotaWindows ?? []).filter(
    (window) => window.key === 'five_hour' || window.key === 'seven_day'
  );
  if (
    windows.length === 0 ||
    windows.some((window) => !Number.isFinite(window.usedPercent) || window.usedPercent < 0)
  )
    return { reason: 'no_windows' };
  if (
    windows.some((window) => {
      const resetAt = window.resetAt ? Date.parse(window.resetAt) : NaN;
      return Number.isFinite(resetAt) && resetAt <= now && fetchedAt < resetAt;
    })
  )
    return { reason: 'reset_passed' };
  return {
    remaining: Math.min(...windows.map((window) => Math.max(0, 100 - window.usedPercent))),
  };
}

function remainingOrNull(assessment: RemainingAssessment): number | null {
  return 'remaining' in assessment ? assessment.remaining : null;
}

/** One monitor per CCS scope. Activation always uses the existing busy-aware transaction. */
export class CodexAutoSwitchService {
  readonly ccsDir: string;
  private timer?: ReturnType<typeof setTimeout>;
  private running = false;
  private stopped = false;
  private generation = 0;
  private inFlight: Promise<void> | null = null;
  private activationInProgress = false;
  private outcome: CodexAutoSwitchOutcome = 'disabled';
  private lastCheckedAt?: string;
  private lastSwitchedAt?: string;
  private retryAfter = 0;
  private blockedCandidate?: string;
  private blockedDetail?: string;
  private lastLoggedOutcome?: CodexAutoSwitchOutcome;
  /** This cycle's vetted decision, so a busy activation can report what it blocked. */
  private lastActive?: string;
  private lastCandidate?: string;

  constructor(private readonly deps: CodexAutoSwitchDeps = {}) {
    this.ccsDir = path.resolve(deps.ccsDir ?? getCcsDir());
  }

  private readConfig(): Required<AutoSwitchConfig> {
    const value = this.deps.readConfig?.() ?? readConfigFile(this.ccsDir);
    const thresholdPercent = value.thresholdPercent ?? DEFAULT_THRESHOLD_PERCENT;
    if (typeof value.enabled !== 'boolean' || !isThreshold(thresholdPercent)) {
      throw new ConfigError('Invalid automatic switching config');
    }
    return { enabled: value.enabled, thresholdPercent };
  }

  getStatus(): CodexAutoSwitchStatus {
    let enabled = false;
    let thresholdPercent = DEFAULT_THRESHOLD_PERCENT;
    let outcome = this.outcome;
    try {
      const config = this.readConfig();
      enabled = config.enabled;
      thresholdPercent = config.thresholdPercent;
      if (!enabled) outcome = this.activationInProgress ? 'switching' : 'disabled';
      else if (outcome === 'disabled') outcome = 'scheduled';
    } catch {
      outcome = 'error';
    }
    const detail = outcome === this.outcome ? this.blockedDetail : undefined;
    return {
      enabled,
      thresholdPercent,
      thresholdUsedPercent: 100 - thresholdPercent,
      pollIntervalSeconds: POLL_MS / 1000,
      outcome,
      message: detail ?? messages[outcome],
      activationInProgress: this.activationInProgress,
      ...(this.lastCheckedAt ? { lastCheckedAt: this.lastCheckedAt } : {}),
      ...(this.lastSwitchedAt ? { lastSwitchedAt: this.lastSwitchedAt } : {}),
      ...(outcome === 'waiting_idle' && this.blockedCandidate
        ? { candidate: this.blockedCandidate }
        : {}),
    };
  }

  setEnabled(enabled: boolean): CodexAutoSwitchStatus {
    return this.updateSettings({ enabled });
  }

  updateSettings(settings: CodexAutoSwitchSettings): CodexAutoSwitchStatus {
    if (!isCodexAutoSwitchSettings(settings))
      throw new ConfigError('Invalid automatic switching settings');
    const config = { ...this.readConfig(), ...settings };
    if (this.deps.writeConfig) this.deps.writeConfig(config);
    else writeConfigFile(this.ccsDir, config);
    this.generation++;
    this.retryAfter = 0;
    this.blockedCandidate = undefined;
    this.blockedDetail = undefined;
    if (!this.activationInProgress) this.outcome = config.enabled ? 'scheduled' : 'disabled';
    if (this.running) this.schedule(config.enabled ? 1000 : POLL_MS);
    return this.getStatus();
  }

  start(): void {
    if (this.running) return;
    this.running = true;
    this.stopped = false;
    this.schedule(1000);
  }

  stop(): void {
    this.running = false;
    this.stopped = true;
    this.generation++;
    clearTimeout(this.timer);
    this.timer = undefined;
  }

  private schedule(delay: number): void {
    clearTimeout(this.timer);
    this.timer = setTimeout(() => {
      this.timer = undefined;
      void this.runCycle().finally(() => {
        if (this.running) this.schedule(POLL_MS);
      });
    }, delay);
    this.timer.unref();
  }

  runCycle(): Promise<void> {
    if (this.inFlight) return this.inFlight;
    this.inFlight = Promise.resolve()
      .then(() => runWithScopedConfigDir(this.ccsDir, () => this.check()))
      .finally(() => {
        this.inFlight = null;
      });
    return this.inFlight;
  }

  private async summary(): Promise<CodexAuthProfilesSummary> {
    if (this.deps.getSummary) return this.deps.getSummary();
    invalidateCodexAuthProfilesCache();
    return getCodexAuthProfilesSummary();
  }

  private logConclusion(context: Record<string, unknown>): void {
    const changed = this.outcome !== this.lastLoggedOutcome;
    if (!changed && this.outcome !== 'switched' && this.outcome !== 'error') return;
    this.lastLoggedOutcome = this.outcome;
    const message = this.blockedDetail ?? messages[this.outcome];
    if (this.deps.log) {
      this.deps.log(this.outcome === 'error' ? 'warn' : 'info', 'codex.auto_switch', message, {
        outcome: this.outcome,
        ...context,
      });
      return;
    }
    try {
      if (this.outcome === 'error')
        logger.warn('codex.auto_switch', message, { outcome: this.outcome, ...context });
      else logger.info('codex.auto_switch', message, { outcome: this.outcome, ...context });
    } catch {
      // The outcome trail must never break the monitor.
    }
  }

  private async check(): Promise<void> {
    const now = this.deps.now ?? Date.now;
    const generation = this.generation;
    this.blockedCandidate = undefined;
    this.blockedDetail = undefined;
    this.lastActive = undefined;
    this.lastCandidate = undefined;
    try {
      const config = this.readConfig();
      if (this.stopped || !config.enabled) {
        this.outcome = 'disabled';
        this.logConclusion({});
        return;
      }
      if (now() < this.retryAfter) return;
      this.lastCheckedAt = new Date(now()).toISOString();
      const initial = await this.summary();
      const active = initial.activated?.name;
      const valid = initial.profiles.filter((profile) => profile.authValid);
      const activeProfile = valid.find((profile) => profile.name === active);
      if (!active || !activeProfile) {
        this.outcome = 'no_quota';
        this.blockedDetail =
          'The shared Codex login does not match any saved profile, so automatic switching has no account to watch.';
        this.logConclusion({ cause: 'no_active_profile' });
        return;
      }
      const names = valid.map((profile) => profile.name);
      const snapshot = (this.deps.getAuthSnapshot ?? readAuthSnapshot)(names);
      if (
        !snapshot.live ||
        !snapshot.profiles[active] ||
        snapshot.live.email !== activeProfile.email ||
        !snapshot.live.accountId ||
        snapshot.live.accountId !== activeProfile.accountId
      ) {
        this.outcome = 'no_quota';
        this.blockedDetail =
          'The shared Codex login changed outside the dashboard. Automatic switching is paused until it matches a saved profile again.';
        this.logConclusion({ active, cause: 'live_login_mismatch' });
        return;
      }
      const rows = await (this.deps.getRows ?? getCodexProfileQuotaRows)(names);
      const byProfile = new Map(rows.map((row) => [row.profile, row]));
      const assessed = assessRemaining(byProfile.get(active), active, now());
      if (!('remaining' in assessed)) {
        this.outcome = 'no_quota';
        this.blockedDetail =
          assessed.reason === 'stale'
            ? "The active Codex account's usage reading is out of date. Waiting for a new reading before deciding."
            : assessed.reason === 'reset_passed'
              ? "The active Codex account's usage window already reset. Waiting for a new reading before deciding."
              : messages.no_quota;
        this.logConclusion({ active, cause: `no_usable_row:${assessed.reason}` });
        return;
      }
      if (assessed.remaining > config.thresholdPercent) {
        this.outcome = 'healthy';
        this.logConclusion({ active, remaining: assessed.remaining });
        return;
      }
      const candidates = valid
        .filter((profile) => profile.name !== active)
        .map((profile) => ({
          profile,
          remaining: remainingOrNull(
            assessRemaining(byProfile.get(profile.name), profile.name, now())
          ),
        }))
        .filter(
          (candidate): candidate is typeof candidate & { remaining: number } =>
            candidate.remaining !== null && candidate.remaining > config.thresholdPercent
        )
        .sort((a, b) => b.remaining - a.remaining || (a.profile.name < b.profile.name ? -1 : 1));
      const candidate = candidates[0];
      if (!candidate) {
        this.outcome = 'no_candidate';
        this.logConclusion({ active, remaining: assessed.remaining, cause: 'no_healthy_other' });
        return;
      }
      const current = await this.summary();
      const target = current.profiles.find((profile) => profile.name === candidate.profile.name);
      const currentActive = current.profiles.find((profile) => profile.name === active);
      const currentSnapshot = (this.deps.getAuthSnapshot ?? readAuthSnapshot)(names);
      if (
        current.activated?.name !== active ||
        currentActive?.email !== activeProfile.email ||
        currentActive?.accountId !== activeProfile.accountId ||
        snapshot.live.fingerprint !== currentSnapshot.live?.fingerprint ||
        snapshot.profiles[active] !== currentSnapshot.profiles[active] ||
        !snapshot.profiles[candidate.profile.name] ||
        snapshot.profiles[candidate.profile.name] !==
          currentSnapshot.profiles[candidate.profile.name] ||
        !target?.authValid ||
        target.email !== candidate.profile.email ||
        target.accountId !== candidate.profile.accountId ||
        remainingOrNull(assessRemaining(byProfile.get(active), active, now())) === null ||
        remainingOrNull(assessRemaining(byProfile.get(target.name), target.name, now())) === null
      ) {
        this.outcome = 'scheduled';
        this.logConclusion({
          active,
          candidate: candidate.profile.name,
          cause: 'decision_changed_before_activation',
        });
        return;
      }
      // No awaits between the last persisted enable check and starting activation.
      // A disabled/replaced queued decision cannot reach the activation transaction.
      const finalConfig = this.readConfig();
      if (
        this.stopped ||
        generation !== this.generation ||
        !finalConfig.enabled ||
        finalConfig.thresholdPercent !== config.thresholdPercent
      ) {
        this.outcome = finalConfig.enabled && !this.stopped ? 'scheduled' : 'disabled';
        this.logConclusion({ active, cause: 'settings_changed_before_activation' });
        return;
      }
      this.lastActive = active;
      this.lastCandidate = target.name;
      this.activationInProgress = true;
      this.outcome = 'switching';
      try {
        await (this.deps.activate ?? activateCodexProfile)(target.name);
        this.lastSwitchedAt = new Date(now()).toISOString();
        this.outcome = 'switched';
        this.logConclusion({ active, candidate: target.name });
      } finally {
        this.activationInProgress = false;
      }
    } catch (error) {
      if (error instanceof CodexActivationError && error.code === 'busy') {
        this.outcome = 'waiting_idle';
        this.blockedCandidate = this.lastCandidate;
        this.logConclusion({
          ...(this.lastActive ? { active: this.lastActive } : {}),
          ...(this.lastCandidate ? { candidate: this.lastCandidate } : {}),
          cause: 'activation_busy',
        });
      } else {
        this.outcome = 'error';
        this.retryAfter = now() + ERROR_BACKOFF_MS;
        this.logConclusion({ cause: 'activation_error' });
      }
    }
  }
}

const services = new Map<string, CodexAutoSwitchService>();

export function getCodexAutoSwitchService(): CodexAutoSwitchService {
  const scope = path.resolve(getCcsDir());
  let service = services.get(scope);
  if (!service) {
    service = new CodexAutoSwitchService({ ccsDir: scope });
    services.set(scope, service);
  }
  return service;
}
