import * as fs from 'fs';
import * as path from 'path';
import { createHash, randomUUID } from 'crypto';
import { ValidationError } from '../../errors/error-types';
import { displayText, safePoolId } from '../../antigravity/usage-normalization';
import { DASHBOARD_HOSTS, helperPlatform } from './dashboard-hosts';
import type {
  DashboardAccount,
  DashboardAccountStatus,
  DashboardAccountWindow,
  DashboardPlatform,
} from './account-dashboard-types';

export const ACCOUNT_ANALYTICS_RETENTION_MS = 30 * 24 * 60 * 60 * 1000;
const MAX_HISTORY_BYTES = 32 * 1024 * 1024;
const MAX_RECORDS = 24_000;
const MAX_ACCOUNTS_PER_SNAPSHOT = 64;
const SAMPLE_BUCKET_MS = 5 * 60 * 1000;
const VALID_STATUSES = ['ok', 'cached', 'unavailable', 'error', 'needs_sign_in'];

export interface AccountAnalyticsObservation {
  identity: string;
  accountId: string;
  sampledAt: string;
  observedAt: string;
  platform: DashboardPlatform;
  source: string;
  status: DashboardAccountStatus;
  isActive: boolean;
  windows: DashboardAccountWindow[];
}

export interface AccountAnalyticsHistoryData {
  schemaVersion: 1;
  collectedSince: string;
  records: AccountAnalyticsObservation[];
}

export interface AccountAnalyticsHistoryStore {
  read(): Promise<AccountAnalyticsHistoryData | null>;
  write(data: AccountAnalyticsHistoryData): Promise<void>;
}

function object(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function text(value: unknown, max = 120): string | null {
  if (typeof value !== 'string' || /[\u0000-\u001f\u007f]/.test(value)) return null;
  return value.length > 0 && value.length <= max ? value : null;
}

export function analyticsTimestamp(value: unknown): string | null {
  if (typeof value !== 'string' || !/^\d{4}-\d{2}-\d{2}T/.test(value)) return null;
  const epoch = Date.parse(value);
  return Number.isFinite(epoch) ? new Date(epoch).toISOString() : null;
}

function numeric(value: unknown, percent = false): number | null {
  if (typeof value !== 'number' || !Number.isFinite(value)) return null;
  // Providers can report genuine overage above 100%; chart bounds belong to
  // presentation, while the measured percentage remains intact here.
  return percent && value < 0 ? null : value;
}

const CLEAR_SLUG_PATTERN = /^[a-z0-9_]{1,40}$/;

/** Whitelist data fields before persisting; raw helper bodies never enter history. */
export function analyticsWindow(value: unknown): DashboardAccountWindow | null {
  if (!object(value) || !text(value.key, 64) || !text(value.label, 80)) return null;
  const cachedSampledAt = value.status === 'cached' ? analyticsTimestamp(value.sampledAt) : null;
  // A retained optional window is not a new observation of the refreshed core.
  // Without its original time, it cannot safely enter measured history.
  if (value.status === 'cached' && !cachedSampledAt) return null;
  if (
    value.clears !== undefined &&
    (!Array.isArray(value.clears) ||
      value.clears.length > 8 ||
      value.clears.some((slug) => typeof slug !== 'string' || !CLEAR_SLUG_PATTERN.test(slug)) ||
      new Set(value.clears).size !== value.clears.length)
  )
    return null;
  const kind = ['rate_limit', 'balance', 'spend', 'extra_usage'].includes(String(value.kind))
    ? (value.kind as DashboardAccountWindow['kind'])
    : undefined;
  return {
    key: value.key as string,
    label: value.label as string,
    usedPercent: numeric(value.usedPercent, true),
    remainingPercent: numeric(value.remainingPercent, true),
    resetAt: analyticsTimestamp(value.resetAt),
    windowMinutes: numeric(value.windowMinutes),
    used: numeric(value.used),
    limit: numeric(value.limit),
    unit: text(value.unit, 40),
    ...(kind ? { kind } : {}),
    remaining: numeric(value.remaining),
    expiresAt: analyticsTimestamp(value.expiresAt),
    ...(value.clears !== undefined ? { clears: [...(value.clears as string[])] } : {}),
    ...(typeof value.unlimited === 'boolean' ? { unlimited: value.unlimited } : {}),
    ...(typeof value.enabled === 'boolean' ? { enabled: value.enabled } : {}),
    ...(cachedSampledAt ? { status: 'cached' as const, sampledAt: cachedSampledAt } : {}),
    ...(safePoolId(value.poolId) ? { poolId: value.poolId } : {}),
    ...(value.poolIdSource === 'provider-id' || value.poolIdSource === 'provider-bucket-membership'
      ? { poolIdSource: value.poolIdSource }
      : {}),
    ...(displayText(value.poolLabel, 80)
      ? { poolLabel: displayText(value.poolLabel, 80) as string }
      : {}),
    ...(Array.isArray(value.modelIds)
      ? {
          modelIds: value.modelIds
            .slice(0, 64)
            .filter(
              (model): model is string =>
                typeof model === 'string' &&
                /^[A-Za-z0-9][A-Za-z0-9_.:/@-]{0,159}$/.test(model) &&
                displayText(model, 160) !== null
            ),
        }
      : {}),
  };
}

export function accountAnalyticsIdentity(account: DashboardAccount): string {
  return createHash('sha256')
    .update(JSON.stringify([account.provider, account.id, account.email?.toLowerCase() ?? null]))
    .digest('hex');
}

export function analyticsWindowIdentity(window: DashboardAccountWindow): string {
  const identity: Array<string | null> = [window.key, window.kind ?? null, window.unit];
  // Explicit reported pool membership prevents a changed group from inheriting
  // another group's series. Existing non-pool history keys remain compatible.
  if (window.poolId) identity.push(window.poolId);
  return JSON.stringify(identity);
}

export function sourcePlatform(account: DashboardAccount): DashboardPlatform {
  // Live Claude quota can come from Windows while the launch selector is Mac.
  if (account.host && DASHBOARD_HOSTS.includes(account.host)) return helperPlatform(account.host);
  if (account.source.includes('on Windows')) return 'windows';
  if (account.source.includes('on Mac')) return 'mac';
  return account.platform;
}

/** One stored observation, validated field by field; null when it is not one. */
export function decodeAccountAnalyticsObservation(
  value: unknown
): AccountAnalyticsObservation | null {
  if (!object(value)) return null;
  const sampledAt = analyticsTimestamp(value.sampledAt);
  const observedAt = analyticsTimestamp(value.observedAt);
  if (
    typeof value.identity !== 'string' ||
    !/^[a-f0-9]{64}$/.test(value.identity) ||
    !text(value.accountId, 128) ||
    !sampledAt ||
    !observedAt ||
    !['ubuntu', 'mac', 'windows'].includes(String(value.platform)) ||
    !VALID_STATUSES.includes(String(value.status)) ||
    !text(value.source, 120) ||
    !Array.isArray(value.windows) ||
    value.windows.length > 256
  )
    return null;
  return {
    identity: value.identity,
    accountId: value.accountId as string,
    sampledAt,
    observedAt,
    platform: value.platform as DashboardPlatform,
    source: value.source as string,
    status: value.status as DashboardAccountStatus,
    isActive: value.isActive === true,
    windows: value.windows.flatMap((window) => {
      const safe = analyticsWindow(window);
      return safe ? [safe] : [];
    }),
  };
}

export function decodeAccountAnalyticsHistory(value: unknown): AccountAnalyticsHistoryData | null {
  if (
    !object(value) ||
    value.schemaVersion !== 1 ||
    !analyticsTimestamp(value.collectedSince) ||
    !Array.isArray(value.records) ||
    value.records.length > MAX_RECORDS
  )
    return null;
  return {
    schemaVersion: 1,
    collectedSince: analyticsTimestamp(value.collectedSince) as string,
    records: value.records.flatMap((record) => {
      const safe = decodeAccountAnalyticsObservation(record);
      return safe ? [safe] : [];
    }),
  };
}

function secureRegularFile(stat: fs.Stats): boolean {
  return (
    stat.isFile() &&
    stat.nlink === 1 &&
    (process.getuid?.() === undefined || stat.uid === process.getuid?.())
  );
}

/** Atomic owner-only storage, with no secrets, symlink following, or raw logs. */
export class FileAccountAnalyticsHistoryStore implements AccountAnalyticsHistoryStore {
  private readonly directory: string;
  private readonly file: string;

  constructor(ccsDir: string) {
    this.directory = path.join(ccsDir, 'account-analytics');
    this.file = path.join(this.directory, 'quota-history-v1.json');
  }

  async read(): Promise<AccountAnalyticsHistoryData | null> {
    try {
      const directory = await fs.promises.lstat(this.directory);
      if (
        !directory.isDirectory() ||
        directory.isSymbolicLink() ||
        (process.getuid?.() !== undefined && directory.uid !== process.getuid?.())
      )
        throw new ValidationError('Account history directory is not private storage');
      const stat = await fs.promises.lstat(this.file);
      if (!secureRegularFile(stat) || stat.size > MAX_HISTORY_BYTES)
        throw new ValidationError('Account history is not a bounded regular file');
      const handle = await fs.promises.open(
        this.file,
        fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0)
      );
      try {
        const actual = await handle.stat();
        if (!secureRegularFile(actual) || actual.size > MAX_HISTORY_BYTES)
          throw new ValidationError('Account history is not a bounded regular file');
        const decoded = decodeAccountAnalyticsHistory(JSON.parse(await handle.readFile('utf8')));
        if (!decoded) throw new ValidationError('Account history schema is unavailable');
        return decoded;
      } finally {
        await handle.close();
      }
    } catch (error) {
      if (object(error) && error.code === 'ENOENT') return null;
      throw error;
    }
  }

  /** A killed writer cannot run its finally block. Reap only its old UUID temporary files. */
  async cleanupTemporaries(now = Date.now()): Promise<number> {
    let removedBytes = 0;
    try {
      const directory = await fs.promises.lstat(this.directory);
      if (
        !directory.isDirectory() ||
        directory.isSymbolicLink() ||
        (process.getuid?.() !== undefined && directory.uid !== process.getuid?.())
      )
        return 0;
      const prefix = `${path.basename(this.file)}.`;
      for (const entry of await fs.promises.readdir(this.directory)) {
        if (!entry.startsWith(prefix)) continue;
        const suffix = entry.slice(prefix.length);
        if (
          !/^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}\.tmp$/.test(suffix)
        )
          continue;
        const temporary = path.join(this.directory, entry);
        try {
          const stat = await fs.promises.lstat(temporary);
          if (!secureRegularFile(stat) || now - stat.mtimeMs < 24 * 60 * 60 * 1000) continue;
          await fs.promises.unlink(temporary);
          removedBytes += stat.size;
        } catch {
          // Another writer may have published or removed its own file meanwhile.
        }
      }
    } catch {
      // Cleanup never makes a valid history import or write fail.
    }
    return removedBytes;
  }

  async write(data: AccountAnalyticsHistoryData): Promise<void> {
    await fs.promises.mkdir(this.directory, { recursive: true, mode: 0o700 });
    const directory = await fs.promises.lstat(this.directory);
    if (
      !directory.isDirectory() ||
      directory.isSymbolicLink() ||
      (process.getuid?.() !== undefined && directory.uid !== process.getuid?.())
    )
      throw new ValidationError('Account history directory is not private storage');
    await fs.promises.chmod(this.directory, 0o700);
    try {
      if (!secureRegularFile(await fs.promises.lstat(this.file)))
        throw new ValidationError('Account history is not a regular private file');
    } catch (error) {
      if (!object(error) || error.code !== 'ENOENT') throw error;
    }
    await this.cleanupTemporaries();
    const records = data.records.slice(-MAX_RECORDS);
    let payload = JSON.stringify({ ...data, records });
    while (Buffer.byteLength(payload) > MAX_HISTORY_BYTES && records.length > 1) {
      records.splice(0, Math.max(1, Math.ceil(records.length / 10)));
      payload = JSON.stringify({ ...data, records });
    }
    if (Buffer.byteLength(payload) > MAX_HISTORY_BYTES)
      throw new ValidationError('Account history sample exceeded storage budget');
    const temporary = `${this.file}.${randomUUID()}.tmp`;
    const handle = await fs.promises.open(temporary, 'wx', 0o600);
    try {
      await handle.writeFile(payload, 'utf8');
      await handle.sync();
      await handle.close();
      await fs.promises.rename(temporary, this.file);
    } catch (error) {
      await handle.close().catch(() => {});
      await fs.promises.unlink(temporary).catch(() => {});
      throw error;
    }
  }
}

/** Cached samples are not rerecorded as fresh history on every screen refresh. */
export function appendAccountAnalyticsSnapshot(
  history: AccountAnalyticsHistoryData | null,
  accounts: DashboardAccount[],
  now: number
): AccountAnalyticsHistoryData {
  const nowIso = new Date(now).toISOString();
  const cutoff = now - ACCOUNT_ANALYTICS_RETENTION_MS;
  const records = (history?.records ?? []).filter(
    (record) => Date.parse(record.sampledAt) >= cutoff && Date.parse(record.sampledAt) <= now
  );
  const latest = new Map<string, AccountAnalyticsObservation>();
  for (const record of records) latest.set(record.identity, record);
  for (const account of accounts.slice(0, MAX_ACCOUNTS_PER_SNAPSHOT)) {
    const identity = accountAnalyticsIdentity(account);
    const previous = latest.get(identity);
    const healthy = account.status === 'ok' || account.status === 'cached';
    const windows = healthy
      ? account.windows.slice(0, 256).flatMap((window) => {
          const safe = analyticsWindow(window);
          if (safe?.status === 'cached') {
            const actualSampleTime = Date.parse(safe.sampledAt ?? '');
            if (actualSampleTime > now || actualSampleTime < cutoff) return [];
          }
          return safe ? [safe] : [];
        })
      : [];
    const sampledAt = healthy
      ? (analyticsTimestamp(account.sampledAt) ?? analyticsTimestamp(account.fetchedAt))
      : nowIso;
    if (!sampledAt || Date.parse(sampledAt) > now || Date.parse(sampledAt) < cutoff) continue;
    // An initial unavailable row is not a measured zero. Keep current status in
    // the dashboard; retain explicit gaps only after a genuine observation.
    if (windows.length === 0 && !previous) continue;
    const candidate: AccountAnalyticsObservation = {
      identity,
      accountId: account.id,
      sampledAt,
      observedAt: nowIso,
      platform: sourcePlatform(account),
      source: account.source,
      status: account.status,
      isActive: account.isActive,
      windows,
    };
    if (previous) {
      if (Date.parse(sampledAt) < Date.parse(previous.sampledAt)) continue;
      const sameSample =
        previous.sampledAt === sampledAt &&
        JSON.stringify(previous.windows) === JSON.stringify(windows) &&
        previous.source === candidate.source &&
        previous.platform === candidate.platform &&
        previous.isActive === candidate.isActive;
      if (
        sameSample ||
        (windows.length === 0 &&
          previous.windows.length === 0 &&
          previous.status === candidate.status)
      )
        continue;
      // Preserve a reset, balance change, active switch, or gap immediately;
      // otherwise retain at most one unchanged reading per five-minute bucket.
      if (
        JSON.stringify(previous.windows) === JSON.stringify(windows) &&
        previous.isActive === candidate.isActive &&
        previous.source === candidate.source &&
        previous.platform === candidate.platform &&
        Math.floor(Date.parse(previous.sampledAt) / SAMPLE_BUCKET_MS) ===
          Math.floor(Date.parse(sampledAt) / SAMPLE_BUCKET_MS)
      )
        continue;
    }
    records.push(candidate);
    latest.set(identity, candidate);
  }
  records.sort((a, b) => Date.parse(a.sampledAt) - Date.parse(b.sampledAt));
  // Keep the last hour's changes intact, recent history at five-minute
  // resolution, and older history hourly. This fits thirty days for the
  // fourteen configured accounts without summing or averaging quotas.
  const retained = new Map<string, AccountAnalyticsObservation>();
  records.forEach((record, index) => {
    const age = now - Date.parse(record.sampledAt);
    const interval = age <= 3_600_000 ? 0 : age <= 86_400_000 ? SAMPLE_BUCKET_MS : 3_600_000;
    const bucket = interval === 0 ? index : Math.floor(Date.parse(record.sampledAt) / interval);
    retained.set(`${record.identity}:${interval}:${bucket}`, record);
  });
  return {
    schemaVersion: 1,
    collectedSince: history?.collectedSince ?? nowIso,
    records: [...retained.values()]
      .sort((a, b) => Date.parse(a.sampledAt) - Date.parse(b.sampledAt))
      .slice(-MAX_RECORDS),
  };
}
