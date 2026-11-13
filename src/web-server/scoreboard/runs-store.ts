/**
 * Agent-report runs kept forever in aac-store (Scoreboard v2, share 2026-11-13-scoreboard-v2):
 * the TypeScript side only moves text between the native calls. Scanning the share, the run@1
 * projection, normalization, dedupe, storage and queries are Rust (native/aac-store/src/runs.rs).
 * Between scan and record every text passes `jsonFromBytes`, the one secret filter (owner
 * decision 2026-11-09). A pass that stops at its read budget continues on the next one: nothing
 * is capped away.
 */
import type { AacStore } from '../../store/store-service';
import type { ReportProblem } from '../status/agent-reports-reader';
import type { RunDoc } from '../status/report-schema';
import { REPORT_SIZE_CAPS } from '../status/report-schema';
import { findSecret, jsonFromBytes } from '../status/schema-helpers';
import { createHash } from 'crypto';
import { CATALOG_CAP_BYTES } from './catalog';
import type { RunsState } from './runs-reader';

/** Every two minutes, and again within a second while a backfill has more to read. */
export const RUNS_PASS_MS = 120_000;
export const RUNS_MORE_MS = 1_000;
/** A full pass (every folder listed, whatever its mtime) at least this often. */
export const RUNS_FULL_MS = 15 * 60_000;

interface ScanFile {
  path: string;
  text: string;
  size: number;
  mtimeMs: number;
}

interface Scan {
  state: 'ok' | 'missing' | 'error';
  files: ScanFile[];
  refused: Array<{ path: string; size: number; mtimeMs: number; reason: string }>;
  threads: Record<string, string>;
  catalog: { models: string | null; aliases: string | null };
  writers?: number;
  listed: number;
  unchanged: number;
  more: boolean;
  problems: Array<{ file: string; reason: string }>;
}

export interface RecordResult {
  recorded: number;
  known: number;
  refused: number;
  rekeyed: number;
  catalogHash: string;
  catalogChanged: boolean;
  generation: number;
}

export interface StoredRun {
  file: string;
  sha256: string;
  firstSeenAt: number;
  doc: RunDoc;
  identity: Record<string, unknown>;
  ids: Record<string, unknown>;
}

/** The page's identity facts: families in order, catalog models' family and order, efforts. */
export interface RunsIdentityTable {
  catalogHash: string;
  families: Array<{ id: string; label: string; short: string; order: number }>;
  models: Record<string, { family: string | null; order: number; label: string }>;
  efforts: string[];
}

export interface RunsQuery {
  fromMs?: number;
  toMs?: number;
  source?: 'observed' | 'benchmark';
  taskType?: string;
  /** Only runs with a review block (the reviews a filtered view joins). */
  reviewsOnly?: boolean;
}

export interface PassState {
  at: number;
  state: Scan['state'];
  more: boolean;
  listed: number;
  read: number;
  recorded: number;
  refused: number;
  generation: number;
  problems: ReportProblem[];
}

const sha256 = (text: string) => createHash('sha256').update(text, 'utf8').digest('hex');

/** A path's hash, as the store keys files and names refusals (`path_key` in runs.rs). */
export function pathHash(path: string): string {
  return createHash('sha256').update(`aac-run-path-v1:${path}`, 'utf8').digest('hex').slice(0, 32);
}

/** The fixed reason for a file whose name looks like a credential (the store's words too). */
export const SECRET_NAME_REASON = 'refused: its file name looks like a credential';

/** One refused file as the store keeps it: never its path, name or writer. */
export interface RefusedRow {
  pathHash: string;
  reason: string | null;
  size: number;
  mtimeMs: number;
  atMs: number;
}

/** What a display shows for a name it may not show. */
export const NAME_WITHHELD = '(name withheld)';

/** A path or folder for display: as it is when the secret filter passes it, else withheld. */
function shown(path: string): string {
  return findSecret(path) === null ? path : NAME_WITHHELD;
}

/** The secret filter over one text: its value when accepted, else the fixed reason. */
function filtered(text: string, cap: number): { ok: true } | { ok: false; reason: string } {
  const parsed = jsonFromBytes(Buffer.from(text, 'utf8'), cap);
  return parsed.ok ? { ok: true } : { ok: false, reason: parsed.reason };
}

export class RunsStore {
  private passing: Promise<PassState> | null = null;
  /**
   * Paths this server has seen, by their hash, for naming a refused file in the Sources and the
   * readers check. Memory only, and only paths the secret filter passes: the store keeps none.
   */
  private readonly safePaths = new Map<string, string>();
  private lastFullAt = 0;
  last: PassState | null = null;

  constructor(
    private readonly deps: {
      store: () => AacStore | null;
      root: () => string;
      now: () => number;
    }
  ) {}

  /** One ingest pass; overlapping calls share it. Rejects only when the store refuses a call. */
  pass(options: { full?: boolean } = {}): Promise<PassState> {
    this.passing ??= this.runPass(options.full ?? false).finally(() => {
      this.passing = null;
    });
    return this.passing;
  }

  private async runPass(forceFull: boolean): Promise<PassState> {
    const store = this.deps.store();
    const now = this.deps.now();
    if (!store)
      return {
        at: now,
        state: 'error',
        more: false,
        listed: 0,
        read: 0,
        recorded: 0,
        refused: 0,
        generation: this.last?.generation ?? 0,
        problems: [],
      };
    const full = forceFull || now - this.lastFullAt >= RUNS_FULL_MS;
    const scan = await store.runs<Scan>('runsScan', { root: this.deps.root(), full });
    if (full) this.lastFullAt = now;
    const files: ScanFile[] = [];
    const refused: Array<Scan['refused'][number] & { sha256?: string }> = [...scan.refused];
    const remember = (path: string) => {
      if (findSecret(path) === null) this.safePaths.set(pathHash(path), path);
    };
    for (const row of scan.refused) remember(row.path);
    for (const [index, file] of scan.files.entries()) {
      // A backfill hands over thousands of texts: let the server answer between batches.
      if (index > 0 && index % 100 === 0) await new Promise((resolve) => setImmediate(resolve));
      remember(file.path);
      // A file's name can hold a credential too: the file is refused and its name kept nowhere.
      if (findSecret(file.path) !== null) {
        refused.push({
          path: file.path,
          size: file.size,
          mtimeMs: file.mtimeMs,
          reason: SECRET_NAME_REASON,
          sha256: sha256(file.text),
        });
        continue;
      }
      const verdict = filtered(file.text, REPORT_SIZE_CAPS.run);
      if (verdict.ok) files.push(file);
      else
        refused.push({
          path: file.path,
          size: file.size,
          mtimeMs: file.mtimeMs,
          reason: verdict.reason,
          sha256: sha256(file.text),
        });
    }
    const threads: Record<string, string> = {};
    for (const [writer, text] of Object.entries(scan.threads))
      if (findSecret(writer) === null && filtered(text, REPORT_SIZE_CAPS.thread).ok)
        threads[writer] = text;
    const catalogText = (text: string | null) =>
      text !== null && filtered(text, CATALOG_CAP_BYTES).ok ? text : null;
    const record = await store.runs<RecordResult>('runsRecord', {
      files,
      refused,
      threads,
      catalog: {
        models: catalogText(scan.catalog.models),
        aliases: catalogText(scan.catalog.aliases),
      },
      nowMs: now,
    });
    const problems: ReportProblem[] = scan.problems.map((problem) => ({
      ...problem,
      file: shown(problem.file),
      since: now,
    }));
    if (scan.more)
      problems.push({
        file: 'limits/import',
        reason: `Importing run history: ${scan.files.length + scan.refused.length} files read this pass; the rest follow in the next passes.`,
        since: now,
      });
    this.last = {
      at: now,
      state: scan.state,
      more: scan.more,
      listed: scan.listed,
      read: scan.files.length,
      recorded: record.recorded,
      refused: record.refused,
      generation: record.generation,
      problems,
    };
    return this.last;
  }

  /** Current revisions in a range, every page. */
  async query(query: RunsQuery): Promise<StoredRun[]> {
    const store = this.deps.store();
    if (!store) return [];
    const runs: StoredRun[] = [];
    let after: unknown = null;
    for (;;) {
      const page = await store.runs<{ runs: StoredRun[]; next: unknown }>('runsQuery', {
        ...query,
        after,
      });
      runs.push(...page.runs);
      if (page.next === null || page.next === undefined) return runs;
      after = page.next;
    }
  }

  async identity(): Promise<RunsIdentityTable | null> {
    const store = this.deps.store();
    return store ? store.runs<RunsIdentityTable>('runsIdentity') : null;
  }

  /** The runs as the view builder reads them (the file reader's shape), from the store. */
  async state(query: RunsQuery): Promise<RunsState> {
    const store = this.deps.store();
    const runs = await this.query(query);
    const refused = store ? await store.runs<RefusedRow[]>('runsRefused') : [];
    // A refused file is named by the path this server saw when the secret filter passes it,
    // else by its hash alone: the store keeps no path, name or writer for it.
    const problems: ReportProblem[] = [
      ...refused.map((row) => ({
        file:
          this.safePaths.get(row.pathHash) ??
          `refused/${NAME_WITHHELD} ${row.pathHash.slice(0, 12)}`,
        reason: row.reason ?? 'refused',
        since: row.atMs,
      })),
      ...(this.last?.problems ?? []),
    ];
    return {
      state: this.last?.state ?? 'ok',
      scannedAt: this.last?.at ?? null,
      runs: runs.map((run) => ({
        file: run.file,
        doc: run.doc,
        firstSeenAt: run.firstSeenAt,
        sha256: run.sha256,
      })),
      writers: new Set(runs.map((run) => run.doc.writer)).size,
      skipped: { runFiles: 0, writerFolders: 0 },
      problems,
    };
  }
}
