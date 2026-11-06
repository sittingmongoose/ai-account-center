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

/**
 * A file absent from its folder's listing is kept (last good copy) this long before it counts as
 * removed: a Mac rename-over through SMB is a delete then a rename, and NFS caches the gap.
 */
export const MISSING_GRACE_MS = 120_000;

export interface ReaderFs {
  readdir(dir: string): Promise<fs.Dirent[]>;
  stat(file: string): Promise<fs.Stats>;
  readFile(file: string): Promise<Buffer>;
}

const nodeFs: ReaderFs = {
  readdir: (dir) => fs.promises.readdir(dir, { withFileTypes: true }),
  stat: (file) => fs.promises.stat(file),
  readFile: (file) => fs.promises.readFile(file),
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
function showable(name: string): boolean {
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

export class AgentReportsReader {
  private readonly entries = new Map<string, Entry>();
  private readonly folderProblems = new Map<string, ReportProblem>();
  private boards = new Map<string, number | null>();
  private state: AgentReportsState['state'] = 'missing';
  private scannedAt: number | null = null;
  private scanning: Promise<boolean> | null = null;

  constructor(
    private readonly options: {
      root?: () => string;
      now?: () => number;
      fs?: ReaderFs;
    } = {}
  ) {}

  get root(): string {
    return (this.options.root ?? agentReportsRoot)();
  }

  private now(): number {
    return (this.options.now ?? Date.now)();
  }

  private get fs(): ReaderFs {
    return this.options.fs ?? nodeFs;
  }

  /** One rescan; resolves whether anything the view shows changed. Overlapping calls share one scan. */
  scan(): Promise<boolean> {
    if (!this.scanning)
      this.scanning = this.runScan().finally(() => {
        this.scanning = null;
      });
    return this.scanning;
  }

  private async runScan(): Promise<boolean> {
    const v1 = path.join(this.root, 'v1');
    const writersDir = path.join(v1, 'writers');
    let changed = false;
    let writers: fs.Dirent[];
    try {
      writers = await this.fs.readdir(writersDir);
    } catch (error) {
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
    for (const dirent of writers) {
      const name = dirent.name;
      if (name.startsWith('.')) continue;
      if (!dirent.isDirectory()) continue;
      if (!isWriterName(name)) {
        if (showable(name)) folderProblem(`writers/${name}`, 'folder name is not a writer name');
        continue;
      }
      const base = path.join(writersDir, name);
      // List the folder first, then read: a listing refreshes the NFS view of a rename-over.
      let listed: Set<string>;
      try {
        listed = new Set((await this.fs.readdir(base)).map((entry) => entry.name));
      } catch (error) {
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
        if (!isTransientShareError(error))
          folderProblem(`writers/${name}/cards`, 'folder could not be listed');
        // A listing failure is not a withdrawal: keep the cards already known.
        keepWriter(name, 'cards/');
      }
      for (const card of cards) {
        if (card.name.startsWith('.') || !card.name.endsWith('.json') || !card.isFile()) continue;
        const id = card.name.slice(0, -'.json'.length);
        const rel = `writers/${name}/cards/${card.name}`;
        if (!showable(card.name)) continue;
        if (await this.visit(path.join(base, 'cards', card.name), rel, 'card', name, id, seen))
          changed = true;
      }
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
      // Listed but gone or stale: keep the last good copy and read again next scan.
      if (isTransientShareError(error)) return false;
      return this.setProblem(rel, kind, writer, 'could not be read');
    }
    if (!stat.isFile()) return false;
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
      bytes = await this.fs.readFile(file);
    } catch (error) {
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
    const hadProblem = entry.problem !== null;
    entry.statKey = statKey;
    entry.problem = null;
    this.entries.set(rel, entry);
    if (entry.good && parsed.doc.seq < entry.good.doc.seq) {
      // An older write landing late (or a writer that restarted its count): ignored.
      entry.good = { ...entry.good, problem: null };
      return hadProblem;
    }
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
      ...this.collect(),
      problems,
      boards: new Map(this.boards),
    };
  }

  /**
   * The card as it is on disk now (for an answer): read fresh, never from the last scan. Null when
   * the file is gone or does not pass the reader's rules.
   */
  async readCardNow(cardId: string): Promise<ParsedReport<CardDoc> | null> {
    const writer = cardId.slice(0, cardId.indexOf('--'));
    if (!isWriterName(writer)) return null;
    const folder = path.join(this.root, 'v1', 'writers', writer, 'cards');
    const file = path.join(folder, `${cardId}.json`);
    try {
      // List, then read (see the reading rules above).
      const listed = await this.fs.readdir(folder);
      if (!listed.some((entry) => entry.name === `${cardId}.json`)) return null;
      const stat = await this.fs.stat(file);
      if (!stat.isFile() || stat.size > REPORT_SIZE_CAPS.card) return null;
      const parsed = parseReportFile('card', await this.fs.readFile(file), writer, cardId);
      return parsed.ok ? parsed.value : null;
    } catch {
      return null;
    }
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
