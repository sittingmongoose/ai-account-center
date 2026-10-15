/** Resumable OMP marker discovery, independent of the usage-log reader. */
import * as fs from 'fs';
import * as path from 'path';

export interface OmpDiscoveryBounds {
  maxDepth?: number;
  maxDirs?: number;
  maxEntries?: number;
  deadlineMs?: number;
}

interface DiscoveryRules {
  version: number;
  maxDepth: number;
  maxRoots: number;
  ttlMs: number;
  skipDirs: ReadonlySet<string>;
  isSessionFilename: (name: string) => boolean;
  isSandbox: (directory: string, entries: fs.Dirent[]) => Promise<boolean>;
  /** null means absence could not be confirmed (read failure or search bound). */
  hasSessionMarker: (directory: string) => Promise<boolean | null>;
}

type PendingDirectory = [relative: string, depth: number];
interface ChildWalk {
  name: string;
  stamp: string;
  completedAt: number;
  lastPass: number;
  /** Last completed roots stay published while their replacement is being walked. */
  roots: string[];
  found: string[];
  pending: PendingDirectory[];
  /** A structural bound, unlike a per-collection budget, leaves the child incomplete. */
  limited: boolean;
}
interface RootsCache {
  version: 3;
  ruleVersion: number;
  base: string;
  maxDepth: number;
  pass: number;
  children: ChildWalk[];
  roots: string[];
  truncated: boolean;
}

const CACHE_FILE = 'omp-session-roots-v1.json';
export const OMP_ROOTS_CACHE_MAX_BYTES = 16 * 1024 * 1024;
const MAX_CHILDREN = 2048;
const MAX_PENDING = 50_000;
const MAX_PENDING_BYTES = 4 * 1024 * 1024;
const MAX_PATH_BYTES = 4096;

function inside(child: string, parent: string): boolean {
  return child === parent || child.startsWith(parent + path.sep);
}

function validRelative(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    value.length > 0 &&
    Buffer.byteLength(value) <= MAX_PATH_BYTES &&
    !path.isAbsolute(value) &&
    path.normalize(value) === value &&
    !value.split(path.sep).some((part) => part === '..' || part === '.')
  );
}

function readCache(
  base: string,
  cacheDir: string | undefined,
  rules: DiscoveryRules,
  maxDepth: number
): RootsCache | null {
  if (!cacheDir) return null;
  try {
    const file = path.join(cacheDir, CACHE_FILE);
    if (fs.statSync(file).size > OMP_ROOTS_CACHE_MAX_BYTES) return null;
    const value = JSON.parse(fs.readFileSync(file, 'utf8')) as RootsCache;
    if (
      value.version !== 3 ||
      value.ruleVersion !== rules.version ||
      value.base !== base ||
      value.maxDepth !== maxDepth ||
      !Number.isSafeInteger(value.pass) ||
      value.pass < 0 ||
      !Array.isArray(value.children) ||
      value.children.length > MAX_CHILDREN
    )
      return null;
    let pendingCount = 0;
    let pendingBytes = 0;
    const names = new Set<string>();
    const roots = new Set<string>();
    for (const child of value.children) {
      if (
        !validRelative(child.name) ||
        path.basename(child.name) !== child.name ||
        names.has(child.name) ||
        typeof child.stamp !== 'string' ||
        child.stamp.length > 160 ||
        !Number.isFinite(child.completedAt) ||
        !Number.isSafeInteger(child.lastPass) ||
        typeof child.limited !== 'boolean' ||
        !Array.isArray(child.roots) ||
        !Array.isArray(child.found) ||
        !Array.isArray(child.pending)
      )
        return null;
      names.add(child.name);
      for (const root of [...child.roots, ...child.found]) {
        if (
          typeof root !== 'string' ||
          !path.isAbsolute(root) ||
          path.resolve(root) !== root ||
          Buffer.byteLength(root) > MAX_PATH_BYTES ||
          !inside(root, path.join(base, child.name))
        )
          return null;
        roots.add(root);
      }
      for (const entry of child.pending) {
        if (
          !Array.isArray(entry) ||
          entry.length !== 2 ||
          !validRelative(entry[0]) ||
          !inside(entry[0], child.name) ||
          !Number.isInteger(entry[1]) ||
          entry[1] < 1 ||
          entry[1] > maxDepth + 1
        )
          return null;
        pendingCount++;
        pendingBytes += Buffer.byteLength(JSON.stringify(entry));
      }
    }
    if (
      roots.size > rules.maxRoots ||
      pendingCount > MAX_PENDING ||
      pendingBytes > MAX_PENDING_BYTES
    )
      return null;
    return value;
  } catch {
    return null;
  }
}

function writeCache(cacheDir: string | undefined, value: RootsCache): boolean {
  if (!cacheDir) return true;
  try {
    const text = JSON.stringify(value);
    if (Buffer.byteLength(text) > OMP_ROOTS_CACHE_MAX_BYTES) return false;
    fs.mkdirSync(cacheDir, { recursive: true, mode: 0o700 });
    const file = path.join(cacheDir, CACHE_FILE);
    const temporary = `${file}.${process.pid}.tmp`;
    fs.writeFileSync(temporary, text, { mode: 0o600 });
    fs.renameSync(temporary, file);
    fs.chmodSync(file, 0o600);
    return true;
  } catch {
    // Without a durable cursor the next collection cannot claim convergence.
    return false;
  }
}

/**
 * Each top-level experiment owns a DFS cursor and its last completed roots. A collection
 * spends one shared directory/entry/time budget, saving unfinished cursors without a retry
 * TTL. New children run first; older cursors rotate by their last pass, so one large child
 * cannot starve the others. Completed children are revisited only on a change or the TTL.
 */
export async function discoverOmpSessionRoots(
  base: string,
  options: { cacheDir?: string; now: number; bounds?: OmpDiscoveryBounds },
  rules: DiscoveryRules
): Promise<{ roots: string[]; truncated: boolean }> {
  const maxDepth = Math.max(
    0,
    Math.min(options.bounds?.maxDepth ?? rules.maxDepth, rules.maxDepth)
  );
  const maxDirs = Math.max(1, options.bounds?.maxDirs ?? 100_000);
  const maxEntries = Math.max(1, options.bounds?.maxEntries ?? 2_000_000);
  const deadline = Date.now() + Math.max(1, options.bounds?.deadlineMs ?? 30_000);
  const cache: RootsCache = readCache(base, options.cacheDir, rules, maxDepth) ?? {
    version: 3,
    ruleVersion: rules.version,
    base,
    maxDepth,
    pass: 0,
    children: [],
    roots: [],
    truncated: false,
  };
  cache.pass++;
  let entries: fs.Dirent[];
  try {
    if (!(await fs.promises.lstat(base)).isDirectory()) return { roots: [], truncated: false };
    entries = await fs.promises.readdir(base, { withFileTypes: true });
  } catch (error) {
    return { roots: [], truncated: (error as NodeJS.ErrnoException).code !== 'ENOENT' };
  }
  if (await rules.isSandbox(base, entries)) return { roots: [], truncated: false };
  const names = entries
    .filter((entry) => entry.isDirectory() && !rules.skipDirs.has(entry.name))
    .map((entry) => entry.name)
    .sort();
  let limited = names.length > MAX_CHILDREN;
  const previous = new Map(cache.children.map((child) => [child.name, child]));
  const fresh = new Set<string>();
  const children: ChildWalk[] = [];
  for (const name of names.slice(0, MAX_CHILDREN)) {
    try {
      const stats = await fs.promises.lstat(path.join(base, name));
      if (!stats.isDirectory()) continue;
      const stamp = `${stats.dev}:${stats.ino}:${stats.mtimeMs}`;
      let child = previous.get(name);
      if (!child) {
        fresh.add(name);
        child = {
          name,
          stamp,
          completedAt: 0,
          lastPass: -1,
          roots: [],
          found: [],
          pending: [[name, 1]],
          limited: false,
        };
      } else if (
        !child.pending.length &&
        (child.stamp !== stamp || options.now - child.completedAt > rules.ttlMs)
      ) {
        child = { ...child, stamp, found: [], pending: [[name, 1]], limited: false };
      }
      children.push(child);
    } catch (error) {
      limited = true;
      if (!['ENOENT', 'ENOTDIR'].includes((error as NodeJS.ErrnoException).code ?? '')) {
        const child = previous.get(name);
        if (child) children.push(child);
      }
    }
  }
  cache.children = children;
  // Adding children or restarting expired children must not push an existing cursor cache
  // past its aggregate bounds (otherwise the following read would discard all progress).
  let pendingCount = 0;
  let pendingBytes = 0;
  for (const child of children) {
    child.pending = child.pending.filter((entry) => {
      const bytes = Buffer.byteLength(JSON.stringify(entry));
      if (pendingCount >= MAX_PENDING || pendingBytes + bytes > MAX_PENDING_BYTES) {
        child.limited = true;
        return false;
      }
      pendingCount++;
      pendingBytes += bytes;
      return true;
    });
  }
  // Revalidate published roots and resumed ancestors against today's sandbox/skip/link rules.
  // Only verdicts are memoized: keeping directory listings for 100k visits would be unbounded.
  const allowed = new Map<string, boolean | null>([['.', true]]);
  const directoryAllowed = async (relative: string): Promise<boolean | null> => {
    const known = allowed.get(relative);
    if (known !== undefined) return known;
    if (
      !validRelative(relative) ||
      relative.split(path.sep).some((part) => rules.skipDirs.has(part))
    )
      return false;
    const parent = await directoryAllowed(path.dirname(relative));
    if (parent !== true) return parent;
    let valid = false;
    try {
      const directory = path.join(base, relative);
      if ((await fs.promises.lstat(directory)).isDirectory()) {
        const listed = await fs.promises.readdir(directory, { withFileTypes: true });
        valid = !(await rules.isSandbox(directory, listed));
      }
    } catch (error) {
      if (!['ENOENT', 'ENOTDIR'].includes((error as NodeJS.ErrnoException).code ?? '')) {
        allowed.set(relative, null);
        limited = true;
        return null;
      }
    }
    allowed.set(relative, valid);
    return valid;
  };
  const present = new Map<string, boolean | null>();
  const rootPresent = async (root: string): Promise<boolean | null> => {
    const known = present.get(root);
    if (known !== undefined) return known;
    let result = await directoryAllowed(path.relative(base, root));
    if (result === true) {
      try {
        result =
          path.basename(root) === 'sessions'
            ? await rules.hasSessionMarker(root)
            : (await fs.promises.readdir(root, { withFileTypes: true })).some(
                (entry) => entry.isFile() && rules.isSessionFilename(entry.name)
              );
      } catch (error) {
        result = ['ENOENT', 'ENOTDIR'].includes((error as NodeJS.ErrnoException).code ?? '')
          ? false
          : null;
      }
    }
    if (result === null) limited = true;
    present.set(root, result);
    return result;
  };
  for (const child of children) {
    for (const key of ['roots', 'found'] as const) {
      const valid: string[] = [];
      for (const root of child[key]) if ((await rootPresent(root)) !== false) valid.push(root);
      child[key] = valid;
    }
  }
  const knownRoots = new Set(children.flatMap((child) => [...child.roots, ...child.found]));
  const pop = (child: ChildWalk): void => {
    const entry = child.pending.pop();
    if (!entry) return;
    pendingCount--;
    pendingBytes -= Buffer.byteLength(JSON.stringify(entry));
  };
  let visited = 0;
  let examined = 0;
  const ordered = children
    .filter((child) => child.pending.length)
    .sort(
      (left, right) =>
        Number(fresh.has(right.name)) - Number(fresh.has(left.name)) ||
        left.lastPass - right.lastPass ||
        left.name.localeCompare(right.name)
    );
  for (const child of ordered) {
    if (visited >= maxDirs || examined >= maxEntries || Date.now() >= deadline) break;
    child.lastPass = cache.pass;
    while (
      child.pending.length &&
      visited < maxDirs &&
      examined < maxEntries &&
      Date.now() < deadline
    ) {
      const current = child.pending[child.pending.length - 1];
      const [relative, depth] = current;
      if (depth > maxDepth && !path.basename(relative).includes('sessions')) {
        child.limited = true;
        pop(child);
        continue;
      }
      const parentAllowed = await directoryAllowed(path.dirname(relative));
      if (parentAllowed !== true) {
        if (parentAllowed === null) child.limited = true;
        pop(child);
        continue;
      }
      const directory = path.join(base, relative);
      let listed: fs.Dirent[];
      try {
        if (!(await fs.promises.lstat(directory)).isDirectory()) {
          pop(child);
          continue;
        }
        listed = await fs.promises.readdir(directory, { withFileTypes: true });
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'ENOENT') child.limited = true;
        pop(child);
        continue;
      }
      // Defer a directory that fits a fresh slice; an individually oversized listing is an
      // explicit structural limit, never an endlessly retried cursor with no progress.
      if (examined && examined + listed.length > maxEntries) break;
      pop(child);
      visited++;
      examined += listed.length;
      if (listed.length > maxEntries) {
        child.limited = true;
        continue;
      }
      const sandbox = await rules.isSandbox(directory, listed);
      allowed.set(relative, !sandbox);
      if (sandbox) continue;
      const sessions = path.basename(directory) === 'sessions';
      const marker = sessions ? await rules.hasSessionMarker(directory) : false;
      if (marker === null) child.limited = true;
      if (
        marker === true ||
        (!sessions && listed.some((entry) => entry.isFile() && rules.isSessionFilename(entry.name)))
      ) {
        if (
          !knownRoots.has(directory) &&
          (knownRoots.size >= rules.maxRoots || Buffer.byteLength(directory) > MAX_PATH_BYTES)
        )
          child.limited = true;
        else {
          knownRoots.add(directory);
          if (!child.found.includes(directory)) child.found.push(directory);
        }
        continue;
      }
      if (sessions) continue;
      const next = listed
        .filter((entry) => entry.isDirectory() && !rules.skipDirs.has(entry.name))
        .map((entry) => entry.name)
        .sort()
        .reverse();
      if (depth >= maxDepth && next.length) child.limited = true;
      for (const name of next) {
        // Preserve the former one-level-past-depth exception for session containers.
        if (depth > maxDepth || (depth === maxDepth && !name.includes('sessions'))) continue;
        const entry: PendingDirectory = [path.join(relative, name), depth + 1];
        const bytes = Buffer.byteLength(JSON.stringify(entry));
        if (
          pendingCount >= MAX_PENDING ||
          pendingBytes + bytes > MAX_PENDING_BYTES ||
          Buffer.byteLength(entry[0]) > MAX_PATH_BYTES
        ) {
          child.limited = true;
          continue;
        }
        child.pending.push(entry);
        pendingCount++;
        pendingBytes += bytes;
      }
    }
    if (!child.pending.length) {
      child.roots = child.limited ? [...new Set([...child.roots, ...child.found])] : child.found;
      child.found = [];
      child.completedAt = options.now;
    }
  }
  cache.roots = [...new Set(children.flatMap((child) => [...child.roots, ...child.found]))].sort();
  cache.truncated = limited || children.some((child) => child.pending.length > 0 || child.limited);
  if (!writeCache(options.cacheDir, cache)) cache.truncated = true;
  return { roots: cache.roots, truncated: cache.truncated };
}
