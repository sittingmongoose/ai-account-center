/**
 * Finished runs for the Model Scoreboard: `<reports>/v1/writers/<writer>/runs/<YYYY-MM-DD>/*.json`,
 * validated by the run@1 projection under the Status reader's share rules (see share-files.ts).
 * Every level is listed before it is read; temporary (dot) files and anything not `.json` are
 * ignored. The highest `seq` per run id wins, across date folders. A file that does not read is
 * counted and named, never dropped silently.
 */
import path from 'path';
import {
  isTransientShareError,
  newestFolders,
  nodeReaderFs,
  showable,
  type ReaderFs,
  type ReportProblem,
} from '../status/agent-reports-reader';
import {
  isWriterName,
  parseReportFile,
  REPORT_SIZE_CAPS,
  type RunDoc,
} from '../status/report-schema';
import { byFile, ShareFiles, type ShareFile } from './share-files';

const DATE_FOLDER = /^\d{4}-\d{2}-\d{2}$/;

/**
 * What one scan reads (docs/scoreboard.md), so the share cannot grow the server without bound. More
 * writers than the Status reader's: an old writer's runs are still history. Past a limit the rest
 * is named in the runs Source.
 */
export const RUN_LIMITS = { writers: 1000, runs: 5000, bytes: 64 * 1024 * 1024 };

export interface RunsState {
  /** missing: no `v1/writers` folder; error: the folder could not be listed. */
  state: 'ok' | 'missing' | 'error';
  scannedAt: number | null;
  /** The newest copy of each run, by run id. */
  runs: ShareFile<RunDoc>[];
  writers: number;
  problems: ReportProblem[];
}

export class RunsReader {
  private readonly files: ShareFiles<RunDoc>;
  private readonly folderProblems = new Map<string, ReportProblem>();
  private state: RunsState['state'] = 'missing';
  private scannedAt: number | null = null;
  private scanning: Promise<void> | null = null;

  constructor(
    private readonly options: {
      root: () => string;
      now?: () => number;
      fs?: ReaderFs;
      limits?: Partial<typeof RUN_LIMITS>;
    }
  ) {
    this.files = new ShareFiles<RunDoc>({
      fs: () => this.fs,
      now: () => this.now(),
      capBytes: REPORT_SIZE_CAPS.run,
      parse: (bytes, file) => {
        const parsed = parseReportFile('run', bytes, file.split('/')[1] ?? '');
        return parsed.ok ? { ok: true, value: parsed.value.doc } : parsed;
      },
      seq: (doc) => doc.seq,
    });
  }

  private get fs(): ReaderFs {
    return this.options.fs ?? nodeReaderFs;
  }

  private now(): number {
    return (this.options.now ?? Date.now)();
  }

  /** One rescan; overlapping calls share it. Never rejects. */
  scan(): Promise<void> {
    this.scanning ??= this.runScan()
      .catch(() => {
        this.state = 'error';
        this.scannedAt = this.now();
      })
      .finally(() => {
        this.scanning = null;
      });
    return this.scanning;
  }

  private async runScan(): Promise<void> {
    const writersDir = path.join(this.options.root(), 'v1', 'writers');
    let writers;
    try {
      writers = await this.fs.readdir(writersDir);
    } catch (error) {
      const code = (error as NodeJS.ErrnoException | null)?.code;
      // A stale handle: keep everything and try again (the source turns stale if it persists).
      if (code !== 'ENOENT' && isTransientShareError(error)) return;
      this.state = code === 'ENOENT' || code === 'ENOTDIR' ? 'missing' : 'error';
      this.scannedAt = this.now();
      return;
    }
    this.state = 'ok';
    const limits = { ...RUN_LIMITS, ...this.options.limits };
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
    /** Lists a folder; on failure keeps what is known under it and names a lasting failure. */
    const listing = async (dir: string, rel: string) => {
      try {
        return await this.fs.readdir(dir);
      } catch (error) {
        if (!isTransientShareError(error)) folderProblem(rel, 'folder could not be listed');
        this.files.keep(`${rel}/`, seen);
        return null;
      }
    };
    // A folder that is not a writer name is the Status reader's problem to name; no runs in it.
    const names = writers
      .filter((w) => !w.name.startsWith('.') && w.isDirectory() && isWriterName(w.name))
      .map((w) => w.name)
      .sort();
    // By `runs/`, not the writer folder: a new day's folder changes the first and not the second.
    const kept = await newestFolders(names, limits.writers, (name) =>
      this.fs.stat(path.join(writersDir, name, 'runs'))
    );
    if (kept.length < names.length)
      folderProblem(
        'limits/run-writers',
        `${names.length - kept.length} writer folders past the limit of ${limits.writers} were not read (the most recently changed are kept)`
      );
    const candidates: Array<{ date: string; absolute: string; rel: string }> = [];
    for (const name of kept) {
      const writerRel = `writers/${name}`;
      const inWriter = await listing(path.join(writersDir, name), writerRel);
      if (!inWriter?.some((entry) => entry.name === 'runs')) continue;
      const runsRel = `${writerRel}/runs`;
      const dates = await listing(path.join(writersDir, name, 'runs'), runsRel);
      for (const date of dates ?? []) {
        if (date.name.startsWith('.') || !date.isDirectory()) continue;
        const dateRel = `${runsRel}/${date.name}`;
        if (!DATE_FOLDER.test(date.name)) {
          if (showable(date.name)) folderProblem(dateRel, 'folder name is not a date');
          continue;
        }
        const dateDir = path.join(writersDir, name, 'runs', date.name);
        for (const file of (await listing(dateDir, dateRel)) ?? []) {
          if (file.name.startsWith('.') || !file.name.endsWith('.json')) continue;
          if (!showable(file.name)) continue;
          if (!file.isFile()) {
            folderProblem(`${dateRel}/${file.name}`, 'is not a regular file');
            continue;
          }
          candidates.push({
            date: date.name,
            absolute: path.join(dateDir, file.name),
            rel: `${dateRel}/${file.name}`,
          });
        }
      }
    }
    // Over a limit the newest days are the ones kept, whichever writer they came from.
    candidates.sort((a, b) => b.date.localeCompare(a.date) || a.rel.localeCompare(b.rel));
    let visited = 0;
    let bytes = 0;
    for (const candidate of candidates) {
      if (visited >= limits.runs || bytes >= limits.bytes) break;
      bytes += await this.files.visit(candidate.absolute, candidate.rel, seen);
      visited += 1;
    }
    if (visited < candidates.length)
      folderProblem(
        'limits/runs',
        `${candidates.length - visited} run files past the limit of ${limits.runs} runs or ${limits.bytes / (1024 * 1024)} MB were not read (the newest days are kept)`
      );
    this.files.sweep(seen);
    this.folderProblems.clear();
    for (const [key, value] of folderProblems) this.folderProblems.set(key, value);
    this.scannedAt = this.now();
  }

  snapshot(): RunsState {
    const best = new Map<string, ShareFile<RunDoc>>();
    for (const file of this.files.good()) {
      const held = best.get(file.doc.id);
      if (!held || newer(file, held)) best.set(file.doc.id, file);
    }
    const runs = [...best.values()].sort((a, b) => a.doc.id.localeCompare(b.doc.id));
    return {
      state: this.state,
      scannedAt: this.scannedAt,
      runs,
      writers: new Set(runs.map((run) => run.doc.writer)).size,
      problems: byFile([...this.files.problems(), ...this.folderProblems.values()]),
    };
  }
}

/** Highest `seq`; a tie goes to the later write, then the later file name, so it never flips. */
function newer(a: ShareFile<RunDoc>, b: ShareFile<RunDoc>): boolean {
  if (a.doc.seq !== b.doc.seq) return a.doc.seq > b.doc.seq;
  const written = Date.parse(a.doc.writtenAt) - Date.parse(b.doc.writtenAt);
  return written !== 0 ? written > 0 : a.file > b.file;
}
