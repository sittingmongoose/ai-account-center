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
  PERSISTENT_LISTING_ERROR,
  showable,
  TransientMisses,
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
 * A run file's name, `<run id>.json`: printable ASCII and up to 200 characters, so every run id the
 * run schema accepts (RUN_ID_MAX, 194) has a readable file. Longer than the 100 the Status reader
 * shows for other names: a run file past that was skipped without a word (2026-11-13).
 */
const RUN_FILE_NAME = /^[\x21-\x7e]{1,200}$/;

export function runFileNameReadable(name: string): boolean {
  return RUN_FILE_NAME.test(name) && !name.includes('/') && !name.includes('\\');
}

/**
 * What one scan reads (docs/scoreboard.md), so the share cannot grow the server without bound. More
 * writers than the Status reader's: an old writer's runs are still history. Past a limit the rest
 * is named in the runs Source.
 */
export const RUN_LIMITS = { writers: 1000, runs: 50000, bytes: 256 * 1024 * 1024 };

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
  /** ENOENT or ESTALE on a folder listing: the race once, a named problem on the next scan too. */
  private readonly listingMisses = new TransientMisses();
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
    const folderProblem = (file: string, reason: string, since = this.now()) => {
      const before = this.folderProblems.get(file);
      folderProblems.set(file, {
        file,
        reason,
        since: before?.reason === reason ? before.since : since,
      });
    };
    const failedListings = new Set<string>();
    /**
     * Lists a folder; on failure keeps what is known under it and names a lasting failure: any
     * error but ENOENT or ESTALE at once, those two when the scan before failed the same way too.
     */
    const listing = async (dir: string, rel: string) => {
      try {
        return await this.fs.readdir(dir);
      } catch (error) {
        if (!isTransientShareError(error)) folderProblem(rel, 'folder could not be listed');
        else {
          failedListings.add(rel);
          const first = this.listingMisses.miss(rel, this.now());
          if (first !== null) folderProblem(rel, PERSISTENT_LISTING_ERROR, first);
        }
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
        if (date.name.startsWith('.')) continue;
        const dateRel = `${runsRel}/${date.name}`;
        if (!date.isDirectory()) {
          if (showable(date.name)) folderProblem(dateRel, 'is not a folder');
          continue;
        }
        if (!DATE_FOLDER.test(date.name)) {
          if (showable(date.name)) folderProblem(dateRel, 'folder name is not a date');
          continue;
        }
        const dateDir = path.join(writersDir, name, 'runs', date.name);
        for (const file of (await listing(dateDir, dateRel)) ?? []) {
          if (file.name.startsWith('.') || !file.name.endsWith('.json')) continue;
          if (!runFileNameReadable(file.name)) continue;
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
    // Preflight in bounded batches, then admit the same sorted prefix under
    // the byte cap before parallel reads. Completion order never chooses winners.
    for (
      let offset = 0;
      offset < candidates.length && visited < limits.runs && bytes < limits.bytes;
      offset += 16
    ) {
      const batch = candidates.slice(offset, Math.min(offset + 16, limits.runs));
      const sizes = await Promise.all(
        batch.map((candidate) =>
          this.fs.stat(candidate.absolute).then(
            (stat) => (stat.isFile() && stat.size <= REPORT_SIZE_CAPS.run ? stat.size : 0),
            () => 0
          )
        )
      );
      const admitted = [];
      for (let index = 0; index < batch.length && bytes < limits.bytes; index++) {
        admitted.push(batch[index]);
        bytes += sizes[index];
        visited++;
      }
      await Promise.all(
        admitted.map((candidate) => this.files.visit(candidate.absolute, candidate.rel, seen))
      );
    }
    if (visited < candidates.length)
      folderProblem(
        'limits/runs',
        `${candidates.length - visited} run files past the limit of ${limits.runs} runs or ${limits.bytes / (1024 * 1024)} MB were not read (the newest days are kept)`
      );
    this.files.sweep(seen);
    // A folder listed well this scan (or not tried) starts over.
    this.listingMisses.keepOnly(failedListings);
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
