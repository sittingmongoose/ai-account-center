/**
 * The readers check file (agent-reports/reader@1): what AAC did with each writer's reports,
 * written after every scan to `<reports>/v1/readers/aac/<writer>.json` so `agent-report check`
 * can tell a writer "accepted, seq 12, shown under pin '…'" or "refused: <the fixed reason>".
 *
 * One row per file AAC last read, the reason vocabulary identical to the readers' own refusal
 * words and never the file's text. Written atomically (temporary file, then rename); the
 * folder is AAC's own (`v1/readers`), and a write that hangs or fails never stalls a scan (a
 * flush is fired and forgotten behind a single in-flight guard, failures logged once) — the
 * check is a courtesy to writers, never a load-bearing part of a scan. Rows live in memory
 * between scans: the status scan holds thread.json, tasks.json, status.json and cards, the
 * Scoreboard's runs scan holds runs, and one flush merges both.
 */
import * as fs from 'fs';
import * as path from 'path';
import type { AgentReportsState } from './agent-reports-reader';
import type { MergedPin } from '../../status-native/pinned';
import type { RunsState } from '../scoreboard/runs-reader';
import type { StatusDocsState } from './status-docs';

export const READERS_CHECK_SCHEMA = 'agent-reports/reader@1';

/** The check file's row for one file: every field `agent-report check` prints. */
export interface ReaderFileRow {
  /** Path under `writers/<writer>/`: thread.json, tasks.json, status.json, cards/<id>.json, runs/<date>/<id>.json. */
  file: string;
  seq: number | null;
  /** Hex digest of the bytes read; null when this version was refused before a copy was kept. */
  sha256: string | null;
  /** When AAC read this version (ISO Z). */
  readAt: string;
  result: 'accepted' | 'refused';
  /** The fixed refusal sentence when refused, never file text; null when accepted. */
  reason: string | null;
  /** Only on thread.json and status.json rows: the pinned thread this report is shown under,
   * or null when no pinned T3 thread has this id. Omitted for kinds pins do not apply to. */
  pin?: { t3Thread: string; title: string } | null;
}

/** The share writes one flush performs, so tests can hang or fail them (F2). */
export interface CheckFiles {
  mkdir(dir: string, options?: { recursive?: boolean }): Promise<void>;
  writeFile(file: string, body: string, options?: { mode?: number }): Promise<void>;
  rename(from: string, to: string): Promise<void>;
  rm(file: string, options?: { force?: boolean }): Promise<void>;
}

const nodeCheckFiles: CheckFiles = {
  mkdir: async (dir, options) => {
    await fs.promises.mkdir(dir, options);
  },
  writeFile: async (file, body, options) => {
    await fs.promises.writeFile(file, body, options);
  },
  rename: async (from, to) => {
    await fs.promises.rename(from, to);
  },
  rm: async (file, options) => {
    await fs.promises.rm(file, options);
  },
};

/**
 * Writes the per-writer check files. The rows are held per reports root, not per instance, so
 * the Status scan and the Scoreboard's runs scan merge into one file per writer wherever they
 * read the same share — and a test pointed at a temporary root never writes the real one.
 */
export class ReadersCheckWriter {
  constructor(
    private readonly options: {
      /** The reports root; defaults to the same root the agent-reports reader uses. */
      root?: () => string;
      now?: () => number;
      /** The share writes; defaults to the real filesystem (tests hang or reject them). */
      files?: CheckFiles;
    } = {}
  ) {}

  private now(): number {
    return this.options.now?.() ?? Date.now();
  }

  private root(): string {
    return this.options.root?.() ?? defaultReadersCheckRoot();
  }

  private files(): CheckFiles {
    return this.options.files ?? nodeCheckFiles;
  }

  /** The rows of one root: writer -> file -> row (the newest row per file wins). */
  private store(root: string): Map<string, Map<string, ReaderFileRow>> {
    let rows = storesByRoot.get(root);
    if (!rows) {
      rows = new Map();
      storesByRoot.set(root, rows);
    }
    return rows;
  }

  private rows: () => Map<string, Map<string, ReaderFileRow>> = () => this.store(this.root());

  /** The rows this writer's check file holds right now (tests and the flush below). */
  rowsFor(writer: string): ReaderFileRow[] {
    return [...(this.rows().get(writer)?.values() ?? [])].sort((a, b) =>
      a.file.localeCompare(b.file)
    );
  }

  /** Replaces the status-side rows (thread.json, tasks.json, status.json, cards) from one scan. */
  recordStatusScan(reports: AgentReportsState, statuses: StatusDocsState, pins: MergedPin[]): void {
    const put = (writer: string, row: ReaderFileRow) => {
      const mine = this.rows().get(writer) ?? new Map<string, ReaderFileRow>();
      mine.set(row.file, row);
      this.rows().set(writer, mine);
    };
    const iso = (at: number) => new Date(at).toISOString();
    for (const row of reports.threads) {
      put(row.writer, {
        file: 'thread.json',
        seq: row.doc.seq,
        sha256: row.sha256,
        readAt: iso(row.firstSeenAt),
        result: row.problem ? 'refused' : 'accepted',
        reason: row.problem?.reason ?? null,
        pin: matchPin(row.doc.agent.t3Thread, pins),
      });
    }
    for (const row of reports.tasks) {
      put(row.writer, {
        file: 'tasks.json',
        seq: row.doc.seq,
        sha256: row.sha256,
        readAt: iso(row.firstSeenAt),
        result: row.problem ? 'refused' : 'accepted',
        reason: row.problem?.reason ?? null,
      });
    }
    for (const row of reports.cards) {
      put(row.writer, {
        file: `cards/${row.doc.id}.json`,
        seq: row.doc.seq,
        sha256: row.sha256,
        readAt: iso(row.firstSeenAt),
        result: row.problem ? 'refused' : 'accepted',
        reason: row.problem?.reason ?? null,
      });
    }
    for (const row of statuses.docs) {
      put(row.writer, {
        file: 'status.json',
        seq: row.seq,
        sha256: row.sha256,
        readAt: iso(row.firstSeenAt),
        result: row.problem ? 'refused' : 'accepted',
        reason: row.problem?.reason ?? null,
        pin: matchPin(t3ThreadOf(row.doc), pins),
      });
    }
    // Files the readers refused with no good copy to keep: the row is the refusal itself.
    for (const problem of [...reports.problems, ...statuses.problems]) {
      const [writer, rest] = writerAndFile(problem.file);
      if (!writer || !rest) continue;
      put(writer, {
        file: rest,
        seq: null,
        sha256: null,
        readAt: iso(problem.since),
        result: 'refused',
        reason: problem.reason,
        ...(rest === 'thread.json' || rest === 'status.json' ? { pin: null } : {}),
      });
    }
  }

  /** Replaces the runs rows from one Scoreboard runs scan. */
  recordRuns(runs: RunsState): void {
    const put = (writer: string, row: ReaderFileRow) => {
      const mine = this.rows().get(writer) ?? new Map<string, ReaderFileRow>();
      mine.set(row.file, row);
      this.rows().set(writer, mine);
    };
    const iso = (at: number) => new Date(at).toISOString();
    for (const run of runs.runs) {
      const [writer, rest] = writerAndFile(run.file);
      if (!writer || !rest) continue;
      put(writer, {
        file: rest,
        seq: run.doc.seq,
        sha256: run.sha256,
        readAt: iso(run.firstSeenAt),
        result: 'accepted',
        reason: null,
      });
    }
    for (const problem of runs.problems) {
      const [writer, rest] = writerAndFile(problem.file);
      if (!writer || !rest || !rest.startsWith('runs/')) continue;
      put(writer, {
        file: rest,
        seq: null,
        sha256: null,
        readAt: iso(problem.since),
        result: 'refused',
        reason: problem.reason,
      });
    }
  }

  /** Writes every writer's check file, atomically. Never throws. */
  async flush(): Promise<void> {
    const dir = path.join(this.root(), 'v1', 'readers', 'aac');
    const writtenAt = new Date(this.now()).toISOString();
    const target = this.files();
    try {
      await target.mkdir(dir, { recursive: true });
    } catch {
      return; // The share is not there for us: skip, the next scan tries again.
    }
    let logged = false;
    for (const [writer, files] of this.rows()) {
      const body = `${JSON.stringify(
        {
          schema: READERS_CHECK_SCHEMA,
          reader: 'aac',
          writer,
          writtenAt,
          files: [...files.values()].sort((a, b) => a.file.localeCompare(b.file)),
        },
        null,
        2
      )}\n`;
      const file = path.join(dir, `${writer}.json`);
      const temp = path.join(dir, `.${writer}.tmp-${process.pid}-${Date.now()}`);
      try {
        await target.writeFile(temp, body, { mode: 0o644 });
        await target.rename(temp, file);
      } catch (error) {
        // The check is a courtesy, never load-bearing: one log per flush, never thrown.
        if (!logged) {
          logged = true;
          console.error('readers check flush failed:', error);
        }
        await target.rm(temp, { force: true }).catch(() => undefined);
      }
    }
  }

  /** The one flush in flight, if any; skipped while one runs so scans never pile up behind it. */
  private flushing: Promise<void> | null = null;

  /**
   * Starts one flush and returns at once (F2): a hung share must never stall a scan or a
   * build. A flush already in flight is not queued behind; it is skipped. A failed write is
   * logged once per flush, never thrown.
   */
  flushSoon(): void {
    if (this.flushing) return;
    this.flushing = this.flush()
      .catch((error: unknown) => {
        console.error('readers check flush failed:', error);
      })
      .finally(() => {
        this.flushing = null;
      });
  }
}

/** The default check folder: the same reports root the readers read. */
export function defaultReadersCheckRoot(env: NodeJS.ProcessEnv = process.env): string {
  const configured = env.AAC_READERS_CHECK_DIR?.trim();
  if (configured) return path.resolve(configured);
  const reports = env.AAC_AGENT_REPORTS_DIR?.trim();
  return reports ? path.resolve(reports) : '/mnt/Cursor/agent-reports';
}

/** `writers/<writer>/<rest>` -> its two halves; anything else has no row here. */
function writerAndFile(rel: string): [string, string] | [null, null] {
  const parts = rel.split('/');
  if (parts.length < 3 || parts[0] !== 'writers') return [null, null];
  return [parts[1], parts.slice(2).join('/')];
}

/** The `t3Thread` a held status document carries, when it does. */
function t3ThreadOf(doc: Record<string, unknown>): string | null {
  const value = doc.t3Thread;
  return typeof value === 'string' && value ? value : null;
}

/**
 * The same id normalisation the composer applies (native/aac-status/src/pinned.rs): the plain
 * id, percent-encoded (as T3's run ids spell `mcp:launch:`), a `t3-thread://` link ending in
 * it, or the bare tail — all compare equal. Titles are never matched.
 */
export function normaliseThreadId(id: string): string {
  let text = decodePercent(id.trim()).toLowerCase();
  if (text.includes('/')) text = text.slice(text.lastIndexOf('/') + 1);
  return text.startsWith('mcp:launch:') ? text.slice('mcp:launch:'.length) : text;
}

function decodePercent(text: string): string {
  return text.replace(/%[0-9a-fA-F]{2}/g, (escape) =>
    String.fromCharCode(parseInt(escape.slice(1), 16))
  );
}

/** The pinned thread a report's t3Thread is shown under, or null when no pin has that id. */
export function matchPin(
  id: string | null | undefined,
  pins: readonly MergedPin[]
): { t3Thread: string; title: string } | null {
  if (!id) return null;
  const want = normaliseThreadId(id);
  if (!want) return null;
  const hit = pins.find((pin) => normaliseThreadId(pin.id) === want);
  return hit ? { t3Thread: hit.id, title: hit.title } : null;
}

/** Every root's rows, so both scans merge wherever they read the same share. */
const storesByRoot = new Map<string, Map<string, Map<string, ReaderFileRow>>>();
