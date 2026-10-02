import * as fs from 'fs';
import * as path from 'path';
import { randomUUID } from 'crypto';
import { ConfigError } from '../../errors/error-types';
import { CLAUDE_PROFILE_ID_PATTERN } from './claude-desktop-profile-service';
import type { ClaudeDesktopLiveUsage } from './claude-desktop-live-service';

const MAX_BYTES = 64 * 1024;
const DIRECTORY = 'claude-desktop-live-cache';

function owned(stat: fs.Stats): boolean {
  return process.getuid?.() === undefined || stat.uid === process.getuid();
}

function privateMode(stat: fs.Stats, mode: number): boolean {
  return process.platform === 'win32' || (stat.mode & 0o777) === mode;
}

function regularFile(stat: fs.Stats): boolean {
  return stat.isFile() && stat.nlink === 1 && owned(stat) && privateMode(stat, 0o600);
}

async function privateDirectory(directory: string, create: boolean): Promise<void> {
  if (create) await fs.promises.mkdir(directory, { recursive: true, mode: 0o700 });
  const stat = await fs.promises.lstat(directory);
  if (!stat.isDirectory() || stat.isSymbolicLink() || !owned(stat) || !privateMode(stat, 0o700))
    throw new ConfigError('Claude quota cache is unavailable.');
}

function cachePath(ccsDir: string, profileId: string, manifestHash: string): string | null {
  // Manifest membership is enforced by the caller, which resolves the profile
  // and binds the snapshot to its manifest hash; this only rejects unsafe names.
  if (!CLAUDE_PROFILE_ID_PATTERN.test(profileId) || !/^[a-f0-9]{64}$/.test(manifestHash))
    return null;
  return path.join(ccsDir, DIRECTORY, `${profileId}.json`);
}

/** Read only a bounded owner-only public quota snapshot; callers validate its DTO. */
export async function readClaudeDesktopLiveSnapshot(
  ccsDir: string,
  profileId: string,
  manifestHash: string
): Promise<unknown | null> {
  const file = cachePath(ccsDir, profileId, manifestHash);
  if (!file) return null;
  try {
    await privateDirectory(path.dirname(file), false);
    const before = await fs.promises.lstat(file);
    if (!regularFile(before) || before.size > MAX_BYTES) return null;
    const handle = await fs.promises.open(
      file,
      fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0)
    );
    try {
      const stat = await handle.stat();
      if (
        !regularFile(stat) ||
        stat.size > MAX_BYTES ||
        stat.ino !== before.ino ||
        stat.dev !== before.dev
      )
        return null;
      // The fixed buffer also bounds allocation if a file grows after stat().
      const bytes = Buffer.alloc(MAX_BYTES + 1);
      let count = 0;
      while (count < bytes.length) {
        const { bytesRead } = await handle.read(bytes, count, bytes.length - count, count);
        if (bytesRead === 0) break;
        count += bytesRead;
      }
      if (count > MAX_BYTES) return null;
      const record: unknown = JSON.parse(bytes.subarray(0, count).toString('utf8'));
      if (
        typeof record !== 'object' ||
        record === null ||
        Array.isArray(record) ||
        (record as Record<string, unknown>).schemaVersion !== 1 ||
        (record as Record<string, unknown>).manifestHash !== manifestHash
      )
        return null;
      return (record as Record<string, unknown>).sample ?? null;
    } finally {
      await handle.close();
    }
  } catch {
    return null;
  }
}

/** Atomically retain normalized public data only; cache failure never drops live quota. */
export async function writeClaudeDesktopLiveSnapshot(
  ccsDir: string,
  profileId: string,
  manifestHash: string,
  sample: ClaudeDesktopLiveUsage
): Promise<void> {
  const file = cachePath(ccsDir, profileId, manifestHash);
  if (!file) return;
  let temporary: string | undefined;
  try {
    const publicSample = {
      profileId: sample.profileId,
      email: sample.email,
      platform: sample.platform,
      source: sample.source,
      plan: sample.plan,
      fetchedAt: sample.fetchedAt,
      ...(typeof sample.sourceContextFingerprint === 'string' &&
      /^[a-f0-9]{64}$/.test(sample.sourceContextFingerprint)
        ? { sourceContextFingerprint: sample.sourceContextFingerprint }
        : {}),
      ...(sample.optionalExtras &&
      ['ok', 'unavailable'].includes(sample.optionalExtras.resetCredits) &&
      ['ok', 'unavailable'].includes(sample.optionalExtras.prepaidBalance)
        ? {
            optionalExtras: {
              resetCredits: sample.optionalExtras.resetCredits,
              prepaidBalance: sample.optionalExtras.prepaidBalance,
            },
          }
        : {}),
      windows: sample.windows.map((window) => ({
        key: window.key,
        label: window.label,
        usedPercent: window.usedPercent,
        remainingPercent: window.remainingPercent,
        resetAt: window.resetAt,
        windowMinutes: window.windowMinutes,
        used: window.used,
        limit: window.limit,
        unit: window.unit,
        kind: window.kind,
        remaining: window.remaining,
        expiresAt: window.expiresAt,
        unlimited: window.unlimited,
        enabled: window.enabled,
        ...(window.status === 'cached' &&
        typeof window.sampledAt === 'string' &&
        window.sampledAt.length <= 64 &&
        /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/.test(
          window.sampledAt
        ) &&
        Number.isFinite(Date.parse(window.sampledAt))
          ? { status: 'cached' as const, sampledAt: window.sampledAt }
          : {}),
      })),
    };
    const contents = JSON.stringify({ schemaVersion: 1, manifestHash, sample: publicSample });
    if (Buffer.byteLength(contents, 'utf8') > MAX_BYTES) return;
    await privateDirectory(path.dirname(file), true);
    try {
      if (!regularFile(await fs.promises.lstat(file))) return;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') return;
    }
    temporary = path.join(path.dirname(file), `.${profileId}-${randomUUID()}.tmp`);
    await fs.promises.writeFile(temporary, contents, { mode: 0o600, flag: 'wx' });
    await fs.promises.rename(temporary, file);
    temporary = undefined;
  } catch {
    // No exception text, filenames, or helper bodies are exposed.
  } finally {
    if (temporary) await fs.promises.unlink(temporary).catch(() => undefined);
  }
}
