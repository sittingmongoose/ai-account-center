import { AntigravityError } from './errors';
import { antigravityPlanDisplay } from './plan';
import type {
  AntigravityPoolWindow,
  AntigravityDashboardAccount,
  AntigravityUsageProfile,
  AntigravityPublicProfile,
} from './usage-contract';

export function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

export function safeId(value: unknown): value is string {
  return typeof value === 'string' && /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(value);
}

/** Pool IDs are lookup keys, never profile names or filesystem paths. */
export function safePoolId(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    /^[A-Za-z0-9][A-Za-z0-9._:@/-]{0,127}$/.test(value) &&
    displayText(value, 128) !== null
  );
}
function safeProviderId(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    /^[A-Za-z0-9][A-Za-z0-9_.:/@-]{0,159}$/.test(value) &&
    displayText(value, 160) !== null
  );
}

export function displayText(value: unknown, maximum = 160): string | null {
  if (
    typeof value !== 'string' ||
    value.length > maximum ||
    /[\u0000-\u001f\u007f]/.test(value) ||
    /(?:bearer\s|dca:|sk[-_]|eyJ[A-Za-z0-9_-]{8}|https?:\/\/|[{}]|access_token|refresh_token|client_secret)/i.test(
      value
    )
  )
    return null;
  return value.trim() || null;
}

export function email(value: unknown): string | null {
  const candidate = displayText(value, 254);
  return candidate && /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(candidate) ? candidate : null;
}

export function timestamp(value: unknown): string | null {
  if (
    typeof value !== 'string' ||
    value.length > 40 ||
    !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/.test(value)
  )
    return null;
  const parsed = new Date(value);
  return Number.isFinite(parsed.getTime()) &&
    parsed.getUTCFullYear() >= 2000 &&
    parsed.getUTCFullYear() <= 2200
    ? parsed.toISOString()
    : null;
}

function nonnegative(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null;
}

function percent(value: unknown): number | null {
  const parsed = nonnegative(value);
  return parsed !== null && parsed <= 100 ? parsed : null;
}

export function poolWindow(value: unknown): AntigravityPoolWindow | null {
  if (!record(value) || !safeProviderId(value.key)) return null;
  const usedPercent = percent(value.usedPercent);
  const remainingPercent = percent(value.remainingPercent);
  const used = nonnegative(value.used);
  const limit = nonnegative(value.limit);
  const remaining = nonnegative(value.remaining);
  const resetAt = timestamp(value.resetAt);
  const expiresAt = timestamp(value.expiresAt);
  if (
    usedPercent === null &&
    remainingPercent === null &&
    used === null &&
    limit === null &&
    remaining === null &&
    resetAt === null &&
    expiresAt === null &&
    typeof value.unlimited !== 'boolean' &&
    typeof value.enabled !== 'boolean'
  )
    return null;
  const kind = ['rate_limit', 'balance', 'spend', 'extra_usage'].includes(String(value.kind))
    ? (value.kind as AntigravityPoolWindow['kind'])
    : 'rate_limit';
  const reportedMinutes = nonnegative(value.windowMinutes);
  const modelIds = Array.isArray(value.modelIds)
    ? [...new Set(value.modelIds.filter((id): id is string => safeProviderId(id)))].slice(0, 64)
    : [];
  return {
    key: value.key,
    label:
      value.key === 'google-ai-credits'
        ? 'AI credits (overage)'
        : (displayText(value.label, 80) ?? 'Model quota'),
    usedPercent: usedPercent ?? (remainingPercent === null ? null : 100 - remainingPercent),
    remainingPercent: remainingPercent ?? (usedPercent === null ? null : 100 - usedPercent),
    resetAt,
    windowMinutes:
      reportedMinutes !== null && reportedMinutes > 0 && reportedMinutes <= 525600
        ? reportedMinutes
        : null,
    used,
    limit,
    unit: displayText(value.unit, 24),
    kind,
    ...(Object.prototype.hasOwnProperty.call(value, 'remaining') ? { remaining } : {}),
    ...(Object.prototype.hasOwnProperty.call(value, 'expiresAt') ? { expiresAt } : {}),
    ...(typeof value.enabled === 'boolean' ? { enabled: value.enabled } : {}),
    ...(typeof value.unlimited === 'boolean' ? { unlimited: value.unlimited } : {}),
    ...(safePoolId(value.poolId) ? { poolId: value.poolId } : {}),
    ...(safePoolId(value.poolId) &&
    (value.poolIdSource === 'provider-id' || value.poolIdSource === 'provider-bucket-membership')
      ? { poolIdSource: value.poolIdSource }
      : {}),
    ...(safePoolId(value.poolId) && displayText(value.poolLabel, 80)
      ? { poolLabel: displayText(value.poolLabel, 80) as string }
      : {}),
    ...(modelIds.length ? { modelIds } : {}),
  };
}

/** Build a fresh allowlisted public object, never spread a private registry row. */
export function publicProfile(profile: AntigravityUsageProfile): AntigravityPublicProfile {
  if (!safeId(profile.id) || !email(profile.email))
    throw new AntigravityError('Invalid account identity.');
  return {
    id: profile.id,
    email: email(profile.email) as string,
    plan: antigravityPlanDisplay(displayText(profile.plan, 80), profile.verifiedAt).plan,
    available: profile.identityVerified === true && profile.available === true,
    selected: profile.identityVerified === true && profile.selected === true,
    runtimeVerified: profile.identityVerified === true && profile.runtimeVerified === true,
    verifiedAt: timestamp(profile.verifiedAt),
    hostId: 'ubuntu',
  };
}

/** The public message of an `identity_unbound` row. */
export const IDENTITY_UNBOUND_MESSAGE =
  'Antigravity usage could not be matched to this saved account.';

export function publicDashboardAccount(
  value: AntigravityDashboardAccount
): AntigravityDashboardAccount {
  const id = value.capabilities?.antigravityProfileId;
  if (
    !safeId(id) ||
    value.id !== `antigravity:profile:${id}` ||
    value.provider !== 'antigravity' ||
    value.platform !== 'ubuntu' ||
    !email(value.email)
  )
    throw new AntigravityError('Invalid Antigravity account.');
  const status = ['ok', 'cached', 'needs_sign_in', 'unavailable', 'error'].includes(value.status)
    ? value.status
    : 'error';
  // The only reason kept: it is meaningful on an error row alone.
  const identityUnbound = status === 'error' && value.statusReason === 'identity_unbound';
  return {
    id: value.id,
    provider: 'antigravity',
    providerLabel: 'Antigravity',
    label: email(value.email) as string,
    email: email(value.email),
    ...antigravityPlanDisplay(
      displayText(value.plan, 80),
      timestamp(value.sampledAt) ?? timestamp(value.fetchedAt)
    ),
    platform: 'ubuntu',
    source: 'Antigravity saved login on Ubuntu',
    status,
    ...(identityUnbound ? { statusReason: 'identity_unbound' as const } : {}),
    message:
      status === 'ok'
        ? null
        : status === 'cached'
          ? 'Showing the last successful Antigravity usage reading.'
          : status === 'needs_sign_in'
            ? 'This Antigravity profile needs to sign in again.'
            : identityUnbound
              ? IDENTITY_UNBOUND_MESSAGE
              : 'Antigravity usage is temporarily unavailable.',
    fetchedAt: timestamp(value.fetchedAt),
    sampledAt: timestamp(value.sampledAt),
    isActive: value.isActive === true,
    windows: (Array.isArray(value.windows) ? value.windows : [])
      .slice(0, 256)
      .map(poolWindow)
      .filter((window): window is AntigravityPoolWindow => window !== null),
    capabilities: {
      codexProfile: null,
      claudeProfileId: null,
      claudePlatforms: [],
      antigravityProfileId: id,
      ...(typeof value.capabilities.antigravityCanActivate === 'boolean'
        ? { antigravityCanActivate: value.capabilities.antigravityCanActivate }
        : {}),
      antigravityHostIds: value.capabilities.antigravityHostIds?.includes('ubuntu')
        ? ['ubuntu']
        : [],
    },
  };
}
