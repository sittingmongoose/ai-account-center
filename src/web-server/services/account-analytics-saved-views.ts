import * as fs from 'fs';
import * as path from 'path';
import type { AccountAnalytics } from './account-analytics-types';

/**
 * The last computed Analytics answer of each recent selection, kept in memory and saved under the CCS folder
 * (`account-analytics/analytics-views-v1.json`), so a read is answered at once with the newest numbers while the next
 * ones are computed, and a restarted server answers its first read from the saved answer instead of from scratch.
 *
 * The file holds whole answers, exactly as they were sent: the same account labels and usage numbers the history
 * folder beside it already holds, never a log path, a session id or content. Written atomically (temporary file,
 * then rename) with owner-only permissions; a missing, oversized, unreadable or older-version file is ignored.
 */
export const SAVED_VIEWS_VERSION = 1;
const FILE = 'analytics-views-v1.json';
/** Selections kept: the presets and filters a person moves between. */
export const MAX_SAVED_VIEWS = 8;
const MAX_FILE_BYTES = 24 * 1024 * 1024;
const MAX_KEY_LENGTH = 1024;
/** A burst of answers is saved once, this long after the last; and at most this often. */
const SAVE_DEBOUNCE_MS = 2_000;
const SAVE_MIN_INTERVAL_MS = 15_000;

interface SavedViewsFile {
  version: typeof SAVED_VIEWS_VERSION;
  views: Array<[string, AccountAnalytics]>;
}

function validAnswer(value: unknown): value is AccountAnalytics {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const answer = value as Record<string, unknown>;
  return (
    answer.schemaVersion === 1 &&
    typeof answer.updatedAt === 'string' &&
    Number.isFinite(Date.parse(answer.updatedAt)) &&
    !!answer.range &&
    typeof answer.range === 'object' &&
    !!answer.activity &&
    typeof answer.activity === 'object' &&
    Array.isArray(answer.accounts) &&
    Array.isArray(answer.providers)
  );
}

export function validSavedViews(value: unknown): value is SavedViewsFile {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const file = value as Record<string, unknown>;
  if (file.version !== SAVED_VIEWS_VERSION || !Array.isArray(file.views)) return false;
  if (file.views.length > MAX_SAVED_VIEWS) return false;
  return file.views.every(
    (entry) =>
      Array.isArray(entry) &&
      entry.length === 2 &&
      typeof entry[0] === 'string' &&
      entry[0].length <= MAX_KEY_LENGTH &&
      validAnswer(entry[1])
  );
}

/** One scope's saved answers. `persist` false keeps them in memory only (tests, ad-hoc scopes). */
export class AnalyticsSavedViews {
  private readonly views = new Map<string, AccountAnalytics>();
  /** Selections whose answer came from the file and has not been computed again since. */
  private readonly loadedFromFile = new Set<string>();
  /** When each held answer's computation started (the service's own counter), never a clock time. */
  private readonly started = new Map<string, number>();
  private loaded = false;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private lastSave = -Infinity;
  private dirty = false;

  constructor(
    private readonly scope: string,
    private readonly persist: boolean,
    private readonly now: () => number = Date.now
  ) {}

  private file(): string | null {
    // Only an existing absolute scope is written to; test and ad-hoc scopes are not directories.
    if (!this.persist || !path.isAbsolute(this.scope)) return null;
    try {
      if (!fs.statSync(this.scope).isDirectory()) return null;
    } catch {
      return null;
    }
    return path.join(this.scope, 'account-analytics', FILE);
  }

  private load(): void {
    if (this.loaded) return;
    this.loaded = true;
    try {
      const file = this.file();
      if (!file || fs.statSync(file).size > MAX_FILE_BYTES) return;
      const value = JSON.parse(fs.readFileSync(file, 'utf8')) as unknown;
      if (!validSavedViews(value)) return;
      for (const [key, answer] of value.views) {
        this.views.set(key, answer);
        this.loadedFromFile.add(key);
      }
    } catch {
      /* A missing or unreadable file behaves like a first run. */
    }
  }

  get(key: string): AccountAnalytics | undefined {
    this.load();
    const answer = this.views.get(key);
    if (answer) {
      // most recently read last, so the least used selection leaves first
      this.views.delete(key);
      this.views.set(key, answer);
    }
    return answer;
  }

  /** Whether the selection's answer came from the saved file (an earlier server) and was not computed since. */
  fromDisk(key: string): boolean {
    return this.loadedFromFile.has(key);
  }

  /**
   * Keeps `answer` unless the one held came from a computation that started later (two computations can finish out
   * of order). `sequence` orders computations by when they started, so a wall clock stepped back never makes a new
   * answer look older than the held one; an answer read from the file is replaced by any computation.
   */
  set(key: string, answer: AccountAnalytics, sequence: number): void {
    this.load();
    const held = this.started.get(key);
    if (held !== undefined && held > sequence) return;
    this.loadedFromFile.delete(key);
    this.started.set(key, sequence);
    this.views.delete(key);
    this.views.set(key, answer);
    while (this.views.size > MAX_SAVED_VIEWS) {
      const oldest = this.views.keys().next().value;
      if (oldest === undefined) break;
      this.views.delete(oldest);
      this.started.delete(oldest);
      this.loadedFromFile.delete(oldest);
    }
    this.dirty = true;
    this.schedule();
  }

  private schedule(): void {
    if (!this.file()) return;
    if (this.timer) clearTimeout(this.timer);
    const wait = Math.max(SAVE_DEBOUNCE_MS, this.lastSave + SAVE_MIN_INTERVAL_MS - this.now());
    this.timer = setTimeout(() => {
      this.timer = undefined;
      this.save();
    }, wait);
    this.timer.unref?.();
  }

  /** Writes the held answers now (also called at shutdown). */
  save(): void {
    if (!this.dirty) return;
    const file = this.file();
    if (!file) return;
    this.dirty = false;
    this.lastSave = this.now();
    try {
      const body = JSON.stringify({
        version: SAVED_VIEWS_VERSION,
        views: [...this.views],
      } satisfies SavedViewsFile);
      if (Buffer.byteLength(body, 'utf8') > MAX_FILE_BYTES) return;
      fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
      const temporary = `${file}.${process.pid}.tmp`;
      fs.writeFileSync(temporary, body, { mode: 0o600 });
      fs.renameSync(temporary, file);
      fs.chmodSync(file, 0o600);
    } catch {
      /* Best-effort: the answers are still served from memory. */
    }
  }

  /** Stops a pending write (tests); `flush` writes it first. */
  close(flush = false): void {
    if (this.timer) clearTimeout(this.timer);
    this.timer = undefined;
    if (flush) this.save();
  }
}
