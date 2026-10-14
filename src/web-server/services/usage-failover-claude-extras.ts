import { HEX_DIGEST, type UsageReading } from './usage-failover-contract';

const MAX_AGE_MS = 24 * 60 * 60 * 1000;
const RESET_GRANT_KEY = /^reset_credit_(available|saved|used|expired)_grant_([1-9]|1[0-9]|20)$/;

function group(key: string): 'resetCredits' | 'prepaidBalance' | null {
  if (key === 'prepaid_balance') return 'prepaidBalance';
  return key === 'reset_credits_available' || RESET_GRANT_KEY.test(key) ? 'resetCredits' : null;
}

/** Reapply the retained-extra age bound on cached responses and after restart. */
export function projectClaudeFailoverExtras<T extends Pick<UsageReading, 'windows'>>(
  reading: T,
  now: number
): T {
  const windows = reading.windows.filter((window) => {
    if (window.status !== 'cached' || group(window.key) === null) return true;
    const sampledAt = Date.parse(window.sampledAt ?? '');
    return Number.isFinite(sampledAt) && sampledAt <= now && now - sampledAt <= MAX_AGE_MS;
  });
  return windows.length === reading.windows.length ? reading : { ...reading, windows };
}

/**
 * Keep only explicitly unavailable optional groups for the same verified source.
 * The caller must compare candidate identity, revision, locator and host. The
 * private context also binds the owned browser capsule to that native sign-in.
 */
export function retainClaudeFailoverExtras(
  previous: UsageReading | null,
  next: UsageReading,
  sameSource: boolean,
  now: number
): UsageReading {
  if (
    !sameSource ||
    !previous ||
    previous.email !== next.email ||
    previous.plan !== next.plan ||
    !next.optionalExtrasContext ||
    !HEX_DIGEST.test(next.optionalExtrasContext) ||
    next.optionalExtrasContext !== previous.optionalExtrasContext ||
    !next.fetchedAt
  )
    return next;
  const fetchedAt = Date.parse(next.fetchedAt);
  if (!Number.isFinite(fetchedAt)) return next;
  const reported = new Set(next.windows.map((window) => group(window.key)));
  const retained = previous.windows.flatMap((window) => {
    const kind = group(window.key);
    const sampledAt = window.sampledAt ?? previous.fetchedAt;
    const timestamp = Date.parse(sampledAt ?? '');
    if (
      kind === null ||
      next.optionalExtras?.[kind] !== 'unavailable' ||
      reported.has(kind) ||
      !sampledAt ||
      !Number.isFinite(timestamp) ||
      now - timestamp > MAX_AGE_MS ||
      timestamp > now ||
      timestamp > fetchedAt
    )
      return [];
    return [{ ...window, status: 'cached' as const, sampledAt }];
  });
  return retained.length ? { ...next, windows: [...next.windows, ...retained] } : next;
}
