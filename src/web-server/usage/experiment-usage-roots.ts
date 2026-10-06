/**
 * Experiment usage roots: Claude Code, Codex, Muse Code and zcode logs that experiment harnesses
 * write under `~/PM-Experiments` in their own private tool homes, outside the default roots the
 * collectors read. OMP has its own marker scan (omp-native-usage-collector.ts).
 *
 * Roots are found by content, never by folder name: a candidate file counts only when its first
 * (or last) records carry the tool's own record signature. The tree is far larger than one bounded
 * walk (about 800k directories), so discovery is a resumable depth-first walk: each slice reads a
 * bounded number of directories within a time budget, saves its stack, and the next collection
 * continues. A completed round is kept for six hours; a new round's finds are added as they
 * arrive, so roots never regress while it runs. Synthetic sandboxes and fixture/build trees are
 * skipped, and caps bound every part of the walk (see the constants below).
 *
 * The walk is synchronous on purpose: the server runs each slice on a collector worker thread at
 * low priority (native-usage-worker.ts), so it never touches the event loop or the shared libuv
 * pool. Reading the published roots is one small file and stays cheap on the main thread.
 */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { SCAN_SKIP_DIRS } from './omp-native-usage-collector';
import {
  classifyUsageLogFile,
  experimentRootFor,
  isRecord,
  isText,
  isZcodeDatabase,
  sandboxMarker,
  zcodeDigest,
  type ExperimentKind,
} from './experiment-usage-signatures';

export type { ExperimentKind } from './experiment-usage-signatures';

export type ExperimentRootSet = Record<ExperimentKind, string[]>;
const KINDS: ExperimentKind[] = ['claude', 'codex', 'muse', 'zcode'];

/** Directories deeper than this below the base are never read (deepest real tool home: 13). */
export const EXPERIMENT_SCAN_MAX_DEPTH = 14;
/** Roots kept per kind; past it that kind's further finds are dropped and the view says `capped`. */
export const EXPERIMENT_MAX_ROOTS = 2048;
/** Pending directories held between slices; past it the round drops the rest, says `truncated`. */
export const EXPERIMENT_SCAN_MAX_STACK = 100_000;
/** Directories one round may read; past it the round ends early and says `truncated`. */
export const EXPERIMENT_SCAN_MAX_DIRS = 1_500_000;
/** One slice: directories read and time spent before it saves and yields. */
export const EXPERIMENT_SLICE_MAX_DIRS = 60_000;
export const EXPERIMENT_SLICE_MS = 8_000;
/** A completed round is reused this long before a new round starts. */
export const EXPERIMENT_ROOTS_TTL_MS = 6 * 3_600_000;
/** Candidate logs (and, separately, databases) sniffed per directory. */
const MAX_SNIFFS_PER_DIR = 4;
/** A Claude root reads its own folder and two levels below (`<session>/subagents/`). */
export const EXPERIMENT_CLAUDE_ROOT_DEPTH = 2;
const ROOTS_FILE = 'experiment-usage-roots-v1.json';
const WALK_FILE = 'experiment-usage-walk-v1.json';
const MAX_ROOTS_FILE_BYTES = 4 * 1024 * 1024;
const MAX_WALK_FILE_BYTES = 32 * 1024 * 1024;

export function emptyRootSet(): ExperimentRootSet {
  return { claude: [], codex: [], muse: [], zcode: [] };
}

interface RootsFile {
  version: 1;
  base: string;
  roots: ExperimentRootSet;
  /** zcode root -> content digest, so copies found in different rounds still collapse. */
  digests: Record<string, string>;
  completedAt: number;
  scanning: boolean;
  truncated: boolean;
  capped: ExperimentKind[];
}

interface WalkFile {
  version: 1;
  base: string;
  startedAt: number;
  stack: Array<[string, number]>;
  found: ExperimentRootSet;
  digests: Record<string, string>;
  dirs: number;
  truncated: boolean;
  capped: ExperimentKind[];
}

function validRootSet(value: unknown): value is ExperimentRootSet {
  return (
    isRecord(value) &&
    KINDS.every(
      (kind) =>
        Array.isArray(value[kind]) &&
        (value[kind] as unknown[]).length <= EXPERIMENT_MAX_ROOTS &&
        (value[kind] as unknown[]).every((item) => isText(item) && path.isAbsolute(item))
    )
  );
}

function readJson(file: string, maxBytes: number): Record<string, unknown> | null {
  try {
    if (fs.statSync(file).size > maxBytes) return null;
    const value: unknown = JSON.parse(fs.readFileSync(file, 'utf8'));
    return isRecord(value) ? value : null;
  } catch {
    return null;
  }
}

function readRootsFile(cacheDir: string, base: string): RootsFile | null {
  const value = readJson(path.join(cacheDir, ROOTS_FILE), MAX_ROOTS_FILE_BYTES);
  if (
    !value ||
    value.version !== 1 ||
    value.base !== base ||
    !validRootSet(value.roots) ||
    !isRecord(value.digests) ||
    typeof value.completedAt !== 'number' ||
    !Array.isArray(value.capped)
  )
    return null;
  return value as unknown as RootsFile;
}

function readWalkFile(cacheDir: string, base: string): WalkFile | null {
  const value = readJson(path.join(cacheDir, WALK_FILE), MAX_WALK_FILE_BYTES);
  if (
    !value ||
    value.version !== 1 ||
    value.base !== base ||
    !Array.isArray(value.stack) ||
    !validRootSet(value.found) ||
    !isRecord(value.digests) ||
    typeof value.dirs !== 'number' ||
    !Array.isArray(value.capped)
  )
    return null;
  return value as unknown as WalkFile;
}

function writeJson(cacheDir: string, name: string, value: unknown): void {
  try {
    fs.mkdirSync(cacheDir, { recursive: true, mode: 0o700 });
    const file = path.join(cacheDir, name);
    const temporary = `${file}.${process.pid}.tmp`;
    fs.writeFileSync(temporary, JSON.stringify(value), { mode: 0o600 });
    fs.renameSync(temporary, file);
    fs.chmodSync(file, 0o600);
  } catch {
    /* The next slice starts over from the last saved state. */
  }
}

/** One path per zcode content, the first by path order: byte-identical copies are read once. */
function onePerDigest(paths: string[], digests: Record<string, string>): string[] {
  const seen = new Set<string>();
  const kept: string[] = [];
  for (const file of [...paths].sort()) {
    const digest = digests[file] ?? file;
    if (seen.has(digest)) continue;
    seen.add(digest);
    kept.push(file);
  }
  return kept;
}

function mergeRoots(left: ExperimentRootSet, right: ExperimentRootSet): ExperimentRootSet {
  const merged = emptyRootSet();
  for (const kind of KINDS)
    merged[kind] = [...new Set([...left[kind], ...right[kind]])]
      .sort()
      .slice(0, EXPERIMENT_MAX_ROOTS);
  return merged;
}

export interface ExperimentRootsView {
  roots: ExperimentRootSet;
  /** A round is running, or none has completed yet: more roots may still be found. */
  scanning: boolean;
  /** A cap on the walk itself (stack or directories) cut the last round short. */
  truncated: boolean;
  /** Kinds whose root cap dropped further finds. */
  capped: ExperimentKind[];
  /** When the last round completed (0: never). */
  completedAt: number;
}

/**
 * The roots to read now: the last completed round plus what the running round has found so far,
 * never fewer while a round runs. Synchronous and cheap (one small file), so the collectors that
 * use these roots start at once instead of waiting for the walk.
 */
export function readExperimentRoots(
  cacheDir: string,
  options: { homeDir?: string } = {}
): ExperimentRootsView {
  const base = path.join(options.homeDir ?? os.homedir(), 'PM-Experiments');
  const file = readRootsFile(cacheDir, base);
  if (!file)
    return { roots: emptyRootSet(), scanning: true, truncated: false, capped: [], completedAt: 0 };
  return {
    roots: { ...file.roots, zcode: onePerDigest(file.roots.zcode, file.digests) },
    scanning: file.scanning,
    truncated: file.truncated,
    capped: file.capped,
    completedAt: file.completedAt,
  };
}

export interface ExperimentScanOptions {
  homeDir?: string;
  cacheDir: string;
  now?: () => number;
  sliceMs?: number;
  sliceMaxDirs?: number;
  maxDepth?: number;
  maxRoots?: number;
  maxStack?: number;
  maxDirs?: number;
}

function under(child: string, parent: string): boolean {
  if (child === parent) return true;
  return child.startsWith(parent.endsWith(path.sep) ? parent : parent + path.sep);
}

function depthBelow(child: string, parent: string): number {
  const relative = path.relative(parent, child);
  return relative ? relative.split(path.sep).length : 0;
}

/**
 * Run one bounded slice of the discovery walk and save its progress. A no-op while the last
 * completed round is younger than the TTL. Synchronous: the server runs it on a worker thread.
 * It stops at its directory or time budget. Returns whether a round is still running (more
 * slices to come).
 */
export function runExperimentRootsSlice(options: ExperimentScanOptions): boolean {
  const now = options.now ?? Date.now;
  const base = path.join(options.homeDir ?? os.homedir(), 'PM-Experiments');
  const maxDepth = Math.min(
    options.maxDepth ?? EXPERIMENT_SCAN_MAX_DEPTH,
    EXPERIMENT_SCAN_MAX_DEPTH
  );
  const maxRoots = Math.min(options.maxRoots ?? EXPERIMENT_MAX_ROOTS, EXPERIMENT_MAX_ROOTS);
  const maxStack = options.maxStack ?? EXPERIMENT_SCAN_MAX_STACK;
  const maxDirs = options.maxDirs ?? EXPERIMENT_SCAN_MAX_DIRS;
  const deadline = now() + Math.max(1, options.sliceMs ?? EXPERIMENT_SLICE_MS);
  const sliceMaxDirs = options.sliceMaxDirs ?? EXPERIMENT_SLICE_MAX_DIRS;
  const published: RootsFile = readRootsFile(options.cacheDir, base) ?? {
    version: 1,
    base,
    roots: emptyRootSet(),
    digests: {},
    completedAt: 0,
    scanning: true,
    truncated: false,
    capped: [],
  };
  let walk = readWalkFile(options.cacheDir, base);
  if (!walk) {
    if (published.completedAt && now() - published.completedAt <= EXPERIMENT_ROOTS_TTL_MS)
      return false;
    let present = false;
    try {
      present = fs.statSync(base).isDirectory();
    } catch {
      present = false;
    }
    if (!present) {
      // No experiments folder: a completed, empty round.
      writeJson(options.cacheDir, ROOTS_FILE, {
        ...published,
        roots: emptyRootSet(),
        digests: {},
        completedAt: now(),
        scanning: false,
        truncated: false,
        capped: [],
      });
      return false;
    }
    walk = {
      version: 1,
      base,
      startedAt: now(),
      stack: [['', 0]],
      found: emptyRootSet(),
      digests: {},
      dirs: 0,
      truncated: false,
      capped: [],
    };
  }
  const round = walk;
  const accept = (kind: ExperimentKind, root: string): boolean => {
    const list = round.found[kind];
    if (list.includes(root)) return true;
    // A Claude folder already read through a root above it (`<session>/subagents/`) is no new root.
    if (
      kind === 'claude' &&
      list.some(
        (existing) =>
          under(root, existing) && depthBelow(root, existing) <= EXPERIMENT_CLAUDE_ROOT_DEPTH
      )
    )
      return true;
    if (list.length >= maxRoots) {
      if (!round.capped.includes(kind)) round.capped.push(kind);
      return false;
    }
    if (kind === 'zcode') {
      try {
        round.digests[root] = zcodeDigest(root);
      } catch {
        return false;
      }
    }
    list.push(root);
    // Codex and Muse roots are read recursively by their collector, so nothing pending below
    // them needs a visit of its own.
    if (kind === 'codex' || kind === 'muse') {
      const relative = path.relative(base, root);
      round.stack = round.stack.filter(([entry]) => !under(entry, relative));
    }
    return true;
  };
  /** A generator marker also rules out its parent: sandbox scripts build fake homes beside it. */
  const excludeContainer = (relative: string): void => {
    round.stack = round.stack.filter(([entry]) => !under(entry, relative));
    const absolute = path.join(base, relative);
    for (const kind of KINDS)
      round.found[kind] = round.found[kind].filter((root) => !under(root, absolute));
  };
  let sliceDirs = 0;
  while (round.stack.length && sliceDirs < sliceMaxDirs && now() < deadline) {
    if (round.dirs >= maxDirs) {
      round.truncated = true;
      round.stack = [];
      break;
    }
    const [relative, depth] = round.stack.pop() as [string, number];
    const directory = path.join(base, relative);
    let entries: fs.Dirent[];
    try {
      entries = fs.readdirSync(directory, { withFileTypes: true });
    } catch {
      continue;
    }
    sliceDirs++;
    round.dirs++;
    if (relative) {
      const marker = sandboxMarker(directory, entries);
      if (marker) {
        const parent = path.dirname(relative);
        if (marker === 'generator' && parent !== '.') excludeContainer(parent);
        continue;
      }
    }
    const files = entries
      .filter((entry) => entry.isFile())
      .map((entry) => entry.name)
      .sort();
    const rootsHere: string[] = [];
    let sniffs = 0;
    const seenKinds = new Set<ExperimentKind>();
    for (const name of files) {
      if (!name.endsWith('.jsonl') || sniffs >= MAX_SNIFFS_PER_DIR) continue;
      sniffs++;
      const file = path.join(directory, name);
      const kind = classifyUsageLogFile(file);
      if (!kind || seenKinds.has(kind)) continue;
      seenKinds.add(kind);
      const root = experimentRootFor(kind, file);
      if (accept(kind, root) && kind !== 'claude') rootsHere.push(root);
    }
    let databases = 0;
    for (const name of files) {
      if (!/\.(sqlite3?|db)$/.test(name) || databases >= MAX_SNIFFS_PER_DIR) continue;
      databases++;
      const file = path.join(directory, name);
      if (isZcodeDatabase(file)) accept('zcode', file);
    }
    if (depth >= maxDepth) continue;
    // Below a Codex/Muse root found here, the collector reads everything: no visits needed.
    if (rootsHere.some((root) => under(directory, root))) continue;
    // Children are pushed in reverse so the walk visits them in name order.
    const children = entries
      .filter((entry) => entry.isDirectory() && !SCAN_SKIP_DIRS.has(entry.name))
      .map((entry) => entry.name)
      .sort()
      .reverse();
    for (const name of children) {
      if (round.stack.length >= maxStack) {
        round.truncated = true;
        break;
      }
      round.stack.push([relative ? path.join(relative, name) : name, depth + 1]);
    }
  }
  const digests = { ...published.digests, ...round.digests };
  if (!round.stack.length) {
    // Round complete: its finds replace the published roots.
    writeJson(options.cacheDir, ROOTS_FILE, {
      version: 1,
      base,
      roots: { ...round.found, zcode: onePerDigest(round.found.zcode, round.digests) },
      digests: round.digests,
      completedAt: now(),
      scanning: false,
      truncated: round.truncated,
      capped: round.capped,
    } satisfies RootsFile);
    try {
      fs.rmSync(path.join(options.cacheDir, WALK_FILE), { force: true });
    } catch {
      /* A stale walk file is replaced by the next round. */
    }
    return false;
  }
  writeJson(options.cacheDir, WALK_FILE, round);
  // Running round: publish the last round's roots plus everything found so far.
  const roots = mergeRoots(published.roots, round.found);
  writeJson(options.cacheDir, ROOTS_FILE, {
    version: 1,
    base,
    roots: { ...roots, zcode: onePerDigest(roots.zcode, digests) },
    digests,
    completedAt: published.completedAt,
    scanning: true,
    truncated: published.truncated || round.truncated,
    capped: [...new Set([...published.capped, ...round.capped])],
  } satisfies RootsFile);
  return true;
}
