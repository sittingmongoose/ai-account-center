import { createHash, randomBytes, timingSafeEqual } from 'crypto';
import fs from 'fs/promises';
import path from 'path';
import {
  authDirectory,
  ensureAuthDirectory,
  withAuthWriteGate,
} from '../services/dashboard-auth-files';
import { readPrivateJsonFile, writePrivateJsonFile } from '../services/private-json-store';

/**
 * The T3 usage hub's management key. Only its SHA-256 is stored, in
 * `~/.ccs/auth/usage-hub-key.json` (folder 0700, file 0600, atomic writes
 * through the dashboard's auth write gate), like paired-device token hashes.
 * The key itself exists only in the output of `generate`/`rotate`, so a leaked
 * file does not reveal it. No file, or a file that is unreadable, unsafe or
 * malformed, means the hub is off.
 *
 * Keys are `aacu_` plus 43 base64url characters (32 random bytes).
 */
export const USAGE_HUB_KEY_FILE = 'usage-hub-key.json';
export const USAGE_HUB_KEY_PREFIX = 'aacu_';
const KEY_SHAPE = /^aacu_[A-Za-z0-9_-]{43}$/;
const MAX_FILE_BYTES = 4096;
/** Longer presented values are refused without hashing. */
const MAX_PRESENTED_LENGTH = 512;

export interface UsageHubKeyRecord {
  version: 1;
  keySha256: string;
  createdAt: string;
}

export type UsageHubKeyState =
  | { state: 'off' }
  | { state: 'invalid' }
  | { state: 'on'; record: UsageHubKeyRecord };

export function usageHubKeyPath(): string {
  return path.join(authDirectory(), USAGE_HUB_KEY_FILE);
}

export function hashUsageHubKey(key: string): string {
  return createHash('sha256').update(key, 'utf8').digest('hex');
}

/** A short, non-reversible fingerprint for status output (first 8 hex of the stored hash). */
export function usageHubKeyFingerprint(record: UsageHubKeyRecord): string {
  return record.keySha256.slice(0, 8);
}

function parseRecord(value: unknown): UsageHubKeyRecord | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  if (record.version !== 1) return null;
  if (typeof record.keySha256 !== 'string' || !/^[0-9a-f]{64}$/.test(record.keySha256)) return null;
  if (typeof record.createdAt !== 'string' || !Number.isFinite(Date.parse(record.createdAt)))
    return null;
  return { version: 1, keySha256: record.keySha256, createdAt: record.createdAt };
}

export async function readUsageHubKeyState(): Promise<UsageHubKeyState> {
  const read = await readPrivateJsonFile(usageHubKeyPath(), MAX_FILE_BYTES);
  if (read.state === 'absent') return { state: 'off' };
  if (read.state === 'invalid') return { state: 'invalid' };
  const record = parseRecord(read.value);
  return record ? { state: 'on', record } : { state: 'invalid' };
}

/**
 * Constant-time check of a presented key against the stored hash: both sides
 * are 32-byte SHA-256 digests, so the comparison time never depends on how
 * much of the key matched or on its length.
 */
export function usageHubKeyMatches(presented: string, record: UsageHubKeyRecord): boolean {
  const expected = Buffer.from(record.keySha256, 'hex');
  const bounded = presented.length <= MAX_PRESENTED_LENGTH ? presented : '';
  const actual = createHash('sha256').update(bounded, 'utf8').digest();
  const same = timingSafeEqual(actual, expected);
  return same && bounded.length > 0;
}

export function newUsageHubKey(): string {
  return `${USAGE_HUB_KEY_PREFIX}${randomBytes(32).toString('base64url')}`;
}

export function isUsageHubKeyShape(value: string): boolean {
  return KEY_SHAPE.test(value);
}

export interface UsageHubKeyWriteOptions {
  /** Replace an existing key (rotate). Without it an existing key is kept and an error thrown. */
  replace: boolean;
  now?: () => Date;
  generate?: () => string;
}

export class UsageHubKeyExistsError extends Error {
  constructor() {
    super('A usage hub key already exists.');
    this.name = 'UsageHubKeyExistsError';
  }
}

/** Creates (or with `replace`, rotates) the key; returns the new key, which is never stored. */
export function writeUsageHubKey(options: UsageHubKeyWriteOptions): Promise<string> {
  return withAuthWriteGate(async () => {
    await ensureAuthDirectory();
    const current = await readUsageHubKeyState();
    if (current.state === 'on' && !options.replace) throw new UsageHubKeyExistsError();
    const key = (options.generate ?? newUsageHubKey)();
    const record: UsageHubKeyRecord = {
      version: 1,
      keySha256: hashUsageHubKey(key),
      createdAt: (options.now ?? (() => new Date()))().toISOString(),
    };
    await writePrivateJsonFile(usageHubKeyPath(), record);
    return key;
  });
}

/** Turns the hub off by removing the stored hash. Returns whether a file was removed. */
export function removeUsageHubKey(): Promise<boolean> {
  return withAuthWriteGate(async () => {
    try {
      await fs.rm(usageHubKeyPath());
      return true;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') return false;
      throw error;
    }
  });
}
