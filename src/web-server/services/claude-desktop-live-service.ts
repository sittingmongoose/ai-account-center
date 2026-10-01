import { execFile } from 'child_process';
import { createHash } from 'crypto';
import { getCcsDir } from '../../utils/config-manager';
import { listClaudeDesktopProfiles } from './claude-desktop-profile-service';
import type { DashboardAccountWindow } from './account-dashboard-types';

/** Identity-bound quota, independent of the computer selected for launching the account. */
export interface ClaudeDesktopLiveUsage {
  profileId: string;
  email: string;
  platform: 'windows';
  source: 'Claude Desktop live quota on Windows';
  plan: 'max' | 'pro' | null;
  fetchedAt: string;
  windows: DashboardAccountWindow[];
}

const PROFILE_IDS = new Set(['gmail', 'platyr', 'party', 'me']);
const CACHE_TTL_MS = 120_000;
const FAILURE_BACKOFF_MS = 30_000;
const PROCESS_TIMEOUT_MS = 20_000;
const MAX_OUTPUT_BYTES = 64 * 1024;
const MAX_CACHE_ENTRIES = 16;
const MAX_RESET_GRANTS = 20;
const RESET_GRANT_KEY = /^reset_credit_(available|saved|used|expired)_grant_([1-9]|1[0-9]|20)$/;
const SOURCE = 'Claude Desktop live quota on Windows' as const;
const WINDOW_FIELDS = {
  five_hour: { label: 'Five-hour usage', minutes: 300 },
  seven_day: { label: 'Weekly usage', minutes: 10_080 },
  seven_day_opus: { label: 'Weekly Opus usage', minutes: 10_080 },
  seven_day_sonnet: { label: 'Weekly Sonnet usage', minutes: 10_080 },
  seven_day_oauth_apps: { label: 'Weekly OAuth app usage', minutes: 10_080 },
  seven_day_cowork: { label: 'Weekly Cowork usage', minutes: 10_080 },
} as const;

interface CacheEntry {
  expiresAt: number;
  pending: boolean;
  failed: boolean;
  promise: Promise<ClaudeDesktopLiveUsage | null>;
}

const cache = new Map<string, CacheEntry>();

export function invalidateClaudeDesktopLiveUsageCache(): void {
  cache.clear();
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isPresent(value: unknown): boolean {
  return value !== null && value !== undefined;
}

function timestamp(value: unknown): string | null {
  if (
    typeof value !== 'string' ||
    value.length > 64 ||
    !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/.test(value)
  ) {
    return null;
  }
  const milliseconds = Date.parse(value);
  if (!Number.isFinite(milliseconds)) return null;
  const instant = new Date(milliseconds);
  if (instant.getUTCFullYear() < 2000 || instant.getUTCFullYear() > 2200) return null;
  return instant.toISOString();
}

/** Reconstruct the public DTO rather than forwarding the helper's object or arbitrary text. */
function normalizeUsage(
  contents: string,
  profileId: string,
  expectedEmail: string
): ClaudeDesktopLiveUsage | null {
  if (Buffer.byteLength(contents, 'utf8') > MAX_OUTPUT_BYTES) return null;
  let result: unknown;
  try {
    result = JSON.parse(contents);
  } catch {
    return null;
  }
  if (
    !isRecord(result) ||
    result.schemaVersion !== 1 ||
    result.provider !== 'claude' ||
    result.profileId !== profileId ||
    result.platform !== 'windows' ||
    result.status !== 'ok' ||
    result.email !== expectedEmail ||
    result.accountVerified !== true ||
    result.organizationVerified !== true ||
    !Array.isArray(result.windows) ||
    result.windows.length > Object.keys(WINDOW_FIELDS).length + MAX_RESET_GRANTS + 3
  ) {
    return null;
  }
  const fetchedAt = timestamp(result.fetchedAt);
  if (!fetchedAt || Math.abs(Date.parse(fetchedAt) - Date.now()) > CACHE_TTL_MS) return null;
  const windows: DashboardAccountWindow[] = [];
  const seen = new Set<string>();
  for (const item of result.windows) {
    if (
      !isRecord(item) ||
      typeof item.key !== 'string' ||
      (item.key !== 'extra_usage' &&
        item.key !== 'reset_credits_available' &&
        item.key !== 'prepaid_balance' &&
        !RESET_GRANT_KEY.test(item.key) &&
        !Object.prototype.hasOwnProperty.call(WINDOW_FIELDS, item.key)) ||
      seen.has(item.key)
    ) {
      return null;
    }
    seen.add(item.key);
    const resetAt =
      item.resetAt === null || item.resetAt === undefined ? null : timestamp(item.resetAt);
    if (isPresent(item.resetAt) && resetAt === null) return null;
    const expiresAt = isPresent(item.expiresAt) ? timestamp(item.expiresAt) : null;
    if (isPresent(item.expiresAt) && expiresAt === null) return null;
    const usedPercent =
      typeof item.usedPercent === 'number' &&
      Number.isFinite(item.usedPercent) &&
      item.usedPercent >= 0
        ? item.usedPercent
        : null;
    const grant = RESET_GRANT_KEY.exec(item.key);
    if (item.key === 'reset_credits_available' || item.key === 'prepaid_balance' || grant) {
      const remaining = nonnegative(item.remaining);
      const used = nonnegative(item.used);
      const limit = nonnegative(item.limit);
      const resetCredit = item.key !== 'prepaid_balance';
      const unit = resetCredit
        ? item.unit === 'resets'
          ? 'resets'
          : null
        : item.unit === 'credits' || (typeof item.unit === 'string' && /^[A-Z]{3}$/.test(item.unit))
          ? item.unit
          : null;
      if (
        item.kind !== 'balance' ||
        remaining === null ||
        unit === null ||
        isPresent(item.usedPercent) ||
        isPresent(item.remainingPercent) ||
        resetAt !== null ||
        (isPresent(item.used) && used === null) ||
        (isPresent(item.limit) && limit === null) ||
        (isPresent(item.enabled) && typeof item.enabled !== 'boolean') ||
        (resetCredit &&
          (!Number.isInteger(remaining) ||
            remaining > 50 ||
            (used !== null && (!Number.isInteger(used) || used > 50)) ||
            (limit !== null && (!Number.isInteger(limit) || limit > 50)))) ||
        (item.key === 'reset_credits_available' &&
          (expiresAt !== null || used !== null || limit !== null))
      ) {
        return null;
      }
      const category = grant?.[1] ?? '';
      const label = grant
        ? `${category.charAt(0).toUpperCase()}${category.slice(1)} rate-limit reset grant ${grant[2]}`
        : item.key === 'reset_credits_available'
          ? 'Rate-limit resets available'
          : unit === 'credits'
            ? 'Prepaid credit balance'
            : 'Prepaid balance';
      windows.push({
        key: item.key,
        label,
        kind: 'balance',
        usedPercent: null,
        remainingPercent: null,
        resetAt: null,
        expiresAt,
        windowMinutes: null,
        used,
        limit,
        remaining,
        unit,
        ...(typeof item.enabled === 'boolean' ? { enabled: item.enabled } : {}),
      });
      continue;
    }
    if (item.key === 'extra_usage') {
      if (isPresent(item.usedPercent) && usedPercent === null) return null;
      if (typeof item.enabled !== 'boolean') return null;
      const used = nonnegative(item.used);
      const limit = nonnegative(item.limit);
      const remaining = nonnegative(item.remaining);
      const unit =
        item.unit === 'credits' || (typeof item.unit === 'string' && /^[A-Z]{3}$/.test(item.unit))
          ? item.unit
          : null;
      if (
        (isPresent(item.used) && used === null) ||
        (isPresent(item.limit) && limit === null) ||
        (isPresent(item.remaining) && remaining === null) ||
        (isPresent(item.unit) && unit === null)
      ) {
        return null;
      }
      windows.push({
        key: 'extra_usage',
        label: 'Extra usage',
        kind: 'extra_usage',
        usedPercent,
        remainingPercent: usedPercent === null ? null : Math.max(0, 100 - usedPercent),
        resetAt,
        expiresAt,
        windowMinutes: null,
        used,
        limit,
        remaining,
        unit,
        enabled: item.enabled,
        ...(typeof item.unlimited === 'boolean' ? { unlimited: item.unlimited } : {}),
      });
      continue;
    }
    if (usedPercent === null) return null;
    const fields = WINDOW_FIELDS[item.key as keyof typeof WINDOW_FIELDS];
    windows.push({
      key: item.key,
      label: fields.label,
      kind: 'rate_limit',
      usedPercent,
      remainingPercent: Math.max(0, 100 - usedPercent),
      resetAt,
      expiresAt,
      windowMinutes: fields.minutes,
      used: null,
      limit: null,
      unit: null,
    });
  }
  if (windows.length === 0) return null;
  return {
    profileId,
    email: expectedEmail,
    platform: 'windows',
    source: SOURCE,
    plan: result.plan === 'max' || result.plan === 'pro' ? result.plan : null,
    fetchedAt,
    windows,
  };
}

function nonnegative(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null;
}

/** Fixed helper path and allowlisted profile only; no credential ever traverses SSH stdout. */
async function runWindowsHelper(sshHost: string, profileId: string): Promise<string> {
  const script = [
    "$ErrorActionPreference = 'Stop'",
    "$env:PYTHONIOENCODING = 'utf-8'",
    "$env:PYTHONUTF8 = '1'",
    "$helper = [IO.Path]::Combine($HOME, '.ccs', 'account-usage', 'claude_usage.py')",
    "$venv = [IO.Path]::Combine($HOME, '.ccs', 'claude-session-migration', 'venv', 'Scripts', 'python.exe')",
    "$python = if (Test-Path -LiteralPath $venv -PathType Leaf) { $venv } else { 'python.exe' }",
    `& $python $helper --provider 'claude' --profile '${profileId}' --platform 'windows'`,
    'exit $LASTEXITCODE',
  ].join('; ');
  const command = `powershell.exe -NoProfile -NonInteractive -EncodedCommand ${Buffer.from(script, 'utf16le').toString('base64')}`;
  return new Promise((resolve, reject) => {
    execFile(
      'ssh',
      [
        '-T',
        '-o',
        'BatchMode=yes',
        '-o',
        'ConnectTimeout=5',
        '-o',
        'ConnectionAttempts=1',
        '-o',
        'ServerAliveInterval=5',
        '-o',
        'ServerAliveCountMax=1',
        '--',
        sshHost,
        command,
      ],
      {
        encoding: 'utf8',
        timeout: PROCESS_TIMEOUT_MS,
        maxBuffer: MAX_OUTPUT_BYTES,
        windowsHide: true,
      },
      (error, stdout) => {
        if (error || Buffer.byteLength(stdout, 'utf8') > MAX_OUTPUT_BYTES) {
          reject(new Error('Claude live quota could not be read.'));
        } else {
          resolve(stdout);
        }
      }
    );
  });
}

/**
 * Read the account's existing Windows Desktop token in its own user context.
 * Errors are intentionally nullable: cached desktop history may still be shown.
 */
export async function getLiveClaudeDesktopUsage(
  profileId: string,
  options: { refresh?: boolean } = {}
): Promise<ClaudeDesktopLiveUsage | null> {
  if (!PROFILE_IDS.has(profileId)) return null;
  try {
    const profiles = await listClaudeDesktopProfiles();
    const profile = profiles.find((candidate) => candidate.id === profileId);
    const sshHost = profile?.windows?.sshHost;
    if (!profile || !sshHost || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(sshHost)) {
      return null;
    }
    const manifestHash = createHash('sha256').update(JSON.stringify(profile)).digest('hex');
    const key = JSON.stringify([getCcsDir(), profileId, manifestHash]);
    const existing = cache.get(key);
    if (
      existing &&
      (existing.pending ||
        (existing.expiresAt > Date.now() && (existing.failed || options.refresh !== true)))
    ) {
      cache.delete(key);
      cache.set(key, existing);
      return existing.promise;
    }
    if (existing) cache.delete(key);
    const promise = runWindowsHelper(sshHost, profileId)
      .then((contents) => normalizeUsage(contents, profileId, profile.email))
      .catch(() => null);
    const entry: CacheEntry = { expiresAt: Infinity, pending: true, failed: false, promise };
    cache.set(key, entry);
    while (cache.size > MAX_CACHE_ENTRIES) {
      const oldestKey = cache.keys().next().value;
      if (oldestKey === undefined) break;
      cache.delete(oldestKey);
    }
    void promise.then((usage) => {
      if (cache.get(key) !== entry) return;
      entry.pending = false;
      entry.failed = usage === null;
      entry.expiresAt = Date.now() + (entry.failed ? FAILURE_BACKOFF_MS : CACHE_TTL_MS);
    });
    return promise;
  } catch {
    return null;
  }
}
