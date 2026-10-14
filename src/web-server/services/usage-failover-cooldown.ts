import { createHash, randomUUID } from 'crypto';
import * as fs from 'fs';
import * as path from 'path';
import { isRecord } from './usage-failover-contract';
import type { FailoverProvider, UsageIdentity } from './usage-failover-contract';

const DAY_MS = 86_400_000;
const MAX_BYTES = 1024;
const KEY = /^[a-f0-9]{64}$/;
const inProcessCooldowns = new Map<string, number>();
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
function readPersistedUsageCooldown(ccsDir: string, key: string, now: number): number {
  if (!KEY.test(key)) return 0;
  let descriptor: number | undefined;
  try {
    const destination = file(ccsDir, key);
    const directory = fs.lstatSync(path.dirname(destination));
    if (!directory.isDirectory() || directory.isSymbolicLink() || !privateOwner(directory)) {
      return 0;
    }
    const link = fs.lstatSync(destination);
    if (!link.isFile() || link.isSymbolicLink()) return 0;
    descriptor = fs.openSync(destination, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0));
    const stat = fs.fstatSync(descriptor);
    if (!stat.isFile() || stat.size > MAX_BYTES || !privateOwner(stat)) return 0;
    const buffer = Buffer.alloc(MAX_BYTES + 1);
    const length = fs.readSync(descriptor, buffer, 0, buffer.length, 0);
    if (length > MAX_BYTES) return 0;
    const value: unknown = JSON.parse(buffer.subarray(0, length).toString('utf8'));
    if (
      !isRecord(value) ||
      Object.keys(value).sort().join(',') !== 'key,until,version' ||
      value.version !== 1 ||
      value.key !== key ||
      typeof value.until !== 'number' ||
      !Number.isFinite(value.until) ||
      value.until <= now ||
      value.until > now + DAY_MS
    ) {
      return 0;
    }
    return value.until;
  } catch {
    return 0;
  } finally {
    if (descriptor !== undefined) fs.closeSync(descriptor);
  }
}

export function readUsageCooldown(ccsDir: string, key: string, now: number): number {
  if (!KEY.test(key)) return 0;
  const id = memoryKey(ccsDir, key);
  const memory = inProcessCooldowns.get(id) ?? 0;
  if (memory <= now) inProcessCooldowns.delete(id);
  return Math.max(memory > now ? memory : 0, readPersistedUsageCooldown(ccsDir, key, now));
}

/** Keep the longer provider cooldown; a failed save does not erase the in-memory one. */
export function holdUsageCooldown(
  ccsDir: string,
  key: string,
  requestedUntil: number,
  now: number
): number {
  if (!KEY.test(key) || !Number.isFinite(requestedUntil)) return 0;
  const until = Math.max(
    readUsageCooldown(ccsDir, key, now),
    Math.max(now, Math.min(requestedUntil, now + DAY_MS))
  );
  inProcessCooldowns.set(memoryKey(ccsDir, key), until);
  if (inProcessCooldowns.size > 2048)
    for (const [id, expiry] of inProcessCooldowns) if (expiry <= now) inProcessCooldowns.delete(id);
  const destination = file(ccsDir, key);
  const temporary = `${destination}.${randomUUID()}.tmp`;
  try {
    fs.mkdirSync(path.dirname(destination), { recursive: true, mode: 0o700 });
    const directory = fs.lstatSync(path.dirname(destination));
    if (!directory.isDirectory() || directory.isSymbolicLink() || !privateOwner(directory)) {
      return until;
    }
    fs.writeFileSync(temporary, JSON.stringify({ version: 1, key, until }), {
      mode: 0o600,
      flag: 'wx',
    });
    fs.renameSync(temporary, destination);
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
