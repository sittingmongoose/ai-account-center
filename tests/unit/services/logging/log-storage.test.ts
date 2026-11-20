import { afterEach, beforeEach, describe, expect, it, spyOn } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as zlib from 'zlib';
import { createEmptyUnifiedConfig } from '../../../../src/config/unified-config-types';
import { saveUnifiedConfig } from '../../../../src/config/unified-config-loader';
import { clearRecentLogEntries } from '../../../../src/services/logging/log-buffer';
import { invalidateLoggingConfigCache } from '../../../../src/services/logging/log-config';
import { getCurrentLogPath, getLogArchiveDir } from '../../../../src/services/logging/log-paths';
import {
  appendStructuredLogEntry,
  pruneExpiredLogArchives,
} from '../../../../src/services/logging/log-storage';
import type { LogEntry } from '../../../../src/services/logging/log-types';

function createEntry(overrides: Partial<LogEntry>): LogEntry {
  return {
    id: overrides.id ?? 'entry-1',
    timestamp: overrides.timestamp ?? new Date().toISOString(),
    level: overrides.level ?? 'info',
    source: overrides.source ?? 'unit:test',
    event: overrides.event ?? 'test.event',
    message: overrides.message ?? 'message',
    processId: overrides.processId ?? 1234,
    runId: overrides.runId ?? 'run-1',
    context: overrides.context ?? {},
  };
}

describe('log storage', () => {
  let tempHome = '';
  let originalCcsHome: string | undefined;

  beforeEach(() => {
    originalCcsHome = process.env.CCS_HOME;
    tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-log-storage-'));
    process.env.CCS_HOME = tempHome;
    clearRecentLogEntries();
    invalidateLoggingConfigCache();
  });

  afterEach(() => {
    if (originalCcsHome !== undefined) {
      process.env.CCS_HOME = originalCcsHome;
    } else {
      delete process.env.CCS_HOME;
    }

    clearRecentLogEntries();
    invalidateLoggingConfigCache();
    fs.rmSync(tempHome, { recursive: true, force: true });
    tempHome = '';
  });

  it('rotates the current log into the archive when the file exceeds the age threshold', () => {
    const config = createEmptyUnifiedConfig();
    config.logging.retain_days = 7;
    config.logging.rotate_mb = 10;
    saveUnifiedConfig(config);
    invalidateLoggingConfigCache();

    const currentLogPath = getCurrentLogPath();
    fs.mkdirSync(path.dirname(currentLogPath), { recursive: true });
    fs.writeFileSync(
      currentLogPath,
      `${JSON.stringify(createEntry({ id: 'old-entry' }))}\n`,
      'utf8'
    );
    const staleTimestamp = new Date(Date.now() - 2 * 24 * 60 * 60 * 1000);
    fs.utimesSync(currentLogPath, staleTimestamp, staleTimestamp);

    appendStructuredLogEntry(
      createEntry({
        id: 'new-entry',
        message: 'new log entry after rotation',
      })
    );

    const archiveDir = getLogArchiveDir();
    const archives = fs.readdirSync(archiveDir);
    expect(archives).toHaveLength(1);

    expect(archives[0]).toEndWith('.jsonl');
    const archivedContent = fs.readFileSync(path.join(archiveDir, archives[0]), 'utf8');
    expect(archivedContent).toContain('"id":"old-entry"');

    const currentContent = fs.readFileSync(currentLogPath, 'utf8');
    expect(currentContent).toContain('"id":"new-entry"');
    expect(currentContent).not.toContain('"id":"old-entry"');
  });

  it('keeps both audit archives when an over-limit log rotates at the same millisecond', () => {
    const config = createEmptyUnifiedConfig();
    config.logging.rotate_mb = 1;
    saveUnifiedConfig(config);
    invalidateLoggingConfigCache();

    const current = getCurrentLogPath();
    const archives = getLogArchiveDir();
    fs.mkdirSync(archives, { recursive: true, mode: 0o700 });
    const timestamp = '2026-11-19T14:00:00.001Z';
    const old = path.join(archives, 'ccs-2026-11-19T14-00-00-001Z.jsonl');
    fs.writeFileSync(old, 'first archive\n');
    fs.writeFileSync(current, 'at rotation threshold\n');
    fs.truncateSync(current, 1024 * 1024);
    const clock = spyOn(Date.prototype, 'toISOString').mockReturnValue(timestamp);
    try {
      appendStructuredLogEntry(createEntry({ id: 'next entry' }));
    } finally {
      clock.mockRestore();
    }
    expect(fs.readFileSync(old, 'utf8')).toBe('first archive\n');
    const rotated = fs.readdirSync(archives).filter((name) => name !== path.basename(old));
    expect(rotated).toHaveLength(1);
    expect(fs.readFileSync(path.join(archives, rotated[0]), 'utf8')).toStartWith(
      'at rotation threshold\n'
    );
    expect(fs.readFileSync(current, 'utf8')).toContain('next entry');
  });

  it('caps aggregate archive bytes including uncompressed rotation files', () => {
    const archiveDir = getLogArchiveDir();
    fs.mkdirSync(archiveDir, { recursive: true });
    for (let n = 0; n < 4; n++) {
      const file = path.join(archiveDir, `ccs-cap-${n}.jsonl.gz`);
      fs.closeSync(fs.openSync(file, 'w'));
      fs.truncateSync(file, 20 * 1024 * 1024);
    }
    pruneExpiredLogArchives();
    const size = fs
      .readdirSync(archiveDir)
      .reduce((total, name) => total + fs.statSync(path.join(archiveDir, name)).size, 0);
    expect(size).toBeLessThanOrEqual(32 * 1024 * 1024);
  });

  it('prunes expired archives according to retention settings', () => {
    const config = createEmptyUnifiedConfig();
    config.logging.retain_days = 1;
    saveUnifiedConfig(config);
    invalidateLoggingConfigCache();

    const archiveDir = getLogArchiveDir();
    fs.mkdirSync(archiveDir, { recursive: true });

    const oldArchive = path.join(archiveDir, 'ccs-old.jsonl.gz');
    const freshArchive = path.join(archiveDir, 'ccs-fresh.jsonl.gz');
    fs.writeFileSync(oldArchive, zlib.gzipSync('old archive'), { mode: 0o600 });
    fs.writeFileSync(freshArchive, zlib.gzipSync('fresh archive'), { mode: 0o600 });

    const oldTimestamp = new Date(Date.now() - 3 * 24 * 60 * 60 * 1000);
    const freshTimestamp = new Date(Date.now() - 2 * 60 * 60 * 1000);
    fs.utimesSync(oldArchive, oldTimestamp, oldTimestamp);
    fs.utimesSync(freshArchive, freshTimestamp, freshTimestamp);

    pruneExpiredLogArchives();

    expect(fs.existsSync(oldArchive)).toBe(false);
    expect(fs.existsSync(freshArchive)).toBe(true);
  });

  function interruptedRotation(): {
    current: string;
    archiveDir: string;
    linked: string;
    distinct: string[];
  } {
    const config = createEmptyUnifiedConfig();
    config.logging.rotate_mb = 10;
    saveUnifiedConfig(config);
    invalidateLoggingConfigCache();
    const current = getCurrentLogPath();
    const archiveDir = getLogArchiveDir();
    fs.mkdirSync(archiveDir, { recursive: true, mode: 0o700 });
    const distinct = ['ccs-big-a.jsonl', 'ccs-big-b.jsonl'].map((name, index) => {
      const file = path.join(archiveDir, name);
      fs.writeFileSync(file, Buffer.alloc(10 * 1024 * 1024, 'z'));
      const at = new Date(Date.now() - (2 - index) * 60 * 60 * 1000);
      fs.utimesSync(file, at, at);
      return file;
    });
    fs.writeFileSync(current, Buffer.alloc(10 * 1024 * 1024, 'x'));
    // A writer stopped between the hard link and the unlink of the current file.
    const linked = path.join(archiveDir, 'ccs-interrupted.jsonl');
    fs.linkSync(current, linked);
    return { current, archiveDir, linked, distinct };
  }

  it('finishes an interrupted rotation instead of evicting a distinct archive', () => {
    const { current, archiveDir, linked, distinct } = interruptedRotation();

    appendStructuredLogEntry(createEntry({ id: 'after-interrupted-rotation' }));

    expect(distinct.every((file) => fs.existsSync(file))).toBe(true);
    expect(fs.readdirSync(archiveDir)).toHaveLength(3);
    expect(fs.readFileSync(current, 'utf8')).toContain('after-interrupted-rotation');
    expect(fs.statSync(linked).nlink).toBe(1);
  });

  it('prune finishes an interrupted rotation without waiting for a new entry', () => {
    const { current, archiveDir, linked, distinct } = interruptedRotation();

    pruneExpiredLogArchives();

    expect(fs.existsSync(current)).toBe(false);
    expect(fs.readdirSync(archiveDir)).toHaveLength(3);
    expect(distinct.every((file) => fs.existsSync(file))).toBe(true);
    expect(fs.statSync(linked).nlink).toBe(1);
  });
});
