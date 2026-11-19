/**
 * Finished runs for the Model Scoreboard: `<reports>/v1/writers/<writer>/runs/<YYYY-MM-DD>/*.json`,
 * validated by the run@1 projection under the Status reader's share rules (see share-files.ts).
 * Every level is listed before it is read; temporary (dot) files and anything not `.json` are
 * ignored. The highest `seq` per run id wins, across date folders. A file that does not read is
 * counted and named, never dropped silently.
 */
import path from 'path';
import { resolveRunsState } from './run-corrections';
import type { RunsState } from './runs-types';
export type { RunsState } from './runs-types';
import {
  isTransientShareError,
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
 * writers than the Status reader's: an old writer's runs are still history. `runs` and `bytes` bound
 * the run files read (the newest days are kept); `writers` bounds the writers whose date folders are
 * listed at all, so past it the not-read run file count is a lower bound. Past a limit the rest is
 * named in the runs Source.
 */
export const RUN_LIMITS = { writers: 20000, runs: 50000, bytes: 256 * 1024 * 1024 };

export class RunsReader {
  private readonly files: ShareFiles<RunDoc>;
  private readonly folderProblems = new Map<string, ReportProblem>();
  /** ENOENT or ESTALE on a folder listing: the race once, a named problem on the next scan too. */
  private readonly listingMisses = new TransientMisses();
  /**
   * The run-file identities (report paths) each date folder held at its last good listing. A folder
   * that cannot be listed now still counts the files this lists that the reader has no copy or
   * problem for, reconciled by identity so a grace-held copy of a different file never cancels a
   * never-read one. A provisional last-known count (files added or removed since are unknown), not
   * a current count or a guaranteed lower bound; the runs Source says so. Kept for every writer the
   * writers listing still names, so a writer the cap pushes out keeps its known count for a later
   * failed listing.
   */
  private dateFileIds = new Map<string, Set<string>>();
  private state: RunsState['state'] = 'missing';
  private scannedAt: number | null = null;
  private skipped = { runFiles: 0, writerFolders: 0 };
  /** The part of `skipped.runFiles` counted from failed listings' last good identities (provisional). */
  private provisionalRunFiles = 0;
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
    /** Every folder this scan could not list, transient or not: its runs count as not read. */
    const unlisted = new Set<string>();
    /**
     * Lists a folder; on failure keeps what is known under it and names a lasting failure: any
     * error but ENOENT or ESTALE at once, those two when the scan before failed the same way too.
     */
    const listing = async (dir: string, rel: string) => {
      try {
        return await this.fs.readdir(dir);
      } catch (error) {
        unlisted.add(rel);
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
    // Every writer's `runs/` date folders: one listing per writer, whichever side of the writer cap
    // it lands on, because writers rank by their newest run-date folder — not by the `runs/` folder's
    // mtime, which moves only when a new day starts (a dropped writer could hold newer runs). The
    // folder may be empty: opening past-cap writers' date folders to prove a run supports the date
    // would lift the listing bound, so the ranking is date-folder based and the cap reason says so.
    const datesOf = async (name: string): Promise<{ name: string; dates: string[] }> => {
      const writerRel = `writers/${name}`;
      const inWriter = await listing(path.join(writersDir, name), writerRel);
      if (!inWriter?.some((entry) => entry.name === 'runs')) return { name, dates: [] };
      const runsRel = `${writerRel}/runs`;
      const entries = await listing(path.join(writersDir, name, 'runs'), runsRel);
      const dates: string[] = [];
      for (const entry of entries ?? []) {
        if (entry.name.startsWith('.')) continue;
        if (!entry.isDirectory()) {
          if (showable(entry.name)) folderProblem(`${runsRel}/${entry.name}`, 'is not a folder');
          continue;
        }
        if (!DATE_FOLDER.test(entry.name)) {
          if (showable(entry.name))
            folderProblem(`${runsRel}/${entry.name}`, 'folder name is not a date');
          continue;
        }
        dates.push(entry.name);
      }
      return { name, dates };
    };
    const ranked: Array<{ name: string; dates: string[] }> = [];
    for (let offset = 0; offset < names.length; offset += 16) {
      const batch = names.slice(offset, offset + 16);
      const parts = await Promise.all(batch.map((name) => datesOf(name)));
      for (const part of parts) ranked.push(part);
    }
    const newestOf = (dates: string[]) =>
      dates.reduce((newest, date) => (date > newest ? date : newest), '');
    ranked.sort(
      (a, b) => newestOf(b.dates).localeCompare(newestOf(a.dates)) || a.name.localeCompare(b.name)
    );
    const kept = ranked.slice(0, limits.writers);
    if (kept.length < ranked.length) {
      folderProblem(
        'limits/run-writers',
        `${ranked.length - kept.length} writer folders past the limit of ${limits.writers} were not listed (writers rank by their newest run-date folder, which may be empty, so a newer empty date folder can outrank a writer with newer runs), so their run files are not read and not counted: the not-read run file count is a lower bound`
      );
      // A writer the cap pushed out was not listed, so its files leave at once, like a past-cap run
      // file, not after the grace period.
      for (const { name } of ranked.slice(limits.writers))
        this.files.forgetUnder(`writers/${name}/`);
    }
    const candidates: Array<{ date: string; absolute: string; rel: string }> = [];
    /** Run-file identities listed in one date folder, remembered for a scan whose listing fails. */
    const listedIds = new Map<string, Set<string>>();
    // One writer's run files. Dates under one writer stay listed in order; writers run 16 at a time.
    const collectWriter = async (name: string, dates: string[]) => {
      const out: Array<{ date: string; absolute: string; rel: string }> = [];
      for (const date of dates) {
        const dateRel = `writers/${name}/runs/${date}`;
        const dateDir = path.join(writersDir, name, 'runs', date);
        const entries = await listing(dateDir, dateRel);
        // A failed listing keeps the folder's last-known identities (carried forward below); only
        // a good listing refreshes them.
        if (entries === null) continue;
        const files: Array<{ date: string; absolute: string; rel: string }> = [];
        for (const file of entries) {
          if (file.name.startsWith('.') || !file.name.endsWith('.json')) continue;
          if (!runFileNameReadable(file.name)) continue;
          if (!file.isFile()) {
            folderProblem(`${dateRel}/${file.name}`, 'is not a regular file');
            continue;
          }
          files.push({
            date,
            absolute: path.join(dateDir, file.name),
            rel: `${dateRel}/${file.name}`,
          });
        }
        listedIds.set(dateRel, new Set(files.map((file) => file.rel)));
        out.push(...files);
      }
      return out;
    };
    for (let offset = 0; offset < kept.length; offset += 16) {
      const batch = kept.slice(offset, offset + 16);
      const parts = await Promise.all(
        batch.map((writer) => collectWriter(writer.name, writer.dates))
      );
      for (const part of parts) candidates.push(...part);
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
    // Carry forward every folder whose writer the writers listing still names (a writer the cap
    // pushed out keeps its known count for a later failed listing), then refresh the folders this
    // scan listed well. A writer gone from the listing leaves no folder behind.
    const present = new Set(names);
    const nextIds = new Map<string, Set<string>>();
    for (const [folder, ids] of this.dateFileIds) {
      const writer = folder.split('/')[1];
      if (writer !== undefined && present.has(writer)) nextIds.set(folder, ids);
    }
    for (const [folder, ids] of listedIds) nextIds.set(folder, ids);
    // Runs under a folder this scan could not list: never read, so the files its last good listing
    // named that the reader has neither a copy nor a problem for stay counted as not read. A
    // provisional last-known count (files added or removed since are unknown), with the folder
    // problem naming why and the runs Source qualifying the count.
    let unlistedRuns = 0;
    if (unlisted.size > 0) {
      const held = new Set<string>();
      for (const file of this.files.good()) held.add(file.file);
      for (const problem of this.files.problems()) held.add(problem.file);
      const folders = new Set<string>();
      for (const rel of unlisted) {
        const prefix = `${rel}/`;
        for (const folder of nextIds.keys())
          if (folder === rel || folder.startsWith(prefix)) folders.add(folder);
      }
      for (const folder of folders) {
        // Reconcile by identity: only a file the last good listing named that the reader still holds
        // is already accounted for. A grace-held copy of a different file the listing no longer
        // names never cancels a never-read one.
        for (const file of nextIds.get(folder) ?? []) if (!held.has(file)) unlistedRuns += 1;
      }
    }
    this.dateFileIds = nextIds;
    this.provisionalRunFiles = unlistedRuns;
    const skippedRunFiles = candidates.length - visited + unlistedRuns;
    if (visited < candidates.length)
      folderProblem(
        'limits/runs',
        `${candidates.length - visited} run files past the limit of ${limits.runs} run files or ${limits.bytes / (1024 * 1024)} MB were not read (the newest days are kept)`
      );
    // A file past a cap was listed, not missing: forget it at once so a grace-held copy is not also
    // scored while it is counted as not read on the same scan.
    for (const cand of candidates.slice(visited)) this.files.forget(cand.rel);
    this.skipped = {
      runFiles: skippedRunFiles,
      writerFolders: names.length - kept.length,
    };
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
    return resolveRunsState({
      state: this.state,
      // While a scan runs the runs are what it has read so far: the view marks that partial.
      ...(this.scanning !== null ? { partial: true } : {}),
      ...(this.provisionalRunFiles > 0 ? { provisionalRunFiles: this.provisionalRunFiles } : {}),
      scannedAt: this.scannedAt,
      runs,
      writers: new Set(runs.map((run) => run.doc.writer)).size,
      skipped: { ...this.skipped },
      problems: byFile([...this.files.problems(), ...this.folderProblems.values()]),
    });
  }
}

/** Highest `seq`; a tie goes to the later write, then the later file name, so it never flips. */
function newer(a: ShareFile<RunDoc>, b: ShareFile<RunDoc>): boolean {
  if (a.doc.seq !== b.doc.seq) return a.doc.seq > b.doc.seq;
  const written = Date.parse(a.doc.writtenAt) - Date.parse(b.doc.writtenAt);
  return written !== 0 ? written > 0 : a.file > b.file;
}
