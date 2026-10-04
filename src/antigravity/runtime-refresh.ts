import fs from 'fs';
import path from 'path';
import {
  defaultNativeReleaseFile,
  readInstalledNativeVersion,
  readNativeRelease,
  type ReviewedNative,
} from './native-version';
import {
  readInstalledAntigravityRuntime,
  verifyOwnedAntigravityNativePin,
} from './production-runtime';

/**
 * Re-verifies the installed native CLI against the packaged reviewed set and
 * re-pins the install descriptor when the CLI moved to a reviewed build. Used
 * by the explicit `antigravity runtime refresh` command and by the automatic
 * refresh inside the activation release gate. Never throws.
 */
export interface RuntimeRefreshRequest {
  ccsDir: string;
  home: string;
  releaseFile?: string;
  /** Defaults to the owned-pin verifier; fixtures inject a pure decision. */
  pinMatches?: (binary: string, sha256: string) => boolean;
  /** Defaults to the fixed `agy --version` read; read only when unreviewed. */
  readVersion?: () => Promise<string | null>;
}

export type RuntimeRefreshResult =
  | { status: 'current'; version: string; sha256: string }
  | { status: 'refreshed'; version: string; sha256: string; previousSha256: string }
  | { status: 'unreviewed'; installedVersion: string | null }
  | { status: 'no-installation' }
  | { status: 'gate-closed' }
  | { status: 'failed' };

interface RefusalCacheEntry {
  key: string;
  installedVersion: string | null;
}
const refusalCache = new Map<string, RefusalCacheEntry>();
const MAX_CACHE_ENTRIES = 8;

function binaryStatKey(binary: string): string | null {
  try {
    const stat = fs.lstatSync(binary);
    return `${stat.dev}:${stat.ino}:${stat.mtimeMs}:${stat.size}`;
  } catch {
    return null;
  }
}

/** Exact owned descriptor bytes with adopt-grade checks, or null. */
function readDescriptorBytes(file: string, directory: string): Buffer | null {
  try {
    const parent = fs.lstatSync(directory);
    const before = fs.lstatSync(file);
    const uid = process.getuid?.();
    if (
      !parent.isDirectory() ||
      parent.isSymbolicLink() ||
      (parent.mode & 0o777) !== 0o700 ||
      !before.isFile() ||
      before.isSymbolicLink() ||
      before.nlink !== 1 ||
      (before.mode & 0o777) !== 0o600 ||
      parent.uid !== uid ||
      before.uid !== uid ||
      before.size > 8192
    )
      return null;
    const fd = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
    let raw: Buffer;
    try {
      const opened = fs.fstatSync(fd);
      if (opened.dev !== before.dev || opened.ino !== before.ino) return null;
      raw = fs.readFileSync(fd);
    } finally {
      fs.closeSync(fd);
    }
    const after = fs.lstatSync(file);
    if (after.dev !== before.dev || after.ino !== before.ino || raw.length > 8192) return null;
    return raw;
  } catch {
    return null;
  }
}

function bundleGateOpen(bundleDirectory: string): boolean {
  try {
    const value = JSON.parse(
      fs.readFileSync(path.join(bundleDirectory, 'lib', 'release.json'), 'utf8')
    ) as Record<string, unknown>;
    return (
      value.nativeActivationReleased === true &&
      typeof value.nativeProofReceiptSha256 === 'string' &&
      /^[a-f0-9]{64}$/.test(value.nativeProofReceiptSha256)
    );
  } catch {
    return false;
  }
}

function ensureBackupDirectory(directory: string): boolean {
  try {
    fs.mkdirSync(directory, { mode: 0o700, recursive: true });
    const info = fs.lstatSync(directory);
    return (
      info.isDirectory() &&
      !info.isSymbolicLink() &&
      info.uid === process.getuid?.() &&
      (info.mode & 0o777) === 0o700
    );
  } catch {
    return false;
  }
}

function writeFileExclusive(file: string, raw: Buffer): boolean {
  let fd: number | null = null;
  try {
    fd = fs.openSync(
      file,
      fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL,
      0o600
    );
    fs.writeFileSync(fd, raw);
    fs.fsyncSync(fd);
    return true;
  } catch {
    return false;
  } finally {
    if (fd !== null) {
      try {
        fs.closeSync(fd);
      } catch {
        return false;
      }
    }
  }
}

function fsyncDirectory(directory: string): void {
  const fd = fs.openSync(directory, fs.constants.O_RDONLY);
  try {
    fs.fsyncSync(fd);
  } finally {
    fs.closeSync(fd);
  }
}

let tempCounter = 0;

export async function refreshRuntimeDescriptor(
  request: RuntimeRefreshRequest
): Promise<RuntimeRefreshResult> {
  const failed: RuntimeRefreshResult = { status: 'failed' };
  try {
    const ccsDir = path.resolve(request.ccsDir);
    const home = path.resolve(request.home);
    const pinMatches = request.pinMatches ?? verifyOwnedAntigravityNativePin;
    const release = readNativeRelease(request.releaseFile ?? defaultNativeReleaseFile());
    if (!release.gateOpen) return { status: 'gate-closed' };
    if (!release.reviewed.length) return failed;
    const directory = path.join(ccsDir, 'antigravity-switching');
    const file = path.join(directory, 'runtime-installation.json');
    const binary = path.join(home, '.local', 'bin', 'agy');
    const setKey = release.reviewed.map((entry) => `${entry.version}:${entry.sha256}`).join(',');
    const statKey = binaryStatKey(binary);
    const cacheKey = statKey ? `${statKey}|${setKey}` : null;
    const refused = refusalCache.get(binary);
    if (refused && cacheKey && refused.key === cacheKey)
      return { status: 'unreviewed', installedVersion: refused.installedVersion };

    const matchReviewed = (): ReviewedNative | null => {
      for (const entry of release.reviewed) {
        if (pinMatches(binary, entry.sha256)) return entry;
      }
      return null;
    };
    const refuse = async (): Promise<RuntimeRefreshResult> => {
      const installedVersion = await readInstalledNativeVersion(home, ccsDir, request.readVersion);
      if (refusalCache.size >= MAX_CACHE_ENTRIES)
        refusalCache.delete(refusalCache.keys().next().value as string);
      refusalCache.set(binary, { key: cacheKey ?? '', installedVersion });
      return { status: 'unreviewed', installedVersion };
    };

    // At most two passes: the second re-reads after a conflicting rival
    // write. A conflict on the second pass fails instead of retrying.
    for (let pass = 0; pass < 2; pass++) {
      const rawBefore = readDescriptorBytes(file, directory);
      const record = readInstalledAntigravityRuntime(ccsDir, home);
      if (!rawBefore || !record) return { status: 'no-installation' };
      if (record.nativeBinary !== binary) return { status: 'no-installation' };
      if (!bundleGateOpen(record.bundleDirectory)) return { status: 'gate-closed' };
      let matched: ReviewedNative | null;
      try {
        matched = pinMatches(binary, record.nativeSha256)
          ? (release.reviewed.find((entry) => entry.sha256 === record.nativeSha256) ?? null)
          : matchReviewed();
      } catch {
        return failed;
      }
      if (!matched) return refuse();
      if (matched.sha256 === record.nativeSha256) {
        const settled = readDescriptorBytes(file, directory);
        if (!settled || !settled.equals(rawBefore)) continue;
        refusalCache.delete(binary);
        return { status: 'current', version: matched.version, sha256: matched.sha256 };
      }
      const next = Buffer.from(`${JSON.stringify({ ...record, nativeSha256: matched.sha256 })}\n`);
      const temp = path.join(
        directory,
        `.runtime-installation.json.tmp-${process.pid}-${tempCounter++}`
      );
      if (!writeFileExclusive(temp, next)) return failed;
      try {
        const preRename = readDescriptorBytes(file, directory);
        if (!preRename || !preRename.equals(rawBefore)) continue;
        const backups = path.join(directory, 'descriptor-backups');
        if (!ensureBackupDirectory(backups)) return failed;
        const backup = path.join(backups, `${record.nativeSha256}.json`);
        if (fs.existsSync(backup)) {
          const kept = readDescriptorBytes(backup, backups);
          if (!kept || !kept.equals(rawBefore)) return failed;
        } else if (!writeFileExclusive(backup, rawBefore)) {
          return failed;
        }
        const finalCheck = readDescriptorBytes(file, directory);
        if (!finalCheck || !finalCheck.equals(rawBefore)) continue;
        fs.renameSync(temp, file);
        fsyncDirectory(directory);
        const observed = readDescriptorBytes(file, directory);
        if (!observed || !observed.equals(next)) {
          try {
            if (fs.readFileSync(backup).equals(rawBefore)) fs.rmSync(backup);
          } catch {
            // A leftover identical backup is harmless; a changed one is kept.
          }
          return failed;
        }
        refusalCache.delete(binary);
        return {
          status: 'refreshed',
          version: matched.version,
          sha256: matched.sha256,
          previousSha256: record.nativeSha256,
        };
      } finally {
        try {
          fs.rmSync(temp, { force: true });
        } catch {
          // A leftover temp file is inert; the descriptor was never touched.
        }
      }
    }
    return failed;
  } catch {
    return failed;
  }
}
