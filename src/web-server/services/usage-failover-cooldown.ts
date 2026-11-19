import { privateFileOwners } from '../../shared/private-files';
import { writeRustCollectionCache } from '../rust-service-transport';
import { createHash, randomUUID } from 'crypto';
import * as fs from 'fs';
import * as path from 'path';
import { MAX_CLAUDE_429_STREAK } from './usage-failover-claude-cadence';
import { isRecord } from './usage-failover-contract';
import type { FailoverProvider, UsageIdentity } from './usage-failover-contract';

const DAY_MS = 86_400_000;
const MAX_BYTES = 1024;
const KEY = /^[a-f0-9]{64}$/;
interface UsageCooldown {
  until: number;
  claude429Streak?: number;
}

const inProcessCooldowns = new Map<string, UsageCooldown>();
const pendingCooldownWrites = new Map<string, Promise<void>>();
const memoryKey = (ccsDir: string, key: string) => JSON.stringify([path.resolve(ccsDir), key]);

/** Native and alternate collectors use the same private provider/account gate. */
export function identityUsageCooldownKey(
  ccsDir: string,
  provider: FailoverProvider,
  identity: UsageIdentity
): string {
  return createHash('sha256')
    .update(JSON.stringify([path.resolve(ccsDir), provider, identity]))
    .digest('hex');
}

export function readIdentityUsageCooldown(
  ccsDir: string,
  provider: FailoverProvider,
  identity: UsageIdentity,
  now: number
): number {
  return readUsageCooldown(ccsDir, identityUsageCooldownKey(ccsDir, provider, identity), now);
}

export function holdIdentityUsageCooldown(
  ccsDir: string,
  provider: FailoverProvider,
  identity: UsageIdentity,
  requestedUntil: number,
  now: number
): number {
  return holdUsageCooldown(
    ccsDir,
    identityUsageCooldownKey(ccsDir, provider, identity),
    requestedUntil,
    now
  );
}

function file(ccsDir: string, key: string): string {
  return path.join(ccsDir, 'account-usage-failover', `cooldown-${key}.json`);
}

function privateOwner(stat: fs.Stats): boolean {
  return (
    process.platform === 'win32' || ((stat.mode & 0o077) === 0 && stat.uid === process.getuid?.())
  );
}

/** Only a bounded, owner-held cooldown may suppress a request after restart. */
function readPersistedUsageCooldown(
  ccsDir: string,
  key: string,
  now: number
): UsageCooldown | null {
  let descriptor: number | undefined;
  try {
    const destination = file(ccsDir, key);
    const directory = fs.lstatSync(path.dirname(destination));
    if (!directory.isDirectory() || directory.isSymbolicLink() || !privateOwner(directory)) {
      return null;
    }
    const link = fs.lstatSync(destination);
    if (!link.isFile() || link.isSymbolicLink()) return null;
    descriptor = fs.openSync(destination, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0));
    const stat = fs.fstatSync(descriptor);
    if (!stat.isFile() || stat.size > MAX_BYTES || !privateOwner(stat)) return null;
    const buffer = Buffer.alloc(MAX_BYTES + 1);
    const length = fs.readSync(descriptor, buffer, 0, buffer.length, 0);
    if (length > MAX_BYTES) return null;
    const value: unknown = JSON.parse(buffer.subarray(0, length).toString('utf8'));
    if (
      !isRecord(value) ||
      Object.keys(value).some(
        (field) => !['key', 'until', 'version', 'claude429Streak'].includes(field)
      ) ||
      value.version !== 1 ||
      value.key !== key ||
      typeof value.until !== 'number' ||
      !Number.isFinite(value.until) ||
      value.until < 0 ||
      value.until > now + DAY_MS
    ) {
      return null;
    }
    const streak = value.claude429Streak;
    return {
      until: value.until,
      ...(typeof streak === 'number' &&
      Number.isInteger(streak) &&
      streak >= 0 &&
      streak <= MAX_CLAUDE_429_STREAK
        ? { claude429Streak: streak }
        : {}),
    };
  } catch {
    return null;
  } finally {
    if (descriptor !== undefined) fs.closeSync(descriptor);
  }
}

export function readUsageCooldownState(ccsDir: string, key: string, now: number): UsageCooldown {
  if (!KEY.test(key)) return { until: 0 };
  const id = memoryKey(ccsDir, key);
  const memory = inProcessCooldowns.get(id);
  const saved = readPersistedUsageCooldown(ccsDir, key, now);
  const until = Math.max(memory?.until ?? 0, saved?.until ?? 0);
  const claude429Streak = memory?.claude429Streak ?? saved?.claude429Streak;
  // Expiry opens the gate but ends no rate-limit streak. A verified read clears it.
  if (memory && memory.until <= now && !memory.claude429Streak) inProcessCooldowns.delete(id);
  return { until: until > now ? until : 0, claude429Streak };
}

export function readUsageCooldown(ccsDir: string, key: string, now: number): number {
  return readUsageCooldownState(ccsDir, key, now).until;
}

/** Wait for the persisted hold before handing a helper slot to another account. */
export async function settleUsageCooldown(ccsDir: string, key: string): Promise<void> {
  await pendingCooldownWrites.get(memoryKey(ccsDir, key));
}

/** Keep the longer provider cooldown; a failed save does not erase the in-memory one. */
export function holdUsageCooldown(
  ccsDir: string,
  key: string,
  requestedUntil: number,
  now: number,
  claude429Streak?: number
): number {
  if (!KEY.test(key) || !Number.isFinite(requestedUntil)) return 0;
  const previous = readUsageCooldownState(ccsDir, key, now);
  const until = Math.max(previous.until, Math.max(now, Math.min(requestedUntil, now + DAY_MS)));
  const record = { until, claude429Streak: claude429Streak ?? previous.claude429Streak };
  const id = memoryKey(ccsDir, key);
  inProcessCooldowns.set(id, record);
  if (inProcessCooldowns.size > 2048)
    for (const [id, entry] of inProcessCooldowns)
      if (entry.until <= now) inProcessCooldowns.delete(id);
  if (privateFileOwners()['account-usage-cooldown'] === 'rust-service') {
    const pending = writeRustCollectionCache(
      ccsDir,
      'account-usage-cooldown',
      key,
      JSON.stringify({ now, ...record })
    ).then((saved) => {
      if (saved && inProcessCooldowns.get(id) === record) inProcessCooldowns.set(id, { until });
    });
    pendingCooldownWrites.set(id, pending);
    void pending.finally(() => {
      if (pendingCooldownWrites.get(id) === pending) pendingCooldownWrites.delete(id);
    });
    return until;
  }
  const destination = file(ccsDir, key);
  const temporary = `${destination}.${randomUUID()}.tmp`;
  try {
    fs.mkdirSync(path.dirname(destination), { recursive: true, mode: 0o700 });
    const directory = fs.lstatSync(path.dirname(destination));
    if (!directory.isDirectory() || directory.isSymbolicLink() || !privateOwner(directory)) {
      return until;
    }
    fs.writeFileSync(temporary, JSON.stringify({ version: 1, key, ...record }), {
      mode: 0o600,
      flag: 'wx',
    });
    fs.renameSync(temporary, destination);
    // On success the file owns the streak. The in-process record backs failed writes.
    inProcessCooldowns.set(id, { until });
  } catch {
    // The caller still applies the returned cooldown when its storage is read-only.
  } finally {
    try {
      fs.unlinkSync(temporary);
    } catch {
      // The file was renamed or never created.
    }
  }
  return until;
}
