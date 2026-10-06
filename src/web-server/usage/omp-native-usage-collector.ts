/** OMP session usage: line parser and session-root enumeration for Analytics. */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { RawUsageEntry } from '../jsonl-parser';

export const OMP_TARGET = 'omp';
/** Bounded marker scan under ~/PM-Experiments for custom --session-dir roots. */
export const OMP_SCAN_MAX_DEPTH = 6;
export const OMP_SCAN_MAX_DIRS = 5000;
export const OMP_SCAN_MAX_ROOTS = 512;
export const OMP_SCAN_CACHE_TTL_MS = 6 * 3_600_000;
/**
 * Sandbox marker: synthetic-log generators (gen-data.mjs, sandbox.sh) write
 * this file at their data-tree root, and the session-root scan skips any
 * subtree whose directory holds it, so measurement fixtures never count as
 * real usage. Explicit roots (env vars, profiles, extra sources) are exempt:
 * configuring a path explicitly means it should count.
 */
export const AAC_SANDBOX_MARKER = '.aac-synthetic';
/** Directory names the root scan never descends into (fixtures, builds). */
const SCAN_SKIP_DIRS = new Set([
  'node_modules',
  '.git',
  'tests',
  'test',
  'fixtures',
  '__fixtures__',
  'target',
  'dist',
  'coverage',
  'test-results',
]);
const SESSION_TS = /^\d{4}-\d{2}-\d{2}T\d{2}[:-]\d{2}/;

function nonNegative(value: unknown): number | null {
  const numeric = typeof value === 'number' ? value : Number.NaN;
  if (typeof numeric !== 'number' || !Number.isFinite(numeric) || numeric < 0) return null;
  return numeric;
}

function cleanModel(value: unknown): string | null {
  if (typeof value !== 'string') return null;
  const text = value.trim();
  if (!text || text.length > 160 || /[\u0000-\u001f\u007f]/.test(text)) return null;
  return text;
}

/** A routing provider id, or '' when the record names none or an unusable one. */
function cleanProvider(value: unknown): string {
  if (typeof value !== 'string') return '';
  const text = value.trim();
  if (!text || text.length > 64 || /[\u0000-\u001f\u007f]/.test(text)) return '';
  return text;
}

function epochMs(timestamp: unknown): number | null {
  if (typeof timestamp === 'string') {
    const epoch = Date.parse(timestamp);
    return Number.isFinite(epoch) ? epoch : null;
  }
  if (typeof timestamp === 'number' && Number.isFinite(timestamp) && timestamp > 0) {
    let seconds = Math.floor(timestamp);
    while (seconds > 4102444800) seconds = Math.floor(seconds / 1000);
    return seconds * 1000;
  }
  return null;
}

/**
 * Parse one OMP session line. Only assistant `type=message` records carry
 * usage; cumulative goal counters and context snapshots are never read (only
 * `message.usage.*` is). Returns null for every other line.
 */
export function parseOmpUsageLine(line: string, sessionId: string): RawUsageEntry | null {
  // The collector pre-filters on `"type":"message"` + `"usage"` before this
  // runs, so conversation content is never parsed.
  let record: Record<string, unknown>;
  try {
    record = JSON.parse(line) as Record<string, unknown>;
  } catch {
    return null;
  }
  if (record.type !== 'message') return null;
  const message = record.message as Record<string, unknown> | undefined;
  if (!message || message.role !== 'assistant') return null;
  const model = cleanModel(message.model);
  if (!model) return null;
  const usage = message.usage as Record<string, unknown> | undefined;
  if (!usage) return null;
  const input = nonNegative(usage.input);
  const output = nonNegative(usage.output);
  const cacheRead = nonNegative(usage.cacheRead);
  const cacheWrite = nonNegative(usage.cacheWrite);
  if (input === null || output === null || cacheRead === null || cacheWrite === null) return null;
  let costUsd: number | undefined;
  const cost = usage.cost as Record<string, unknown> | undefined;
  if (cost) {
    const total = nonNegative(cost.total);
    // A logged 0 is "not logged", never free: only nonzero costs are kept.
    if (total !== null && total > 0) costUsd = total;
  }
  // Zero-usage pings (e.g. union-alpha) carry no usage. totalTokens equals
  // input+cacheRead+output and is never added to the parts.
  if (input + output + cacheRead + cacheWrite === 0 && costUsd === undefined) return null;
  const epoch = epochMs(record.timestamp);
  if (epoch === null) return null;
  return {
    inputTokens: Math.floor(input),
    outputTokens: Math.floor(output),
    cacheCreationTokens: Math.floor(cacheWrite),
    cacheReadTokens: Math.floor(cacheRead),
    model,
    sessionId,
    timestamp: new Date(epoch).toISOString(),
    projectPath: '',
    target: OMP_TARGET,
    // Rates are looked up under the provider that served the call, as the
    // remote helper does, so a model is priced the same on every host.
    provider: cleanProvider(message.provider),
    ...(costUsd === undefined ? {} : { costUsd }),
  };
}

/** Session files: `<ts>_<uuid>[.jsonl]`, plus `__advisor.jsonl` subagent calls. */
export function isOmpSessionFilename(name: string): boolean {
  if (name === '__advisor.jsonl') return true;
  if (name.endsWith('.jsonl')) {
    const stem = name.slice(0, -'.jsonl'.length);
    return SESSION_TS.test(stem) || SESSION_TS.test(name);
  }
  return SESSION_TS.test(name) && !name.includes('.') && name.includes('_');
}

/** Session id from the enclosing file or directory name. */
export function ompSessionIdForFile(file: string): string {
  const base = path.basename(file);
  if (base === '__advisor.jsonl' || !isOmpSessionFilename(base)) {
    const parent = path.basename(path.dirname(file));
    if (parent && parent !== '.' && parent !== path.sep) return parent.slice(0, 160);
    return '';
  }
  return (base.endsWith('.jsonl') ? base.slice(0, -'.jsonl'.length) : base).slice(0, 160);
}

export interface OmpRootOptions {
  env?: NodeJS.ProcessEnv;
  homeDir?: string;
  /** Cache directory for the marker-scan roots; without it every call scans. */
  cacheDir?: string;
  now?: () => number;
  scanBounds?: OmpScanBounds;
}

export interface OmpScanBounds {
  maxDepth?: number;
  maxDirs?: number;
  /** Total directory entries examined across the walk. */
  maxEntries?: number;
  deadlineMs?: number;
}

/**
 * True when the scanned directory is a synthetic sandbox tree: it holds the
 * sandbox marker, a SANDBOX.md, or a generator MANIFEST.json with a `trees`
 * key (the T5-recipe sandboxes predate the marker). The MANIFEST read is one
 * bounded small file, only for dirs that hold one; anything unparseable or
 * oversized fails open (not a sandbox) so real logs are never hidden.
 */
async function isSandboxTree(directory: string, entries: fs.Dirent[]): Promise<boolean> {
  let hasManifest = false;
  for (const entry of entries) {
    if (!entry.isFile()) continue;
    if (entry.name === AAC_SANDBOX_MARKER || entry.name === 'SANDBOX.md') return true;
    if (entry.name === 'MANIFEST.json') hasManifest = true;
  }
  if (!hasManifest) return false;
  try {
    const text = await fs.promises.readFile(path.join(directory, 'MANIFEST.json'), 'utf8');
    if (text.length > 64 * 1024) return false;
    const parsed: unknown = JSON.parse(text);
    return (
      !!parsed &&
      typeof parsed === 'object' &&
      !Array.isArray(parsed) &&
      typeof (parsed as Record<string, unknown>).trees === 'object' &&
      (parsed as Record<string, unknown>).trees !== null
    );
  } catch {
    return false;
  }
}

/**
 * A `sessions/` dir is an OMP root candidate when it holds an OMP-named file
 * within two levels, by the same filename rule the collector uses to read
 * it. Presence alone (any `.jsonl`) accepted synthetic Muse trees
 * (`<uuid>/session.jsonl`); the OMP line parser still rejects every non-OMP
 * record inside an accepted root.
 */
async function sessionsDirHasMarker(directory: string): Promise<boolean> {
  const pending: Array<{ directory: string; depth: number }> = [{ directory, depth: 0 }];
  let checked = 0;
  while (pending.length) {
    const current = pending.pop();
    if (!current) break;
    let entries: fs.Dirent[];
    try {
      entries = await fs.promises.readdir(current.directory, { withFileTypes: true });
    } catch {
      return false;
    }
    for (const entry of entries.slice(0, 200)) {
      checked++;
      if (checked > 400) return false;
      if (entry.isFile()) {
        if (isOmpSessionFilename(entry.name)) return true;
      } else if (current.depth < 1 && entry.isDirectory()) {
        pending.push({
          directory: path.join(current.directory, entry.name),
          depth: current.depth + 1,
        });
      }
    }
  }
  return false;
}

/**
 * Breadth-first marker walk. Every directory read is awaited, so a large tree
 * never blocks the server's event loop; the walk is bounded by depth,
 * directory count, entries examined and a deadline.
 */
async function scanSessionRoots(
  base: string,
  bounds: OmpScanBounds = {}
): Promise<{ roots: string[]; truncated: boolean }> {
  const maxDepth = bounds.maxDepth ?? OMP_SCAN_MAX_DEPTH;
  const maxDirs = bounds.maxDirs ?? OMP_SCAN_MAX_DIRS;
  const maxEntries = bounds.maxEntries ?? 2_000_000;
  const deadline = Date.now() + Math.max(1, bounds.deadlineMs ?? 15_000);
  const found: string[] = [];
  try {
    if (!(await fs.promises.stat(base)).isDirectory()) return { roots: found, truncated: false };
  } catch {
    return { roots: found, truncated: false };
  }
  // Breadth-first so shallow roots are found even when the caps bite.
  const pending: Array<{ directory: string; depth: number }> = [{ directory: base, depth: 0 }];
  let head = 0;
  let visited = 0;
  let examined = 0;
  while (head < pending.length && found.length < OMP_SCAN_MAX_ROOTS) {
    if (visited >= maxDirs || Date.now() >= deadline) break;
    const current = pending[head++];
    if (current.depth > maxDepth) continue;
    visited++;
    let entries: fs.Dirent[];
    try {
      entries = await fs.promises.readdir(current.directory, { withFileTypes: true });
    } catch {
      continue;
    }
    examined += entries.length;
    if (examined > maxEntries) break;
    // Synthetic sandbox trees are never roots and never descended: their
    // fixture records would otherwise count as real usage.
    if (await isSandboxTree(current.directory, entries)) continue;
    if (current.directory !== base) {
      if (path.basename(current.directory) === 'sessions') {
        if (await sessionsDirHasMarker(current.directory)) found.push(current.directory);
        continue;
      }
      // Custom `--session-dir` roots are named freely (`<branch>-sessions`,
      // `rev-<id>-sessions`, `<task>-run-sessions`, ...), so a literal
      // `sessions` name check missed them: Jared's wave-4 workers wrote 25 such
      // dirs (75 files, 125 MB) that never became roots. Accept any non-base
      // directory that directly holds an OMP session file, by the same filename
      // rule the collector uses to read it. Accepted roots are not descended
      // here; the collector's own recursive scan then reads their nested
      // `<ts>_<uuid>/__advisor.jsonl` subagent files.
      if (entries.some((entry) => entry.isFile() && isOmpSessionFilename(entry.name))) {
        found.push(current.directory);
        continue;
      }
    }
    if (current.depth >= maxDepth) {
      // One level past the depth cap, still examine session-container
      // children: experiment runners nest per-job session dirs one level
      // deeper than the cap (`runs/<run>/jobs/<job>/sessions`), and the usage
      // inside is real. Only `*sessions*`-named children are examined, inline
      // and never descended, so the extra work stays bounded by that small
      // set. Anything deeper (or not session-named) stays out of reach; those
      // locations need explicit extra usage-log sources.
      if (current.depth === maxDepth) {
        for (const entry of entries) {
          if (Date.now() >= deadline || found.length >= OMP_SCAN_MAX_ROOTS) break;
          if (!entry.isDirectory() || !entry.name.includes('sessions')) continue;
          const child = path.join(current.directory, entry.name);
          let childEntries: fs.Dirent[];
          try {
            childEntries = await fs.promises.readdir(child, { withFileTypes: true });
          } catch {
            continue;
          }
          examined += childEntries.length + 1;
          if (examined > maxEntries) break;
          if (await isSandboxTree(child, childEntries)) continue;
          if (entry.name === 'sessions') {
            if (await sessionsDirHasMarker(child)) found.push(child);
          } else if (
            childEntries.some(
              (childEntry) => childEntry.isFile() && isOmpSessionFilename(childEntry.name)
            )
          ) {
            found.push(child);
          }
        }
        if (examined > maxEntries) break;
      }
      continue;
    }
    for (const entry of entries) {
      if (SCAN_SKIP_DIRS.has(entry.name)) continue;
      if (entry.isDirectory())
        pending.push({
          directory: path.join(current.directory, entry.name),
          depth: current.depth + 1,
        });
    }
  }
  // Truncated when the walk stopped with directories still pending or hit the
  // root cap: the tree is larger than one bounded scan, so the caller unions
  // this result with the cache and rescans soon to converge over collections.
  const truncated = head < pending.length || found.length >= OMP_SCAN_MAX_ROOTS;
  return { roots: found, truncated };
}

const ROOTS_CACHE_FILE = 'omp-session-roots-v1.json';
const ROOTS_CACHE_TTL_MS = 6 * 3_600_000;
/**
 * A truncated scan (the tree outgrew one bounded walk) rescans on this shorter
 * TTL and unions into the cache, so successive collections accumulate roots and
 * converge toward full coverage instead of freezing one partial scan for 6 h.
 */
const ROOTS_CACHE_TRUNCATED_TTL_MS = 30 * 60_000;

interface OmpRootsCache {
  version: 2;
  scannedAt: number;
  roots: string[];
  truncated: boolean;
}

function readRootsCacheRaw(cacheDir: string): OmpRootsCache | null {
  try {
    const raw = fs.readFileSync(path.join(cacheDir, ROOTS_CACHE_FILE), 'utf8');
    const value = JSON.parse(raw) as OmpRootsCache;
    if (
      value.version !== 2 ||
      !Number.isFinite(value.scannedAt) ||
      !Array.isArray(value.roots) ||
      value.roots.length > OMP_SCAN_MAX_ROOTS ||
      value.roots.some((root) => typeof root !== 'string' || !path.isAbsolute(root)) ||
      typeof value.truncated !== 'boolean'
    )
      return null;
    return value;
  } catch {
    return null;
  }
}

function writeRootsCache(
  cacheDir: string,
  roots: string[],
  truncated: boolean,
  scannedAt: number
): void {
  try {
    fs.mkdirSync(cacheDir, { recursive: true, mode: 0o700 });
    const file = path.join(cacheDir, ROOTS_CACHE_FILE);
    const temporary = `${file}.${process.pid}.tmp`;
    fs.writeFileSync(
      temporary,
      JSON.stringify({
        version: 2,
        scannedAt,
        truncated,
        roots: roots.slice(0, OMP_SCAN_MAX_ROOTS),
      }),
      { mode: 0o600 }
    );
    fs.renameSync(temporary, file);
    fs.chmodSync(file, 0o600);
  } catch {
    /* The next collection scans again. */
  }
}

/**
 * OMP session roots: the default `~/.omp/agent/sessions`, per-profile
 * `~/.omp/profiles/<name>/agent/sessions`, `$PI_CODING_AGENT_DIR` sessions when
 * set, `$OMP_SESSION_DIRS` extras, and custom `--session-dir` roots found by a
 * bounded marker scan under `~/PM-Experiments`. The tree outgrows one bounded
 * walk, so a completed scan is cached six hours while a truncated one is
 * rescanned on a short TTL and unioned into the cache: roots accumulate across
 * collections and never regress. Explicit roots never wait on the scan, which
 * awaits each directory read so it never blocks the event loop.
 */
export async function resolveOmpSessionRoots(options: OmpRootOptions = {}): Promise<string[]> {
  const env = options.env ?? process.env;
  const homeDir = options.homeDir ?? os.homedir();
  const now = (options.now ?? Date.now)();
  const roots: string[] = [];
  const seen = new Set<string>();
  const add = (candidate: string): void => {
    const absolute = path.resolve(candidate);
    if (!seen.has(absolute)) {
      seen.add(absolute);
      roots.push(absolute);
    }
  };
  add(path.join(homeDir, '.omp', 'agent', 'sessions'));
  const agentDir = env.PI_CODING_AGENT_DIR;
  if (agentDir && agentDir.trim()) add(path.join(agentDir, 'sessions'));
  const extra = env.OMP_SESSION_DIRS;
  if (extra) {
    for (const piece of extra.split(path.delimiter)) {
      if (piece.trim()) add(piece.trim());
    }
  }
  // Per-profile OMP instances (Windows keeps `~/.omp/profiles/<name>`) store
  // their own sessions under `<profile>/agent/sessions`; enumerate the bounded
  // profile dirs so their usage is discovered too, not just the default agent.
  // `localRequests` filters these to real dirs, so a profile without a sessions
  // dir is harmless. A cheap readdir, so it runs even on a marker-scan cache hit.
  try {
    const profilesRoot = path.join(homeDir, '.omp', 'profiles');
    const profiles = await fs.promises.readdir(profilesRoot, { withFileTypes: true });
    for (const profile of profiles.slice(0, OMP_SCAN_MAX_ROOTS)) {
      if (profile.isDirectory()) add(path.join(profilesRoot, profile.name, 'agent', 'sessions'));
    }
  } catch {
    /* No profiles directory: only the default agent applies. */
  }
  const base = path.join(homeDir, 'PM-Experiments');
  const cached = options.cacheDir ? readRootsCacheRaw(options.cacheDir) : null;
  if (cached) {
    const ttl = cached.truncated ? ROOTS_CACHE_TRUNCATED_TTL_MS : ROOTS_CACHE_TTL_MS;
    if (now - cached.scannedAt <= ttl) {
      for (const found of cached.roots) add(found);
      return roots.slice(0, OMP_SCAN_MAX_ROOTS);
    }
  }
  // A bounded walk of a large tree usually cannot finish in one pass, so the
  // result is unioned with the cached roots (never regressing) and, while
  // truncated, rescanned on a short TTL: successive collections accumulate
  // roots and converge toward full coverage. Defaults/explicit roots always
  // apply, so a walk that finds nothing keeps no stale roots of its own.
  const { roots: scanned, truncated } = await scanSessionRoots(base, {
    maxDirs: 100_000,
    deadlineMs: 30_000,
    ...options.scanBounds,
  });
  const union = new Set<string>(scanned);
  for (const prior of cached?.roots ?? []) union.add(prior);
  const merged = [...union].slice(0, OMP_SCAN_MAX_ROOTS);
  if (options.cacheDir) writeRootsCache(options.cacheDir, merged, truncated, now);
  for (const found of merged) add(found);
  return roots.slice(0, OMP_SCAN_MAX_ROOTS);
}

/**
 * The part of an OMP session file's path that names its session: the nearest
 * `<ts>_<uuid>` component (the file itself or its session directory) and
 * everything below it. A resumed run copies a session into a new root under
 * the same name, so two files with the same key may hold the same records.
 * Null for files outside any session-named component.
 */
export function ompSessionCopyKey(file: string): string | null {
  const parts = path.resolve(file).split(path.sep);
  for (let index = parts.length - 1; index >= 0; index--) {
    const part = parts[index];
    if (part !== '__advisor.jsonl' && isOmpSessionFilename(part))
      return parts.slice(index).join('/');
  }
  return null;
}
