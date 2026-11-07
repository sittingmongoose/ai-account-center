/**
 * One set of JSON files on the reports share, under the Status reader's rules
 * (docs/status-tab.md, Reading rules): the caller lists a folder first and visits what the listing
 * names; a file is read only when its size, time or inode changed; a file that later fails keeps its
 * last good copy with a named problem; ENOENT and ESTALE on a listed file keep the copy and retry
 * next scan, and the same error on the next scan too is a named problem (since the first failure);
 * a file absent from its listing counts as removed only after the grace period.
 */
import {
  isTransientShareError,
  MISSING_GRACE_MS,
  PERSISTENT_SHARE_ERROR,
  TransientMisses,
  type ReaderFs,
  type ReportProblem,
} from '../status/agent-reports-reader';
import type { Parsed } from '../status/schema-helpers';

export interface ShareFile<T> {
  /** Relative to `<reports>/v1`. */
  file: string;
  doc: T;
  /** When AAC first saw this version (epoch ms). */
  firstSeenAt: number;
}

interface Entry<T> {
  statKey: string;
  good: ShareFile<T> | null;
  problem: ReportProblem | null;
  missingSince: number | null;
}

export class ShareFiles<T> {
  private readonly entries = new Map<string, Entry<T>>();
  private readonly misses = new TransientMisses();

  constructor(
    private readonly options: {
      fs: () => ReaderFs;
      now: () => number;
      capBytes: number;
      parse: (bytes: Buffer, file: string) => Parsed<T>;
      /** Documents with a sequence: a lower one than the copy held is an older write landing late. */
      seq?: (doc: T) => number;
    }
  ) {}

  private entry(file: string): Entry<T> {
    let entry = this.entries.get(file);
    if (!entry) {
      entry = { statKey: '', good: null, problem: null, missingSince: null };
      this.entries.set(file, entry);
    }
    return entry;
  }

  private setProblem(file: string, reason: string, statKey = '', since = this.options.now()): void {
    const entry = this.entry(file);
    if (entry.problem?.reason !== reason) entry.problem = { file, reason, since };
    // A read error keeps the key empty so the next scan tries again.
    entry.statKey = statKey;
  }

  /** ENOENT or ESTALE on a listed file: the race once, a named problem when the last scan had it too. */
  private transient(file: string): void {
    const since = this.misses.miss(file, this.options.now());
    if (since !== null) this.setProblem(file, PERSISTENT_SHARE_ERROR, '', since);
  }

  /**
   * Stat and, when changed, read one listed file. `absolute` is on disk, `file` is its report path.
   * Resolves the file's size, for a caller that caps the bytes held per scan.
   */
  async visit(absolute: string, file: string, seen: Set<string>): Promise<number> {
    const { fs, capBytes, parse, seq, now } = this.options;
    let statKey: string;
    let size: number;
    try {
      const stat = await fs().stat(absolute);
      seen.add(file);
      if (!stat.isFile()) {
        this.setProblem(file, 'is not a regular file');
        return 0;
      }
      statKey = `${stat.size}:${stat.mtimeMs}:${stat.ino}:${stat.ctimeMs}`;
      size = stat.size;
    } catch (error) {
      seen.add(file);
      if (isTransientShareError(error)) this.transient(file);
      else {
        this.misses.clear(file);
        this.setProblem(file, 'could not be read');
      }
      return 0;
    }
    // Before the unchanged check: a file over its cap is never read, on this scan or any later one,
    // so it never takes from the scan's byte budget.
    if (size > capBytes) {
      this.setProblem(file, `larger than the ${capBytes / 1024} KB cap`, statKey);
      return 0;
    }
    if (this.entries.get(file)?.statKey === statKey) {
      this.misses.clear(file);
      return size;
    }
    let bytes: Buffer;
    try {
      bytes = await fs().readFile(absolute, capBytes);
    } catch (error) {
      if (isTransientShareError(error)) this.transient(file);
      else {
        this.misses.clear(file);
        this.setProblem(file, 'could not be read');
      }
      return size;
    }
    this.misses.clear(file);
    const parsed = parse(bytes, file);
    if (!parsed.ok) {
      this.setProblem(file, parsed.reason, statKey);
      return size;
    }
    const entry = this.entry(file);
    entry.statKey = statKey;
    entry.problem = null;
    if (entry.good && seq && seq(parsed.value) < seq(entry.good.doc)) return size;
    entry.good = { file, doc: parsed.value, firstSeenAt: now() };
    return size;
  }

  /** Marks every file under a folder that could not be listed: not listed is not withdrawn. */
  keep(prefix: string, seen: Set<string>): void {
    for (const file of this.entries.keys()) if (file.startsWith(prefix)) seen.add(file);
  }

  /** After a scan: forgets files absent from their listing for longer than the grace period. */
  sweep(seen: Set<string>): void {
    this.misses.keepOnly(seen);
    const now = this.options.now();
    for (const [file, entry] of [...this.entries]) {
      if (seen.has(file)) {
        entry.missingSince = null;
        continue;
      }
      entry.missingSince ??= now;
      if (now - entry.missingSince >= MISSING_GRACE_MS) this.entries.delete(file);
    }
  }

  good(): ShareFile<T>[] {
    return [...this.entries.values()].flatMap((entry) => (entry.good ? [entry.good] : []));
  }

  get(file: string): ShareFile<T> | null {
    return this.entries.get(file)?.good ?? null;
  }

  problems(): ReportProblem[] {
    return [...this.entries.values()].flatMap((entry) => (entry.problem ? [entry.problem] : []));
  }
}

/** Problems sorted by file, for the view. */
export function byFile(problems: ReportProblem[]): ReportProblem[] {
  return [...problems].sort((a, b) => a.file.localeCompare(b.file));
}
