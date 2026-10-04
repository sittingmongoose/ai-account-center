import fs from 'fs';
import path from 'path';
import { nativeBinaryProblem } from './signin-sandbox';

const SHA256 = /^[a-f0-9]{64}$/;

/**
 * Reviewed native versions of the Antigravity CLI (`agy --version`), oldest
 * first. Each entry was reviewed under the product's native review procedure
 * with its release receipt; a new reviewed release appends here.
 */
export const REVIEWED_NATIVE_VERSIONS: readonly string[] = ['1.2.16'];

/** A reviewed version stamp: dotted release with an optional pre-release tail. */
export function isNativeVersion(value: unknown): value is string {
  return (
    typeof value === 'string' && /^\d+\.\d+\.\d+(?:-[\w.-]+)?$/.test(value) && value.length <= 128
  );
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

function readReleasePin(releaseFile: string): {
  nativeSha256: string | null;
  nativeVersion: string | null;
} {
  try {
    const value = JSON.parse(fs.readFileSync(releaseFile, 'utf8')) as Record<string, unknown>;
    return {
      nativeSha256:
        typeof value.nativeSha256 === 'string' && SHA256.test(value.nativeSha256)
          ? value.nativeSha256
          : null,
      nativeVersion: isNativeVersion(value.nativeVersion) ? value.nativeVersion : null,
    };
  } catch {
    return { nativeSha256: null, nativeVersion: null };
  }
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
 * Non-null exactly when an installed CLI no longer matches the reviewed pin:
 * switching is then paused until a new review. Re-reads the version only when
 * the binary changes; never throws and never fails closed into a 500.
 */
export async function readNativeUpdatePaused(
  probe: NativeUpdateProbe
): Promise<NativeUpdatePaused | null> {
  try {
    const home = path.resolve(probe.home);
    const ccsDir = path.resolve(probe.ccsDir);
    const binary = path.join(home, '.local', 'bin', 'agy');
    if (nativeBinaryProblem(home, process.getuid?.() ?? null)) return null;
    const release = readReleasePin(
      probe.releaseFile ?? path.resolve(__dirname, '../../scripts/antigravity/runtime/release.json')
    );
    if (!release.nativeSha256) return null;
    const matches = probe.pinMatches(binary, release.nativeSha256);
    if (matches) return null;
    const statKey = binaryStatKey(binary);
    const cached = pausedCache.get(binary);
    if (cached && statKey && cached.key === statKey) return cached.value;
    const value: NativeUpdatePaused = {
      installedVersion: await readInstalledNativeVersion(home, ccsDir, probe.readVersion),
    };
    if (pausedCache.size >= MAX_CACHE_ENTRIES)
      pausedCache.delete(pausedCache.keys().next().value as string);
    pausedCache.set(binary, { key: statKey ?? '', value });
    return value;
  } catch {
    return null;
  }
}
