/**
 * The one-time import of the JSON quota history into the store (docs/store.md section 6):
 * `account-analytics/quota-history-v1.json` and whatever `account-analytics/archive/` holds.
 * Archive files are read tolerantly: one unreadable file or record is skipped and counted, never
 * the end of the import. The source files are left untouched (the rollback path reads them).
 */
import { createHash } from 'crypto';
import * as fs from 'fs';
import * as path from 'path';
import * as zlib from 'zlib';
import {
  decodeAccountAnalyticsObservation,
  FileAccountAnalyticsHistoryStore,
  type AccountAnalyticsObservation,
} from '../web-server/services/account-analytics-history';
import type { AacStore } from './store-service';
import { storeObservationFromRecord, type StoreObservation } from './usage-observations';

const MAX_FILE_BYTES = 64 * 1024 * 1024;
const MAX_ARCHIVE_FILES = 2000;
const MAX_ARCHIVE_DEPTH = 3;
const BATCH = 2000;
const PROVIDERS = [
  'codex',
  'claude',
  'antigravity',
  'muse',
  'cursor',
  'kimi-code',
  'qwen',
  'zai',
  'opencode-go',
];

export interface BackfillReport {
  files: number;
  filesAlreadyImported: number;
  filesUnreadable: number;
  records: number;
  recordsSkipped: number;
  sampled: number;
  seen: number;
  duplicates: number;
  rejected: number;
}

interface SourceFile {
  path: string;
  label: string;
  sha256: string;
  records: AccountAnalyticsObservation[];
  skipped: number;
}

function sha256(bytes: Buffer): string {
  return createHash('sha256').update(bytes).digest('hex');
}

/** Records from any shape the archive may hold: a history object, an array, or JSON lines. */
export function recordsFromText(text: string): { records: unknown[]; parsed: boolean } {
  const trimmed = text.trim();
  if (!trimmed) return { records: [], parsed: true };
  try {
    const value = JSON.parse(trimmed) as unknown;
    if (Array.isArray(value)) return { records: value, parsed: true };
    if (value && typeof value === 'object') {
      const records = (value as { records?: unknown }).records;
      if (Array.isArray(records)) return { records, parsed: true };
      return { records: [value], parsed: true };
    }
    return { records: [], parsed: false };
  } catch {
    const lines = trimmed.split(/\r?\n/);
    const records: unknown[] = [];
    let any = false;
    for (const line of lines) {
      if (!line.trim()) continue;
      try {
        records.push(JSON.parse(line));
        any = true;
      } catch {
        records.push(null);
      }
    }
    return { records, parsed: any };
  }
}

function readArchiveFile(file: string): Buffer | null {
  try {
    const stat = fs.lstatSync(file);
    if (!stat.isFile() || stat.size > MAX_FILE_BYTES) return null;
    const raw = fs.readFileSync(file);
    if (file.endsWith('.gz')) return zlib.gunzipSync(raw, { maxOutputLength: MAX_FILE_BYTES });
    return raw;
  } catch {
    return null;
  }
}

function archiveFiles(directory: string, depth = 0, out: string[] = []): string[] {
  if (depth > MAX_ARCHIVE_DEPTH || out.length >= MAX_ARCHIVE_FILES) return out;
  let entries: fs.Dirent[];
  try {
    entries = fs.readdirSync(directory, { withFileTypes: true });
  } catch {
    return out;
  }
  for (const entry of entries.sort((a, b) => a.name.localeCompare(b.name))) {
    if (out.length >= MAX_ARCHIVE_FILES) break;
    const full = path.join(directory, entry.name);
    if (entry.isDirectory()) archiveFiles(full, depth + 1, out);
    else if (entry.isFile()) out.push(full);
  }
  return out;
}

function providerFor(record: AccountAnalyticsObservation, known: Map<string, string>): string {
  const live = known.get(record.identity);
  if (live) return live;
  const prefix = record.accountId.split(':')[0];
  return PROVIDERS.includes(prefix) ? prefix : 'unknown';
}

/**
 * Imports every source file not imported yet. `providers` maps account identity to provider
 * (from the live dashboard rows): the JSON history keeps the provider only inside its hash.
 */
export async function backfillUsageHistory(
  store: AacStore,
  ccsDir: string,
  providers: Map<string, string> = new Map()
): Promise<BackfillReport> {
  const report: BackfillReport = {
    files: 0,
    filesAlreadyImported: 0,
    filesUnreadable: 0,
    records: 0,
    recordsSkipped: 0,
    sampled: 0,
    seen: 0,
    duplicates: 0,
    rejected: 0,
  };
  const sources: SourceFile[] = [];
  const base = path.join(ccsDir, 'account-analytics');
  await new FileAccountAnalyticsHistoryStore(ccsDir).cleanupTemporaries();
  for (const file of archiveFiles(path.join(base, 'archive'))) {
    const bytes = readArchiveFile(file);
    if (!bytes) {
      report.filesUnreadable += 1;
      continue;
    }
    const { records, parsed } = recordsFromText(bytes.toString('utf8'));
    if (!parsed) {
      report.filesUnreadable += 1;
      continue;
    }
    const valid: AccountAnalyticsObservation[] = [];
    let skipped = 0;
    for (const record of records) {
      const decoded = decodeAccountAnalyticsObservation(record);
      if (decoded) valid.push(decoded);
      else skipped += 1;
    }
    sources.push({
      path: file,
      label: `archive/${path.relative(path.join(base, 'archive'), file).split(path.sep).join('/')}`,
      sha256: sha256(bytes),
      records: valid,
      skipped,
    });
  }
  // The main file through its own strict reader; an unreadable one is counted, not fatal.
  const main = path.join(base, 'quota-history-v1.json');
  if (fs.existsSync(main)) {
    try {
      const data = await new FileAccountAnalyticsHistoryStore(ccsDir).read();
      if (data) {
        sources.push({
          path: main,
          label: 'quota-history-v1.json',
          sha256: sha256(fs.readFileSync(main)),
          records: data.records,
          skipped: 0,
        });
      }
    } catch {
      report.filesUnreadable += 1;
    }
  }

  const pending: StoreObservation[] = [];
  const imported: SourceFile[] = [];
  for (const source of sources) {
    report.files += 1;
    if ((await store.getMeta(`ts:backfill:${source.sha256}`)) !== null) {
      report.filesAlreadyImported += 1;
      continue;
    }
    report.recordsSkipped += source.skipped;
    source.records.forEach((record, index) => {
      const observation = storeObservationFromRecord(
        record,
        providerFor(record, providers),
        `${source.label}#${index}`
      );
      if (observation) pending.push(observation);
      else report.recordsSkipped += 1;
    });
    imported.push(source);
  }
  // Oldest first across every file, so archive and main file interleave correctly; an overlap
  // between them is skipped by the store as a duplicate sample.
  pending.sort((a, b) => a.sampled_at_ms - b.sampled_at_ms || a.observed_at_ms - b.observed_at_ms);
  report.records = pending.length;
  for (let i = 0; i < pending.length; i += BATCH) {
    const result = await store.record(pending.slice(i, i + BATCH));
    report.sampled += result.sampled;
    report.seen += result.seen;
    report.duplicates += result.skipped;
    report.rejected += result.rejected;
  }
  for (const source of imported)
    await store.setMeta(
      `backfill:${source.sha256}`,
      JSON.stringify({ file: source.label, records: source.records.length, at: Date.now() })
    );
  if (imported.length > 0) await store.setMeta('backfill_report', JSON.stringify(report));
  return report;
}
