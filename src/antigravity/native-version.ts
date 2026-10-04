import fs from 'fs';
import path from 'path';
import { nativeBinaryProblem } from './signin-sandbox';

const SHA256 = /^[a-f0-9]{64}$/;

/** A reviewed version stamp: dotted release with an optional pre-release tail. */
export function isNativeVersion(value: unknown): value is string {
  return (
    typeof value === 'string' && /^\d+\.\d+\.\d+(?:-[\w.-]+)?$/.test(value) && value.length <= 128
  );
}

/** One reviewed native build: `agy --version` plus the sha256 of its bytes. */
export interface ReviewedNative {
  version: string;
  sha256: string;
}

export interface NativeRelease {
  gateOpen: boolean;
  /** The reviewed set, oldest first; empty when the release file is unusable. */
  reviewed: ReviewedNative[];
}

function reviewedEntry(value: unknown): ReviewedNative | null {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  const keys = Object.keys(record);
  if (keys.length !== 2 || !keys.includes('nativeVersion') || !keys.includes('nativeSha256'))
    return null;
  if (!isNativeVersion(record.nativeVersion)) return null;
  if (typeof record.nativeSha256 !== 'string' || !SHA256.test(record.nativeSha256)) return null;
  return { version: record.nativeVersion, sha256: record.nativeSha256 };
}

/**
 * The packaged release's gate and reviewed set. A release without
 * `reviewedNatives` reads as its single current pin; anything malformed reads
 * as a closed gate with an empty set. Never throws.
 */
export function readNativeRelease(releaseFile: string): NativeRelease {
  const closed: NativeRelease = { gateOpen: false, reviewed: [] };
  try {
    const raw = fs.readFileSync(releaseFile);
    if (raw.length > 65536) return closed;
    const value = JSON.parse(raw.toString('utf8')) as Record<string, unknown>;
    const gateOpen =
      value.nativeActivationReleased === true &&
      typeof value.nativeProofReceiptSha256 === 'string' &&
      SHA256.test(value.nativeProofReceiptSha256);
    const reviewed: ReviewedNative[] = [];
    if (value.reviewedNatives === undefined) {
      const single = reviewedEntry({
        nativeVersion: value.nativeVersion,
        nativeSha256: value.nativeSha256,
      });
      if (single) reviewed.push(single);
    } else {
      if (!Array.isArray(value.reviewedNatives) || value.reviewedNatives.length > 16)
        return { gateOpen, reviewed: [] };
      for (const entry of value.reviewedNatives) {
        const parsed = reviewedEntry(entry);
        if (!parsed) return { gateOpen, reviewed: [] };
        reviewed.push(parsed);
      }
    }
    const versions = new Set(reviewed.map((entry) => entry.version));
    const hashes = new Set(reviewed.map((entry) => entry.sha256));
    if (versions.size !== reviewed.length || hashes.size !== reviewed.length)
      return { gateOpen, reviewed: [] };
    return { gateOpen, reviewed };
  } catch {
    return closed;
  }
}

export interface NativeUpdatePaused {
  installedVersion: string | null;
}

export interface NativeUpdateProbe {
  home: string;
  ccsDir: string;
  releaseFile?: string;
  /** Callers pass the owned-pin verifier; the probe owns no trust decision. */
  pinMatches: (binary: string, sha256: string) => boolean;
  readVersion?: () => Promise<string | null>;
}

interface CacheEntry {
  key: string;
  value: NativeUpdatePaused | null;
}
const pausedCache = new Map<string, CacheEntry>();
const MAX_CACHE_ENTRIES = 8;

function binaryStatKey(binary: string): string | null {
  try {
    const stat = fs.lstatSync(binary);
    return `${stat.dev}:${stat.ino}:${stat.mtimeMs}:${stat.size}`;
  } catch {
    return null;
  }
}

export function defaultNativeReleaseFile(): string {
  return path.resolve(__dirname, '../../scripts/antigravity/runtime/release.json');
}

/**
 * The installed CLI version, or null when it cannot be read. Spawns only
 * `agy --version` with auto-update disabled; never throws.
 */
export async function readInstalledNativeVersion(
  home: string,
  ccsDir: string,
  readVersion?: () => Promise<string | null>
): Promise<string | null> {
  try {
    // Imported at call time: the fixed version command transits through the
    // production runtime graph, so a static import would close a module cycle.
    const { createManagedNativeUpdate } = await import('./managed-update-command');
    const version = readVersion
      ? await readVersion()
      : await createManagedNativeUpdate({
          nativeBinary: path.join(path.resolve(home), '.local', 'bin', 'agy'),
          ccsDirectory: path.resolve(ccsDir),
        }).version();
    return isNativeVersion(version) ? version : null;
  } catch {
    return null;
  }
}

/**
 * Non-null exactly when an installed CLI matches no reviewed build: switching
 * is then paused until a new review. Both verdicts are cached by binary
 * identity, so polling never re-hashes an unchanged binary; never throws and
 * never fails closed into a 500.
 */
export async function readNativeUpdatePaused(
  probe: NativeUpdateProbe
): Promise<NativeUpdatePaused | null> {
  try {
    const home = path.resolve(probe.home);
    const ccsDir = path.resolve(probe.ccsDir);
    const binary = path.join(home, '.local', 'bin', 'agy');
    if (nativeBinaryProblem(home, process.getuid?.() ?? null)) return null;
    const release = readNativeRelease(probe.releaseFile ?? defaultNativeReleaseFile());
    if (!release.reviewed.length) return null;
    const statKey = binaryStatKey(binary);
    const setKey = release.reviewed.map((entry) => `${entry.version}:${entry.sha256}`).join(',');
    const cacheKey = statKey ? `${statKey}|${setKey}` : null;
    const cached = pausedCache.get(binary);
    if (cached && cacheKey && cached.key === cacheKey) return cached.value;
    let matched = false;
    for (const entry of release.reviewed) {
      if (probe.pinMatches(binary, entry.sha256)) {
        matched = true;
        break;
      }
    }
    const value: NativeUpdatePaused | null = matched
      ? null
      : { installedVersion: await readInstalledNativeVersion(home, ccsDir, probe.readVersion) };
    if (pausedCache.size >= MAX_CACHE_ENTRIES)
      pausedCache.delete(pausedCache.keys().next().value as string);
    pausedCache.set(binary, { key: cacheKey ?? '', value });
    return value;
  } catch {
    return null;
  }
}
