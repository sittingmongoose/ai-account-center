import { createHash } from 'crypto';
import { getCcsDir } from '../../utils/config-manager';
import {
  listClaudeDesktopProfiles,
  type ClaudeDesktopProfile,
} from './claude-desktop-profile-service';
import { readClaudeDesktopUsageHistory } from './claude-desktop-transport';

export type ClaudeDesktopPlatform = 'mac' | 'windows';

export interface ClaudeDesktopUtilization {
  fiveHour?: number;
  weekly?: number;
  weeklyOpus?: number;
  weeklySonnet?: number;
  extra?: number;
}

export interface ClaudeDesktopProfileUsage {
  id?: string;
  email: string;
  status: 'cached' | 'needs-sign-in' | 'unavailable';
  cached: true;
  fetchedAt: string;
  sampledAt: string | null;
  utilization: ClaudeDesktopUtilization;
}

export interface ClaudeDesktopUsage {
  platform: ClaudeDesktopPlatform;
  fetchedAt: string;
  profiles: ClaudeDesktopProfileUsage[];
}

const CACHE_TTL_MS = 30_000;
const MAX_CACHE_ENTRIES = 16;
const MAX_HISTORY_BYTES = 1024 * 1024;
const USAGE_FIELDS = {
  fh: 'fiveHour',
  sd: 'weekly',
  so: 'weeklyOpus',
  sn: 'weeklySonnet',
  xu: 'extra',
} as const;

interface CacheEntry {
  expiresAt: number;
  promise: Promise<ClaudeDesktopUsage>;
}

const cache = new Map<string, CacheEntry>();

export function invalidateClaudeDesktopUsageCache(): void {
  cache.clear();
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function validTimestamp(value: unknown): value is number {
  return (
    typeof value === 'number' &&
    Number.isFinite(value) &&
    Number.isInteger(value) &&
    value >= 0 &&
    !Number.isNaN(new Date(value).getTime())
  );
}

/** Project only the newest sample; never merge metrics from older samples. */
function readCachedSample(
  contents: string | null
): Pick<ClaudeDesktopProfileUsage, 'status' | 'sampledAt' | 'utilization'> {
  const missing = { status: 'needs-sign-in' as const, sampledAt: null, utilization: {} };
  const unavailable = { status: 'unavailable' as const, sampledAt: null, utilization: {} };
  if (contents === null) return missing;
  if (Buffer.byteLength(contents, 'utf8') > MAX_HISTORY_BYTES) return unavailable;

  let history: unknown;
  try {
    history = JSON.parse(contents);
  } catch {
    return unavailable;
  }
  if (!isRecord(history) || history.version !== 2 || !Array.isArray(history.samples)) {
    return unavailable;
  }
  if (history.samples.length === 0) return missing;

  let latest: Record<string, unknown> | undefined;
  let latestTime = -1;
  for (const sample of history.samples) {
    if (isRecord(sample) && validTimestamp(sample.t) && sample.t >= latestTime) {
      latest = sample;
      latestTime = sample.t;
    }
  }
  if (!latest || !isRecord(latest.u)) return unavailable;

  const utilization: ClaudeDesktopUtilization = {};
  for (const [source, target] of Object.entries(USAGE_FIELDS)) {
    const value = latest.u[source];
    if (typeof value === 'number' && Number.isFinite(value) && value >= 0) {
      utilization[target] = value;
    }
  }
  if (Object.keys(utilization).length === 0) return unavailable;
  return {
    status: 'cached',
    sampledAt: new Date(latestTime).toISOString(),
    utilization,
  };
}

/**
 * What one profile's usage read found: no launcher for this platform, a remote read that
 * failed, or the remote history text (null when the desktop app has none yet).
 */
export type ClaudeDesktopHistoryRead =
  { state: 'unconfigured' } | { state: 'failed' } | { state: 'read'; history: string | null };

/** The owner reads behind one usage answer; composing them reads nothing more. */
export interface ClaudeDesktopUsageFacts {
  platform: ClaudeDesktopPlatform;
  fetchedAt: string;
  profiles: Array<{
    profile: Pick<ClaudeDesktopProfile, 'id' | 'email'>;
    read: ClaudeDesktopHistoryRead;
  }>;
}

async function readProfileHistory(
  profile: ClaudeDesktopProfile,
  platform: ClaudeDesktopPlatform
): Promise<ClaudeDesktopHistoryRead> {
  const launcher = profile[platform];
  if (!launcher?.sshHost || !launcher.profilePath) return { state: 'unconfigured' };
  try {
    return { state: 'read', history: await readClaudeDesktopUsageHistory(launcher, platform) };
  } catch {
    // Remote errors may contain paths or SSH details; only expose a safe status.
    return { state: 'failed' };
  }
}

/** The usage answer from its owner reads (pure; the M3 Rust service ports it). */
export function composeClaudeDesktopUsage(facts: ClaudeDesktopUsageFacts): ClaudeDesktopUsage {
  const { platform, fetchedAt } = facts;
  return {
    platform,
    fetchedAt,
    profiles: facts.profiles.map(({ profile, read }) => {
      const identity = {
        ...(profile.id === undefined ? {} : { id: profile.id }),
        email: profile.email,
        cached: true as const,
        fetchedAt,
      };
      return read.state === 'read'
        ? { ...identity, ...readCachedSample(read.history) }
        : { ...identity, status: 'unavailable' as const, sampledAt: null, utilization: {} };
    }),
  };
}

/**
 * Read desktop-owned cached usage without requesting live usage or credentials. `compose`
 * turns the owner reads into the answer; the route may compose through the M3 Rust service.
 */
export async function getClaudeDesktopUsage(
  platform: ClaudeDesktopPlatform,
  compose: (
    facts: ClaudeDesktopUsageFacts
  ) => ClaudeDesktopUsage | Promise<ClaudeDesktopUsage> = composeClaudeDesktopUsage
): Promise<ClaudeDesktopUsage> {
  const scope = getCcsDir();
  const profiles = await listClaudeDesktopProfiles();
  const manifestHash = createHash('sha256').update(JSON.stringify(profiles)).digest('hex');
  const key = JSON.stringify([scope, platform, manifestHash]);
  const existing = cache.get(key);
  if (existing && existing.expiresAt > Date.now()) {
    // Keep recently used scopes while bounding the number of retained entries.
    cache.delete(key);
    cache.set(key, existing);
    return existing.promise;
  }
  if (existing) cache.delete(key);

  const fetchedAt = new Date().toISOString();
  const promise = Promise.all(
    profiles.map(async (profile) => ({
      profile: { ...(profile.id === undefined ? {} : { id: profile.id }), email: profile.email },
      read: await readProfileHistory(profile, platform),
    }))
  ).then((reads) => compose({ platform, fetchedAt, profiles: reads }));
  const entry: CacheEntry = { expiresAt: Infinity, promise };
  cache.set(key, entry);
  while (cache.size > MAX_CACHE_ENTRIES) {
    const oldestKey = cache.keys().next().value;
    if (oldestKey === undefined) break;
    cache.delete(oldestKey);
  }
  void promise.then(
    () => {
      if (cache.get(key) === entry) entry.expiresAt = Date.now() + CACHE_TTL_MS;
    },
    () => {
      // A composition that failed is never served from the cache.
      if (cache.get(key) === entry) cache.delete(key);
    }
  );
  return promise;
}
