/**
 * The agent reports reader: `<reports>/v1/writers/<writer>/{thread.json,tasks.json,cards/*.json}`.
 *
 * The folder lives on a network share (NFS here, SMB from the Mac and Windows), so there is no
 * change notification: the service rescans on a timer. A rescan stats every file and reads only
 * the ones whose size, time or inode changed. Each file keeps its last good copy; a file that later
 * fails (bad JSON, NUL bytes from a damaged write, a secret, a schema miss) keeps showing that copy
 * with a named problem, and a file with no good copy is listed as a problem by name. A lower `seq`
 * than the one already accepted is ignored. AAC records when it first saw each version, so a
 * writer whose clock runs ahead cannot look fresher than it is.
 */
import fs from 'fs';
import path from 'path';
import {
  isWriterName,
  parseReportFile,
  REPORT_SIZE_CAPS,
  type CardDoc,
  type ParsedReport,
  type ReportDoc,
  type ReportKind,
  type TasksDoc,
  type ThreadDoc,
} from './report-schema';
import { readRegularFile } from './share-read';

export const DEFAULT_AGENT_REPORTS_DIR = '/mnt/Cursor/agent-reports';

/** `AAC_AGENT_REPORTS_DIR`, else the shared folder. */
export function agentReportsRoot(): string {
  const configured = process.env.AAC_AGENT_REPORTS_DIR?.trim();
  return configured ? path.resolve(configured) : DEFAULT_AGENT_REPORTS_DIR;
}

/** The folders AAC owns under `<reports>/v1`. `writers/` is writable by every agent. */
export const REPORT_FOLDERS = ['writers', 'answers', 'keys', 'schemas', 'catalog'] as const;

/**
 * Creates `<reports>/v1/` and its folders when missing (`writers/` mode 0777). The reports root
 * itself is never created: an unmounted share must not grow a local folder. Resolves which
 * folders it created; never throws.
 */
export async function ensureReportsLayout(root: string): Promise<string[]> {
  const created: string[] = [];
  try {
    if (!(await fs.promises.stat(root)).isDirectory()) return created;
  } catch {
    return created;
  }
  const make = async (dir: string, mode?: number): Promise<boolean> => {
    try {
      await fs.promises.mkdir(dir);
    } catch {
      // Already there (or not creatable): leave it as it is.
      return false;
    }
    if (mode !== undefined) await fs.promises.chmod(dir, mode).catch(() => undefined);
    created.push(path.relative(root, dir));
    return true;
  };
  const v1 = path.join(root, 'v1');
  await make(v1);
  for (const folder of REPORT_FOLDERS)
    await make(path.join(v1, folder), folder === 'writers' ? 0o777 : undefined);
  return created;
}

/** The JSON Schemas this package ships (docs/agent-reports/schemas), published to `<reports>/v1/schemas`. */
export function packagedSchemasDir(): string {
  return path.resolve(__dirname, '../../../docs/agent-reports/schemas');
}

/**
 * Copies every packaged `<kind>.json` schema to `<reports>/v1/schemas/` when it is missing there or
 * different (temporary dot file, then rename). Only into an existing `v1/schemas` folder. Resolves
 * the names it wrote; never throws.
 */
export async function publishSchemas(
  root: string,
  sourceDir: string = packagedSchemasDir()
): Promise<string[]> {
  const target = path.join(root, 'v1', 'schemas');
  const written: string[] = [];
  try {
    if (!(await fs.promises.stat(target)).isDirectory()) return written;
    const names = (await fs.promises.readdir(sourceDir)).filter((name) =>
      /^[a-z][a-z0-9-]*\.json$/.test(name)
    );
    for (const name of names.sort()) {
      const body = await fs.promises.readFile(path.join(sourceDir, name));
      const current = await readRegularFile(path.join(target, name), body.length).catch(() => null);
      if (current && current.equals(body)) continue;
      const temp = path.join(target, `.${name}.tmp-${process.pid}-${Date.now()}`);
      try {
        await fs.promises.writeFile(temp, body, { mode: 0o644 });
        await fs.promises.rename(temp, path.join(target, name));
        written.push(name);
      } catch {
        await fs.promises.rm(temp, { force: true }).catch(() => undefined);
      }
    }
  } catch {
    /* No schemas folder or no packaged schemas: nothing to publish. */
  }
  return written;
}

export interface ReportProblem {
  /** Relative to `<reports>/v1`, such as `writers/vm.foo/tasks.json`. */
  file: string;
  reason: string;
  /** When this problem was first seen (epoch ms). */
  since: number;
}

export interface SeenReport<T extends ReportDoc> {
  file: string;
  writer: string;
  doc: T;
  sha256: string;
  /** When AAC first saw this version (epoch ms). */
  firstSeenAt: number;
  writtenAtMs: number;
  /** The file's current problem: the copy above is the last good one. */
  problem: ReportProblem | null;
}

export interface AgentReportsState {
  root: string;
  /** missing: no `v1/writers` folder; error: the folder could not be listed. */
  state: 'ok' | 'missing' | 'error';
  /** The last scan that finished (epoch ms), null before the first. */
  scannedAt: number | null;
  /**
   * The latest scan listed `v1/writers` and every writer and `cards/` folder, read every file it
   * tried (a file it refused for its content is named in `problems` and does not count) and hit no
   * limit. Only then is a card missing from `cards` known to be gone.
   */
  complete: boolean;
  threads: SeenReport<ThreadDoc>[];
  tasks: SeenReport<TasksDoc>[];
  cards: SeenReport<CardDoc>[];
  /** Every current problem, with or without a good copy, sorted by file. */
  problems: ReportProblem[];
  /** Board files by the path a writer gave: modification time, or null when not readable here. */
  boards: ReadonlyMap<string, number | null>;
}

interface Entry {
  kind: ReportKind;
  writer: string;
  statKey: string;
  good: SeenReport<ReportDoc> | null;
  problem: ReportProblem | null;
  /** When the file stopped appearing in its folder's listing (epoch ms), null while listed. */
  missingSince?: number | null;
}

const SEQ_WENT_BACK = 'its seq is lower than one already read, so this copy is ignored';

/**
 * A file absent from its folder's listing is kept (last good copy) this long before it counts as
 * removed: a Mac rename-over through SMB is a delete then a rename, and NFS caches the gap.
 */
export const MISSING_GRACE_MS = 120_000;

export interface ReaderFs {
  readdir(dir: string): Promise<fs.Dirent[]>;
  stat(file: string): Promise<fs.Stats>;
  /** Reads a regular file; `capBytes` limits what is held (a larger file comes back over the cap). */
  readFile(file: string, capBytes: number): Promise<Buffer>;
}

/**
 * What one scan reads (docs/status-tab.md, Reading rules), so the share cannot grow the server
 * without bound. Past a limit the rest is named as a problem; the most recently changed writers are
 * the ones kept.
 */
export const REPORT_LIMITS = { writers: 200, cards: 500, bytes: 64 * 1024 * 1024 };

export const nodeReaderFs: ReaderFs = {
  readdir: (dir) => fs.promises.readdir(dir, { withFileTypes: true }),
  stat: (file) => fs.promises.stat(file),
  readFile: (file, capBytes) => readRegularFile(file, capBytes),
};

function isMissing(error: unknown): boolean {
  const code = (error as NodeJS.ErrnoException | null)?.code;
  return code === 'ENOENT' || code === 'ENOTDIR';
}

/**
 * ENOENT or ESTALE (Node reports ESTALE as "Unknown system error -116"): the share is between a
 * delete and a rename, or holds a stale handle. Keep the last good copy and try again next scan.
 */
export function isTransientShareError(error: unknown): boolean {
  const err = error as NodeJS.ErrnoException | null;
  return (
    err?.code === 'ENOENT' ||
    err?.code === 'ESTALE' ||
    err?.errno === -116 ||
    /system error -116/.test(String(err?.message ?? ''))
  );
}

/** A name safe to show: printable ASCII without separators. */
export function showable(name: string): boolean {
  return /^[\x21-\x7e]{1,100}$/.test(name) && !name.includes('/') && !name.includes('\\');
}

/**
 * A path a writer gave (VM, Mac or Windows spelling of the shared folder) -> where it is on this
 * computer, or null. Only paths inside the share that holds the reports folder are looked at.
 */
export function shareLocalPath(given: string, root: string): string | null {
  const shareRoot = path.dirname(path.resolve(root));
  let candidate: string | null = null;
  const share = /^(?:\/mnt\/Cursor\/|\/Volumes\/Cursor\/|\\\\TRUENAS\\Cursor\\)/i.exec(given);
  if (share) candidate = path.join(shareRoot, given.slice(share[0].length).replace(/\\/g, '/'));
  else if (path.isAbsolute(given)) candidate = path.resolve(given);
  if (!candidate || !candidate.startsWith(shareRoot + path.sep)) return null;
  return candidate;
}

function boardPaths(threads: SeenReport<ThreadDoc>[], tasks: SeenReport<TasksDoc>[]): string[] {
  const paths = new Set<string>();
  for (const row of tasks) if (row.doc.board) paths.add(row.doc.board);
  for (const row of threads)
    for (const link of row.doc.links) if (/board/i.test(link.label)) paths.add(link.path);
  return [...paths];
}

const NEWEST_STATS_AT_ONCE = 16;

/**
 * At most `limit` names: all of them when they fit, else those whose folder changed most recently
 * (never read, 0, last), with a bounded number of stats running: a share can hold many thousands.
 */
export async function newestFolders(
  names: string[],
  limit: number,
  statOf: (name: string) => Promise<fs.Stats>
): Promise<string[]> {
  if (names.length <= limit) return names;
  const dated: Array<{ name: string; mtime: number }> = [];
  let next = 0;
  const worker = async () => {
    while (next < names.length) {
      const name = names[next++];
      const mtime = await statOf(name).then(
        (stat) => stat.mtimeMs,
        () => 0
      );
      dated.push({ name, mtime });
    }
  };
  await Promise.all(Array.from({ length: NEWEST_STATS_AT_ONCE }, worker));
  dated.sort((a, b) => b.mtime - a.mtime || a.name.localeCompare(b.name));
  return dated.slice(0, limit).map((row) => row.name);
}

export class AgentReportsReader {
  private readonly entries = new Map<string, Entry>();
  private readonly folderProblems = new Map<string, ReportProblem>();
  private boards = new Map<string, number | null>();
  private state: AgentReportsState['state'] = 'missing';
  private scannedAt: number | null = null;
  private complete = false;
  /** This scan: a folder or file that could not be read, or a limit reached. */
  private scanPartial = false;
  private scanBytes = 0;
  private overBytes = 0;
  private scanning: Promise<boolean> | null = null;

  constructor(
    private readonly options: {
      root?: () => string;
      now?: () => number;
      fs?: ReaderFs;
      limits?: Partial<typeof REPORT_LIMITS>;
    } = {}
  ) {}

  get root(): string {
    return (this.options.root ?? agentReportsRoot)();
  }

  private now(): number {
    return (this.options.now ?? Date.now)();
  }

  private get fs(): ReaderFs {
    return this.options.fs ?? nodeReaderFs;
  }

  /** One rescan; resolves whether anything the view shows changed. Overlapping calls share one scan. */
  scan(): Promise<boolean> {
    if (!this.scanning)
      this.scanning = this.runScan()
        .catch((error: unknown) => {
          this.complete = false;
          throw error;
        })
        .finally(() => {
          this.scanning = null;
        });
    return this.scanning;
  }

  private async runScan(): Promise<boolean> {
    const v1 = path.join(this.root, 'v1');
    const writersDir = path.join(v1, 'writers');
    let changed = false;
    let writers: fs.Dirent[];
    const limits = { ...REPORT_LIMITS, ...this.options.limits };
    this.scanPartial = false;
    this.scanBytes = 0;
    this.overBytes = 0;
    try {
      writers = await this.fs.readdir(writersDir);
    } catch (error) {
      this.complete = false;
      // A stale handle on the share: keep everything and try again next scan (the source turns
      // stale if it persists).
      if (
        (error as NodeJS.ErrnoException | null)?.code !== 'ENOENT' &&
        isTransientShareError(error)
      )
        return false;
      const next = isMissing(error) ? 'missing' : 'error';
      changed = this.state !== next;
      this.state = next;
      this.scannedAt = this.now();
      return changed;
    }
    if (this.state !== 'ok') changed = true;
    this.state = 'ok';
    const seen = new Set<string>();
    const folderProblems = new Map<string, ReportProblem>();
    const folderProblem = (file: string, reason: string) => {
      const before = this.folderProblems.get(file);
      folderProblems.set(file, {
        file,
        reason,
        since: before?.reason === reason ? before.since : this.now(),
      });
    };
    const keepWriter = (name: string, folder = '') => {
      const prefix = `writers/${name}/${folder}`;
      for (const key of this.entries.keys()) if (key.startsWith(prefix)) seen.add(key);
    };
    const names: string[] = [];
    for (const dirent of writers) {
      const name = dirent.name;
      if (name.startsWith('.')) continue;
      if (!dirent.isDirectory()) continue;
      if (!isWriterName(name)) {
        if (showable(name)) folderProblem(`writers/${name}`, 'folder name is not a writer name');
        continue;
      }
      names.push(name);
    }
    const kept = await newestFolders(names, limits.writers, (name) =>
      this.fs.stat(path.join(writersDir, name))
    );
    if (kept.length < names.length) {
      this.scanPartial = true;
      folderProblem(
        'limits/writers',
        `${names.length - kept.length} writer folders past the limit of ${limits.writers} were not read (the most recently changed are kept)`
      );
    }
    let cardsVisited = 0;
    let cardsOver = 0;
    for (const name of kept) {
      const base = path.join(writersDir, name);
      // List the folder first, then read: a listing refreshes the NFS view of a rename-over.
      let listed: Set<string>;
      try {
        listed = new Set((await this.fs.readdir(base)).map((entry) => entry.name));
      } catch (error) {
        this.scanPartial = true;
        if (!isTransientShareError(error))
          folderProblem(`writers/${name}`, 'folder could not be listed');
        keepWriter(name);
        continue;
      }
      for (const [file, kind] of [
        ['thread.json', 'thread'],
        ['tasks.json', 'tasks'],
      ] as const) {
        if (!listed.has(file)) continue;
        const rel = `writers/${name}/${file}`;
        if (await this.visit(path.join(base, file), rel, kind, name, '', seen)) changed = true;
      }
      if (!listed.has('cards')) continue;
      let cards: fs.Dirent[] = [];
      try {
        cards = await this.fs.readdir(path.join(base, 'cards'));
      } catch (error) {
        this.scanPartial = true;
        if (!isTransientShareError(error))
          folderProblem(`writers/${name}/cards`, 'folder could not be listed');
        // A listing failure is not a withdrawal: keep the cards already known.
        keepWriter(name, 'cards/');
      }
      for (const card of [...cards].sort((a, b) => a.name.localeCompare(b.name))) {
        if (card.name.startsWith('.') || !card.name.endsWith('.json')) continue;
        const id = card.name.slice(0, -'.json'.length);
        const rel = `writers/${name}/cards/${card.name}`;
        if (!showable(card.name)) continue;
        if (!card.isFile()) {
          // A FIFO or a link where a card belongs: it may stand for an open card nobody can read.
          this.scanPartial = true;
          folderProblem(rel, 'is not a regular file');
          continue;
        }
        if (cardsVisited >= limits.cards) {
          cardsOver += 1;
          continue;
        }
        cardsVisited += 1;
        if (await this.visit(path.join(base, 'cards', card.name), rel, 'card', name, id, seen))
          changed = true;
      }
    }
    if (cardsOver > 0) {
      this.scanPartial = true;
      folderProblem(
        'limits/cards',
        `${cardsOver} card files past the limit of ${limits.cards} were not read`
      );
    }
    if (this.overBytes > 0) {
      this.scanPartial = true;
      folderProblem(
        'limits/bytes',
        `${this.overBytes} files past the limit of ${limits.bytes / (1024 * 1024)} MB were not read`
      );
    }
    const now = this.now();
    for (const [key, entry] of [...this.entries]) {
      if (seen.has(key)) {
        entry.missingSince = null;
        continue;
      }
      entry.missingSince ??= now;
      if (now - entry.missingSince >= MISSING_GRACE_MS) {
        this.entries.delete(key);
        changed = true;
      }
    }
    if (!sameProblems(this.folderProblems, folderProblems)) changed = true;
    this.folderProblems.clear();
    for (const [key, value] of folderProblems) this.folderProblems.set(key, value);
    if (await this.statBoards()) changed = true;
    this.scannedAt = this.now();
    // A card file refused for its content with no earlier good copy is absent from the list though it may be open:
    // the list then proves nothing about closed cards (the page would drop that card's unsent draft).
    const unseenCard = [...this.entries.values()].some(
      (entry) => entry.kind === 'card' && entry.problem !== null && entry.good === null
    );
    this.complete = !this.scanPartial && !unseenCard;
    return changed;
  }

  /** Stat and, when changed, read one file. Resolves whether it changed what the view shows. */
  private async visit(
    file: string,
    rel: string,
    kind: ReportKind,
    writer: string,
    cardId: string,
    seen: Set<string>
  ): Promise<boolean> {
    let stat: fs.Stats;
    try {
      stat = await this.fs.stat(file);
    } catch (error) {
      seen.add(rel);
      this.scanPartial = true;
      // Listed but gone or stale: keep the last good copy and read again next scan.
      if (isTransientShareError(error)) return false;
      return this.setProblem(rel, kind, writer, 'could not be read');
    }
    if (!stat.isFile()) {
      seen.add(rel);
      return this.setProblem(rel, kind, writer, 'is not a regular file');
    }
    // A file over its own cap is never read or held, so it takes nothing from the other files' budget.
    if (stat.size <= REPORT_SIZE_CAPS[kind]) {
      if (this.scanBytes + stat.size > (this.options.limits?.bytes ?? REPORT_LIMITS.bytes)) {
        this.overBytes += 1;
        return false;
      }
      this.scanBytes += stat.size;
    }
    seen.add(rel);
    const statKey = `${stat.size}:${stat.mtimeMs}:${stat.ino}:${stat.ctimeMs}`;
    const entry = this.entries.get(rel);
    if (entry && entry.statKey === statKey) return false;
    if (stat.size > REPORT_SIZE_CAPS[kind])
      return this.setProblem(
        rel,
        kind,
        writer,
        `larger than the ${REPORT_SIZE_CAPS[kind] / 1024} KB cap`,
        statKey
      );
    let bytes: Buffer;
    try {
      bytes = await this.fs.readFile(file, REPORT_SIZE_CAPS[kind]);
    } catch (error) {
      this.scanPartial = true;
      if (isTransientShareError(error)) return false;
      return this.setProblem(rel, kind, writer, 'could not be read');
    }
    const parsed =
      kind === 'card'
        ? parseReportFile('card', bytes, writer, cardId)
        : kind === 'tasks'
          ? parseReportFile('tasks', bytes, writer)
          : parseReportFile('thread', bytes, writer);
    if (!parsed.ok) return this.setProblem(rel, kind, writer, parsed.reason, statKey);
    return this.accept(rel, kind, writer, parsed.value, statKey);
  }

  private setProblem(
    rel: string,
    kind: ReportKind,
    writer: string,
    reason: string,
    statKey = ''
  ): boolean {
    const entry = this.entries.get(rel) ?? { kind, writer, statKey: '', good: null, problem: null };
    const changed = entry.problem?.reason !== reason;
    entry.problem = changed ? { file: rel, reason, since: this.now() } : entry.problem;
    // A read error keeps the key empty so the next scan tries again.
    entry.statKey = statKey;
    if (entry.good) entry.good = { ...entry.good, problem: entry.problem };
    this.entries.set(rel, entry);
    return changed;
  }

  private accept(
    rel: string,
    kind: ReportKind,
    writer: string,
    parsed: ParsedReport,
    statKey: string
  ): boolean {
    const entry = this.entries.get(rel) ?? { kind, writer, statKey, good: null, problem: null };
    // An older write landing late, or a writer that restarted its count: the older copy is ignored
    // and said out loud. A card stays unanswerable until then: the page shows the higher seq, an
    // answer to it could only ever come back "card changed", and the owner would loop.
    if (entry.good && parsed.doc.seq < entry.good.doc.seq)
      return this.setProblem(rel, kind, writer, SEQ_WENT_BACK, statKey);
    const hadProblem = entry.problem !== null;
    entry.statKey = statKey;
    entry.problem = null;
    this.entries.set(rel, entry);
    if (entry.good && entry.good.sha256 === parsed.sha256) {
      entry.good = { ...entry.good, problem: null };
      return hadProblem;
    }
    entry.good = {
      file: rel,
      writer,
      doc: parsed.doc,
      sha256: parsed.sha256,
      firstSeenAt: this.now(),
      writtenAtMs: Date.parse(parsed.doc.writtenAt),
      problem: null,
    };
    return true;
  }

  private async statBoards(): Promise<boolean> {
    const { threads, tasks } = this.collect();
    const next = new Map<string, number | null>();
    for (const given of boardPaths(threads, tasks)) {
      const local = shareLocalPath(given, this.root);
      let mtime: number | null = null;
      if (local) {
        try {
          const stat = await this.fs.stat(local);
          mtime = stat.isFile() ? stat.mtimeMs : null;
        } catch {
          mtime = null;
        }
      }
      next.set(given, mtime);
    }
    const changed =
      next.size !== this.boards.size ||
      [...next].some(([key, value]) => this.boards.get(key) !== value);
    this.boards = next;
    return changed;
  }

  private collect() {
    const threads: SeenReport<ThreadDoc>[] = [];
    const tasks: SeenReport<TasksDoc>[] = [];
    const cards: SeenReport<CardDoc>[] = [];
    for (const entry of this.entries.values()) {
      if (!entry.good) continue;
      if (entry.kind === 'thread') threads.push(entry.good as SeenReport<ThreadDoc>);
      else if (entry.kind === 'tasks') tasks.push(entry.good as SeenReport<TasksDoc>);
      else cards.push(entry.good as SeenReport<CardDoc>);
    }
    return { threads, tasks, cards };
  }

  snapshot(): AgentReportsState {
    const problems = [
      ...[...this.entries.values()].flatMap((entry) => (entry.problem ? [entry.problem] : [])),
      ...this.folderProblems.values(),
    ].sort((a, b) => a.file.localeCompare(b.file));
    return {
      root: this.root,
      state: this.state,
      scannedAt: this.scannedAt,
      complete: this.complete,
      ...this.collect(),
      problems,
      boards: new Map(this.boards),
    };
  }

  /**
   * The card as it is on disk now (for an answer): read fresh, never from the last scan. Null when
   * the share says the card is not there or the file does not pass the reader's rules. Rejects when
   * the share cannot say: that is not the card changing, and the owner must not be told it did.
   */
  async readCardNow(cardId: string): Promise<ParsedReport<CardDoc> | null> {
    const writer = cardId.slice(0, cardId.indexOf('--'));
    if (!isWriterName(writer)) return null;
    const folder = path.join(this.root, 'v1', 'writers', writer, 'cards');
    const file = path.join(folder, `${cardId}.json`);
    // List, then read (see the reading rules above).
    let listed: fs.Dirent[];
    try {
      listed = await this.fs.readdir(folder);
    } catch (error) {
      if (!isMissing(error)) throw error;
      // No `cards/` under a share that is there means no such card. An unmounted root looks the
      // same from here, so ask `v1`; this stat rejects if the share itself is gone.
      await this.fs.stat(path.join(this.root, 'v1'));
      return null;
    }
    if (!listed.some((entry) => entry.name === `${cardId}.json`)) return null;
    const stat = await this.fs.stat(file);
    if (!stat.isFile() || stat.size > REPORT_SIZE_CAPS.card) return null;
    const bytes = await this.fs.readFile(file, REPORT_SIZE_CAPS.card);
    const parsed = parseReportFile('card', bytes, writer, cardId);
    return parsed.ok ? parsed.value : null;
  }
}

function sameProblems(
  before: ReadonlyMap<string, ReportProblem>,
  after: ReadonlyMap<string, ReportProblem>
): boolean {
  if (before.size !== after.size) return false;
  for (const [key, value] of after) if (before.get(key)?.reason !== value.reason) return false;
  return true;
}
