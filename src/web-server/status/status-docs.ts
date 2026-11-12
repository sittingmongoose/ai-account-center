import { reportV2 } from './report-v2';
/**
 * The `agent-reports/status@1` documents agents write at `writers/<writer>/status.json`: one
 * optional file per reporting thread, holding what `thread@1` and `tasks.json` lack for a
 * status update (PINNED-THREADS.md section 4). This reader owns the share scan under the
 * same rules as the agent-reports reader (list first, never read an unlisted file, size cap,
 * last good copy, `seq` ordering) and checks every report with that reader's own
 * `jsonFromBytes` (NUL, UTF-8, JSON, secrets) before the aac-status native module
 * (`parseStatus`) reads it: there is one secret filter, the TypeScript one, and the native
 * module only reads reports TypeScript accepted (owner decision 2026-11-09).
 */
import type fs from 'fs';
import type { NativeStatusHandle } from '../../status-native/native-loader';
import {
  MISSING_GRACE_MS,
  PERSISTENT_LISTING_ERROR,
  SEQ_WENT_BACK,
  TransientMisses,
  folderRowProblem,
  isTransientShareError,
  newestFolders,
  nodeReaderFs,
  type ReaderFs,
} from './agent-reports-reader';
import { Invalid, obj, isWriterName, jsonFromBytes } from './schema-helpers';
import { STATUS_SIZE_CAP } from './status-size';

/** The agent-reports reader's plain listing failure (the sibling of PERSISTENT_LISTING_ERROR). */
const LISTING_ERROR = 'folder could not be listed';

/** How the agent-reports reader words a writer folder that is not a writer id (F9). */
const NOT_A_WRITER = 'folder name is not a writer name';

/** One writer's status document: the last good copy, and its current problem if any. */
export interface SeenStatus {
  writer: string;
  doc: Record<string, unknown>;
  sha256: string;
  /** When AAC first saw this version (epoch ms), for the readers check file. */
  firstSeenAt: number;
  /** The document's own `seq`, kept out of the record for ordering. */
  seq: number;
  /** When this version was written (`writtenAt` in the document), epoch ms. */
  writtenAtMs: number;
  problem: { reason: string; since: number } | null;
}

export interface StatusDocsState {
  /** missing: no `v1/writers` folder; error: the folder could not be listed. */
  state: 'ok' | 'missing' | 'error';
  scannedAt: number | null;
  docs: SeenStatus[];
  /** Every current problem, sorted by file. */
  problems: Array<{ file: string; reason: string; since: number }>;
}

/** As many writers as the agent-reports reader reads, and how many documents stay retained
 * across scans (not just this scan's selection). */
export const STATUS_DOCS_LIMITS = { writers: 1000, retained: 1000 };

/** A handle to the native module, or null when it is not available (the caller says so). */
export type StatusHandle = () => NativeStatusHandle | null;

interface Entry extends SeenStatus {
  /** `mtimeMs:size` of the file the good copy was read from; null after a refused read. */
  statKey: string | null;
  /** When the file stopped appearing in its folder listing; null while listed. */
  missingSince: number | null;
}

export class StatusDocsReader {
  private readonly entries = new Map<string, Entry>();
  private readonly misses = new Map<string, number>();
  /** Consecutive ENOENT/ESTALE listing failures, per folder (the agent-reports reader's rule). */
  private readonly listingMisses = new TransientMisses();
  private state: StatusDocsState['state'] = 'missing';
  private scannedAt: number | null = null;
  private scanning: Promise<boolean> | null = null;
  /** The named problem while retained documents were dropped past the retained limit. */
  private retainedOver: { count: number; limit: number; since: number } | null = null;

  constructor(
    private readonly options: {
      root?: () => string;
      now?: () => number;
      fs?: ReaderFs;
      handle?: StatusHandle;
      limits?: Partial<typeof STATUS_DOCS_LIMITS>;
    } = {}
  ) {}

  private get root(): string {
    return this.options.root?.() ?? '';
  }

  private now(): number {
    return this.options.now?.() ?? Date.now();
  }

  private get fs(): ReaderFs {
    return this.options.fs ?? nodeReaderFs;
  }

  /** One rescan; resolves whether anything changed. Overlapping calls share one scan. */
  scan(): Promise<boolean> {
    if (!this.scanning)
      this.scanning = this.runScan()
        .catch(() => false)
        .finally(() => {
          this.scanning = null;
        });
    return this.scanning;
  }

  private async runScan(): Promise<boolean> {
    const now = this.now();
    const handle = this.options.handle?.() ?? null;
    let found: { writers: string[]; rejected: string[] };
    try {
      found = await this.listWriters();
    } catch (error) {
      return this.rootListingFailed(error, handle, now);
    }
    const { writers, rejected } = found;
    if (!handle) {
      // Without the native module there is nothing to validate against; this scan only
      // stamps its time.
      this.scannedAt = now;
      return false;
    }
    const limits = { ...STATUS_DOCS_LIMITS, ...this.options.limits };
    let changed = false;
    const failedListings = new Set<string>();
    // A folder that is not a writer id is refused, never read (F9): its problem-only
    // entry keeps the fixed wording, as the agent-reports reader does for its folders.
    for (const name of rejected) {
      const rel = `writers/${name}/status.json`;
      if (this.recordProblem(rel, name, this.entries.get(rel) ?? null, NOT_A_WRITER, now))
        changed = true;
    }
    const scanned = await newestFolders(writers, limits.writers, (name) =>
      this.fs.stat(`${this.root}/v1/writers/${name}`)
    );
    for (const writer of scanned) {
      if (await this.scanWriter(writer, handle, now, failedListings)) changed = true;
    }
    // A folder listed well this scan (or not tried) starts over.
    this.listingMisses.keepOnly(failedListings);
    // Reconcile the retained entries against this successful complete listing: a writer
    // folder that is no longer listed was withdrawn, which starts the same removal grace as
    // a missing status.json and ends the same way. A failed listing never reaches here, so
    // a failure stays distinct from a successful absence. Writers listed but past the scan's
    // own limit were not tried, not withdrawn; their entries are left to the bound below.
    const listed = new Set([...writers, ...rejected]);
    for (const [rel, entry] of [...this.entries]) {
      if (listed.has(entry.writer)) continue;
      entry.missingSince ??= now;
      if (now - entry.missingSince > MISSING_GRACE_MS) {
        this.entries.delete(rel);
        changed = true;
      }
    }
    // The retained documents themselves are bounded, not just this scan's selection:
    // documents left by writers that fell out of the scan or the listing cannot pile up
    // without end. Past the bound the oldest-written are dropped, and that is named as a
    // problem, never done silently.
    const over = this.entries.size - limits.retained;
    if (over > 0) {
      const oldest = [...this.entries.values()]
        .sort((a, b) => a.writtenAtMs - b.writtenAtMs || a.writer.localeCompare(b.writer))
        .slice(0, over);
      for (const entry of oldest) this.entries.delete(`writers/${entry.writer}/status.json`);
      this.retainedOver = {
        count: over,
        limit: limits.retained,
        since: this.retainedOver?.since ?? now,
      };
      changed = true;
    } else if (this.retainedOver) {
      this.retainedOver = null;
      changed = true;
    }
    this.scannedAt = now;
    this.state = 'ok';
    return changed;
  }

  /**
   * The writers root itself could not be listed. The reader keeps its state as ever (missing
   * when the folder is not there, error otherwise), and every cached document is marked with
   * the agent-reports reader's two-scan rule: one ENOENT/ESTALE miss is the share's race and
   * says nothing, a second consecutive failure or any other error is named on the document,
   * which stays as the last good copy, unreadable since the failure began.
   */
  private rootListingFailed(
    error: unknown,
    handle: NativeStatusHandle | null,
    now: number
  ): boolean {
    const code = (error as NodeJS.ErrnoException | null)?.code;
    const next: StatusDocsState['state'] =
      code === 'ENOENT' || code === 'ENOTDIR' ? 'missing' : 'error';
    const changed = this.state !== next;
    this.state = next;
    this.scannedAt = now;
    // The writer folders were not tried this scan: their own miss counts start over.
    this.listingMisses.keepOnly(new Set(['writers']));
    let reason: string;
    let since = now;
    if (isTransientShareError(error)) {
      const first = this.listingMisses.miss('writers', now);
      if (first === null) return changed; // The share's race: keep the last good copies quiet.
      reason = PERSISTENT_LISTING_ERROR;
      since = first;
    } else {
      reason = LISTING_ERROR;
    }
    let marked = changed;
    if (handle) {
      for (const entry of [...this.entries.values()]) {
        if (this.markUnreadable(entry, folderRowProblem(reason), since)) marked = true;
      }
    }
    return marked;
  }

  /** One writer's `status.json`. Resolves whether anything the view shows changed. */
  private async scanWriter(
    writer: string,
    handle: NativeStatusHandle,
    now: number,
    failedListings: Set<string>
  ): Promise<boolean> {
    const rel = `writers/${writer}/status.json`;
    let listed: fs.Dirent[];
    try {
      listed = await this.fs.readdir(`${this.root}/v1/writers/${writer}`);
    } catch (error) {
      // A writer folder that cannot be listed: not listed is not withdrawn, and the
      // agent-reports reader's rule names the failure — one ENOENT/ESTALE miss is the
      // share's race, a second consecutive failure or any other error is said on the
      // cached document, which stays as the last good copy.
      let reason: string;
      let since = now;
      if (isTransientShareError(error)) {
        failedListings.add(`writers/${writer}`);
        const first = this.listingMisses.miss(`writers/${writer}`, now);
        if (first === null) return false; // The share's race: its last copy stays as it was.
        reason = PERSISTENT_LISTING_ERROR;
        since = first;
      } else {
        reason = LISTING_ERROR;
      }
      const held = this.entries.get(rel) ?? null;
      if (!held) {
        // Nothing good to keep: an unlistable folder with no last copy is a problem alone.
        this.entries.set(rel, {
          writer,
          doc: {},
          sha256: '',
          seq: 0,
          writtenAtMs: 0,
          firstSeenAt: now,
          problem: { reason: folderRowProblem(reason), since },
          statKey: null,
          missingSince: null,
        });
        return true;
      }
      return this.markUnreadable(held, folderRowProblem(reason), since);
    }
    const held = this.entries.get(rel) ?? null;
    if (!listed.some((entry) => entry.name === 'status.json')) {
      // Not listed is not withdrawn until the grace passes (the share's rename-over race).
      if (!held) return false;
      held.missingSince ??= now;
      if (now - held.missingSince > MISSING_GRACE_MS) {
        this.entries.delete(rel);
        return true;
      }
      return false;
    }
    if (held) held.missingSince = null;
    const file = `${this.root}/v1/writers/${writer}/status.json`;

    let problem: string | null = null;
    let problemSince = now;
    let raced = false;
    let statKey: string | null = null;
    let bytes: Buffer | null = null;
    try {
      const stat = await this.fs.stat(file);
      if (!stat.isFile()) problem = 'is not a regular file';
      else {
        statKey = `${stat.mtimeMs}:${stat.size}`;
        if (held && held.statKey === statKey && !held.problem) {
          this.misses.delete(rel);
          return false; // Unchanged since the last good read.
        }
        if (stat.size > STATUS_SIZE_CAP) problem = 'is over the size cap';
        else bytes = await this.fs.readFile(file, STATUS_SIZE_CAP);
      }
    } catch (error) {
      // ENOENT or ESTALE once is the share's race and says nothing; the same error on the
      // scan before too is a file this reader cannot read (the agent-reports reader's rule).
      if (isTransientShareError(error)) {
        const first = this.misses.get(rel);
        if (first === undefined) {
          this.misses.set(rel, now);
          raced = true;
        } else {
          problem = 'could not be read on two scans in a row';
          problemSince = first;
        }
      } else {
        problem = 'could not be read';
      }
    }
    if (raced) return false;
    if (problem) {
      this.misses.delete(rel);
      return this.recordProblem(rel, writer, held, problem, problemSince);
    }
    this.misses.delete(rel);

    const parsed = await parseStatus(handle, bytes as Buffer, writer);
    if (!parsed.ok)
      return this.recordProblem(rel, writer, held, `status.json ${parsed.reason}`, now);

    // A copy older than one already read is not the document, whatever problem the held copy
    // currently carries (the agent-reports reader's rule, applied to its good copy): the
    // scan keeps the newer copy and names why the lower one was not used, until a copy with
    // a same-or-higher seq arrives and clears it.
    if (held && held.sha256 !== '' && parsed.seq < held.seq)
      return this.markUnreadable(held, `status.json ${SEQ_WENT_BACK}`, now);
    if (held && held.sha256 === parsed.sha256 && !held.problem) return false;
    this.entries.set(rel, {
      writer,
      doc: parsed.doc,
      sha256: parsed.sha256,
      seq: parsed.seq,
      writtenAtMs: parsed.writtenAtMs ?? now,
      firstSeenAt: now,
      problem: null,
      statKey,
      missingSince: null,
    });
    return true;
  }

  /** Mark a held document unreadable: the same reason keeps the failure's first clock. */
  private markUnreadable(entry: Entry, reason: string, since: number): boolean {
    const held = entry.problem;
    const problem = held?.reason === reason ? { reason, since: held.since } : { reason, since };
    const changed = held?.reason !== problem.reason || held?.since !== problem.since;
    entry.problem = problem;
    entry.statKey = null; // Retry the read next scan.
    return changed;
  }

  private recordProblem(
    rel: string,
    writer: string,
    held: Entry | null,
    reason: string,
    since: number
  ): boolean {
    const problem = { reason, since };
    const changed = held?.problem?.reason !== reason;
    if (held) {
      held.problem = problem;
      held.statKey = null; // Retry the read next scan.
    } else {
      // Nothing good to keep: a refused file with no last copy is a problem alone.
      this.entries.set(rel, {
        writer,
        doc: {},
        sha256: '',
        seq: 0,
        writtenAtMs: 0,
        firstSeenAt: since,
        problem,
        statKey: null,
        missingSince: null,
      });
    }
    return changed;
  }

  /** Lists the writers root (every valid writer name); a listing failure is thrown to
   * `runScan`, which owns its rule. A folder that is not a writer id (an uppercase name,
   * a missing machine prefix, a `--`) is refused with the agent-reports wording (F9). */
  private async listWriters(): Promise<{ writers: string[]; rejected: string[] }> {
    const names: fs.Dirent[] = await this.fs.readdir(`${this.root}/v1/writers`);
    const writers: string[] = [];
    const rejected: string[] = [];
    for (const entry of names) {
      if (!entry.isDirectory() || entry.name.startsWith('.')) continue;
      if (isWriterName(entry.name)) writers.push(entry.name);
      else rejected.push(entry.name);
    }
    return { writers, rejected };
  }

  snapshot(): StatusDocsState {
    const entries = [...this.entries.values()].sort((a, b) => a.writer.localeCompare(b.writer));
    const docs = entries
      .filter((entry) => entry.sha256 !== '')
      .map((entry) => ({
        writer: entry.writer,
        doc: entry.doc,
        sha256: entry.sha256,
        seq: entry.seq,
        writtenAtMs: entry.writtenAtMs,
        firstSeenAt: entry.firstSeenAt,
        problem: entry.problem,
      }));
    const problems = entries
      .filter((entry) => entry.problem)
      .map((entry) => ({
        file: `writers/${entry.writer}/status.json`,
        reason: entry.problem?.reason ?? '',
        since: entry.problem?.since ?? 0,
      }));
    if (this.retainedOver) {
      const { count, limit, since } = this.retainedOver;
      problems.push({
        file: 'limits/status-docs',
        reason: `${count} documents past the retained limit of ${limit} were not kept (the most recently written are kept)`,
        since,
      });
    }
    return { state: this.state, scannedAt: this.scannedAt, docs, problems };
  }
}

/**
 * The document's parse: the agent-reports reader's own checks (`jsonFromBytes`: size cap,
 * NUL, UTF-8, JSON, secrets) first, then the native module's projection, with the bytes
 * intact (base64). A report TypeScript refuses never reaches the native module; its refusal
 * reason is the TypeScript one — the single secret filter (owner decision 2026-11-09).
 */
async function parseStatus(
  handle: NativeStatusHandle,
  bytes: Buffer,
  writer: string
): Promise<
  | {
      ok: true;
      doc: Record<string, unknown>;
      sha256: string;
      seq: number;
      writtenAtMs: number | null;
    }
  | { ok: false; reason: string }
> {
  const checked = jsonFromBytes(bytes, STATUS_SIZE_CAP);
  if (!checked.ok) return { ok: false, reason: checked.reason };
  try {
    reportV2(obj(checked.value, 'document'));
  } catch (error) {
    return {
      ok: false,
      reason: error instanceof Invalid ? error.message : 'could not be validated',
    };
  }
  let raw: string;
  try {
    raw = await handle.call(
      'parseStatus',
      JSON.stringify({ writer, contentBase64: bytes.toString('base64') })
    );
  } catch {
    return { ok: false, reason: 'could not be validated' };
  }
  let answer: unknown;
  try {
    answer = JSON.parse(raw) as unknown;
  } catch {
    return { ok: false, reason: 'could not be validated' };
  }
  const record = answer as Record<string, unknown>;
  if (record.ok !== true || !record.doc || typeof record.sha256 !== 'string') {
    return { ok: false, reason: typeof record.reason === 'string' ? record.reason : 'was refused' };
  }
  const doc = record.doc as Record<string, unknown>;
  const writtenAtMs = typeof doc.writtenAt === 'string' ? Date.parse(doc.writtenAt) : NaN;
  return {
    ok: true,
    doc,
    sha256: record.sha256,
    seq: typeof doc.seq === 'number' ? doc.seq : 0,
    writtenAtMs: Number.isFinite(writtenAtMs) ? writtenAtMs : null,
  };
}
