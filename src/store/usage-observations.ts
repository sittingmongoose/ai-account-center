/**
 * Dashboard rows and stored JSON observations become the store's `usage.sampled` payload
 * (docs/store.md 2.2). The whitelisting is the JSON history's own (`analyticsWindow`): raw helper
 * bodies never enter the store. Which observations are new is decided by the store.
 */
import {
  accountAnalyticsIdentity,
  analyticsTimestamp,
  analyticsWindow,
  analyticsWindowIdentity,
  sourcePlatform,
  type AccountAnalyticsObservation,
} from '../web-server/services/account-analytics-history';
import type {
  DashboardAccount,
  DashboardAccountWindow,
} from '../web-server/services/account-dashboard-types';

export const MAX_ACCOUNTS_PER_SAMPLE = 64;
const MAX_WINDOWS = 256;

export interface StoreWindowObservation {
  identity: string;
  key: string;
  label: string;
  kind: string | null;
  unit: string | null;
  used_pct: number | null;
  remaining_pct: number | null;
  used: number | null;
  limit: number | null;
  remaining: number | null;
  reset_at_ms: number | null;
  expires_at_ms: number | null;
  window_minutes: number | null;
  unlimited: boolean | null;
  enabled: boolean | null;
  cached_sampled_at_ms: number | null;
  pool_id: string | null;
  pool_id_source: string | null;
  pool_label: string | null;
  model_ids: string[] | null;
}

export interface StoreObservation {
  account: {
    identity: string;
    provider: string;
    account_id: string;
    plan: string | null;
    platform: string;
    source: string;
    status: string;
    is_active: boolean;
  };
  sampled_at_ms: number;
  observed_at_ms: number;
  windows: StoreWindowObservation[];
  import_ref: string | null;
}

function ms(value: string | null | undefined): number | null {
  const epoch = Date.parse(value ?? '');
  return Number.isFinite(epoch) ? epoch : null;
}

/** One whitelisted window in the store's shape. */
export function storeWindow(window: DashboardAccountWindow): StoreWindowObservation | null {
  const safe = analyticsWindow(window);
  if (!safe) return null;
  return {
    identity: analyticsWindowIdentity(safe),
    key: safe.key,
    label: safe.label,
    kind: safe.kind ?? null,
    unit: safe.unit ?? null,
    used_pct: safe.usedPercent ?? null,
    remaining_pct: safe.remainingPercent ?? null,
    used: safe.used ?? null,
    limit: safe.limit ?? null,
    remaining: safe.remaining ?? null,
    reset_at_ms: ms(safe.resetAt),
    expires_at_ms: ms(safe.expiresAt),
    window_minutes: safe.windowMinutes ?? null,
    unlimited: typeof safe.unlimited === 'boolean' ? safe.unlimited : null,
    enabled: typeof safe.enabled === 'boolean' ? safe.enabled : null,
    cached_sampled_at_ms: safe.status === 'cached' ? ms(safe.sampledAt) : null,
    pool_id: safe.poolId ?? null,
    pool_id_source: safe.poolIdSource ?? null,
    pool_label: safe.poolLabel ?? null,
    model_ids: Array.isArray(safe.modelIds) ? safe.modelIds : null,
  };
}

/**
 * The live sample of one dashboard row, with the JSON history's per-account rules: a failed row
 * records its status with no windows at the sampling time; a retained (cached) window keeps its
 * own measured time and only when it falls inside the retention; no time in the future.
 */
export function storeObservationFromAccount(
  account: DashboardAccount,
  now: number,
  retentionMs: number
): StoreObservation | null {
  const cutoff = now - retentionMs;
  const healthy = account.status === 'ok' || account.status === 'cached';
  const windows = healthy
    ? account.windows.slice(0, MAX_WINDOWS).flatMap((window) => {
        const stored = storeWindow(window);
        if (!stored) return [];
        const cached = stored.cached_sampled_at_ms;
        if (cached !== null && (cached > now || cached < cutoff)) return [];
        return [stored];
      })
    : [];
  const sampledIso = healthy
    ? (analyticsTimestamp(account.sampledAt) ?? analyticsTimestamp(account.fetchedAt))
    : new Date(now).toISOString();
  const sampled = ms(sampledIso);
  if (sampled === null || sampled > now || sampled < cutoff) return null;
  return {
    account: {
      identity: accountAnalyticsIdentity(account),
      provider: account.provider,
      account_id: account.id.slice(0, 128),
      plan: account.plan ? account.plan.slice(0, 80) : null,
      platform: sourcePlatform(account),
      source: account.source.slice(0, 120),
      status: account.status,
      is_active: account.isActive,
    },
    sampled_at_ms: sampled,
    observed_at_ms: now,
    windows,
    import_ref: null,
  };
}

/** One observation of the JSON history (or its archive), for the backfill. */
export function storeObservationFromRecord(
  record: AccountAnalyticsObservation,
  provider: string,
  importRef: string
): StoreObservation | null {
  const sampled = ms(record.sampledAt);
  const observed = ms(record.observedAt);
  if (sampled === null || observed === null) return null;
  return {
    account: {
      identity: record.identity,
      provider,
      account_id: record.accountId,
      plan: null,
      platform: record.platform,
      source: record.source,
      status: record.status,
      is_active: record.isActive,
    },
    sampled_at_ms: sampled,
    observed_at_ms: observed,
    windows: record.windows.flatMap((window) => {
      const stored = storeWindow(window);
      return stored ? [stored] : [];
    }),
    import_ref: importRef,
  };
}
