import * as fs from 'fs';
import * as path from 'path';
import { claimWriterLease, type FileLease } from '../../shared/private-file-lease';
import { getResolvedLoggingConfig } from './log-config';
import {
  ensureLoggingDirectories,
  getCurrentLogPath,
  buildArchiveLogPath,
  getLogArchiveDir,
  getNativeLogsDir,
} from './log-paths';
import { pushRecentLogEntry } from './log-buffer';
import { shouldWriteLogLevel, type LogEntry } from './log-types';

// Includes old .gz archives and new uncompressed rotations.
export const LOG_ARCHIVE_BYTE_CAP = 32 * 1024 * 1024;
const ONE_DAY_MS = 24 * 60 * 60 * 1000;
const PRUNE_INTERVAL_MS = 60 * 1000;
const WRITER_WAIT_MS = 500;
const waitWord = new Int32Array(new SharedArrayBuffer(4));
let lastPruneAt = 0;

function getRotateBytes(rotateMb: number): number {
  return Math.max(1, rotateMb) * 1024 * 1024;
}

function logWriter(): FileLease {
  const logs = getNativeLogsDir();
  const lock = path.join(logs, 'current.jsonl.writer.lock');
  const deadline = performance.now() + WRITER_WAIT_MS;
  for (;;) {
    try {
      return claimWriterLease(path.dirname(logs), lock);
    } catch (error) {
      if (performance.now() >= deadline) throw error;
      Atomics.wait(waitWord, 0, 0, 5);
    }
  }
}

/**
 * Finish a rotation that linked the current file into the archive and then stopped before
 * the unlink. The archive keeps the bytes; prune must not charge the inode per name.
 */
function finishInterruptedRotation(): void {
  const currentLogPath = getCurrentLogPath();
  let stats: fs.Stats;
  try {
    stats = fs.lstatSync(currentLogPath);
  } catch {
    return;
  }
  if (!stats.isFile() || stats.nlink < 2) return;
  const archiveDir = getLogArchiveDir();
  let names: string[];
  try {
    names = fs.readdirSync(archiveDir);
  } catch {
    return;
  }
  const archived = names.some((name) => {
    try {
      const linked = fs.lstatSync(path.join(archiveDir, name));
      return linked.isFile() && linked.dev === stats.dev && linked.ino === stats.ino;
    } catch {
      return false;
    }
  });
  if (!archived) return;
  try {
    fs.unlinkSync(currentLogPath);
  } catch {
    /* The next writer retries the same check. */
  }
}

function rotateCurrentLogIfNeeded(): void {
  finishInterruptedRotation();
  const config = getResolvedLoggingConfig();
  const currentLogPath = getCurrentLogPath();

  if (!fs.existsSync(currentLogPath)) {
    return;
  }

  const stats = fs.lstatSync(currentLogPath);
  if (!stats.isFile()) return;
  const ageMs = Date.now() - stats.mtimeMs;
  const exceedsSize = stats.size >= getRotateBytes(config.rotate_mb);
  const exceedsAge = ageMs >= ONE_DAY_MS;
  if (!exceedsSize && !exceedsAge) {
    return;
  }

  if (stats.size === 0) return;
  // Publish without replacing an archive, even when two rotations share one timestamp.
  const stem = buildArchiveLogPath(new Date()).replace(/\.jsonl\.gz$/, '');
  for (let sequence = 0; sequence < 100; sequence++) {
    const archive = (sequence === 0 ? stem : stem + '-' + sequence) + '.jsonl';
    try {
      fs.linkSync(currentLogPath, archive);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'EEXIST') continue;
      throw error;
    }
    fs.unlinkSync(currentLogPath);
    pruneLocked();
    return;
  }
}

export function pruneExpiredLogArchives(): void {
  if (!fs.existsSync(getLogArchiveDir())) return;
  try {
    const lease = logWriter();
    try {
      pruneLocked();
    } finally {
      lease.close();
    }
  } catch {
    // Pruning is best effort when the log cannot be opened.
  }
}

function pruneLocked(): void {
  // A crash mid-rotation leaves the current file linked from the archive; finish that
  // rotation before measuring the archive, so this inode is charged once.
  finishInterruptedRotation();
  const config = getResolvedLoggingConfig();
  const archiveDir = getLogArchiveDir();
  if (!fs.existsSync(archiveDir)) {
    return;
  }

  const cutoffMs = Date.now() - config.retain_days * ONE_DAY_MS;
  const archives: {
    file: string;
    size: number;
    mtime: number;
    dev: number;
    ino: number;
    nlink: number;
  }[] = [];
  for (const entry of fs.readdirSync(archiveDir)) {
    const archivePath = path.join(archiveDir, entry);
    try {
      const stats = fs.lstatSync(archivePath);
      if (!stats.isFile() || stats.isSymbolicLink()) {
        continue;
      }
      if (stats.mtimeMs < cutoffMs) {
        fs.unlinkSync(archivePath);
      } else {
        archives.push({
          file: archivePath,
          size: stats.size,
          mtime: stats.mtimeMs,
          dev: stats.dev,
          ino: stats.ino,
          nlink: stats.nlink,
        });
      }
    } catch {
      continue;
    }
  }
  // A second name for an inode already counted is dropped, not charged again.
  const charged: typeof archives = [];
  for (const archive of archives) {
    if (
      archive.nlink > 1 &&
      charged.some((seen) => seen.dev === archive.dev && seen.ino === archive.ino)
    ) {
      try {
        fs.unlinkSync(archive.file);
      } catch {
        /* Retry next prune. */
      }
    } else charged.push(archive);
  }
  let bytes = charged.reduce((sum, item) => sum + item.size, 0);
  for (const archive of charged.sort((a, b) => a.mtime - b.mtime || a.file.localeCompare(b.file))) {
    if (bytes <= LOG_ARCHIVE_BYTE_CAP) break;
    try {
      fs.unlinkSync(archive.file);
      bytes -= archive.size;
    } catch {
      /* Retry next prune. */
    }
  }
}

export function appendStructuredLogEntry(entry: LogEntry): void {
  const config = getResolvedLoggingConfig();
  if (!config.enabled || !shouldWriteLogLevel(entry.level, config.level)) {
    return;
  }

  try {
    ensureLoggingDirectories();
    const lease = logWriter();
    try {
      rotateCurrentLogIfNeeded();
      fs.appendFileSync(getCurrentLogPath(), `${JSON.stringify(entry)}\n`, {
        encoding: 'utf8',
        mode: 0o600,
      });
      pushRecentLogEntry(entry, config.live_buffer_size);
      if (Date.now() - lastPruneAt >= PRUNE_INTERVAL_MS) {
        pruneLocked();
        lastPruneAt = Date.now();
      }
    } finally {
      lease.close();
    }
  } catch {
    // Logging must never break runtime behavior.
  }
}
