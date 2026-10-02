/** OMP session usage: line parser and session-root enumeration for Analytics. */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { RawUsageEntry } from '../jsonl-parser';

export const OMP_TARGET = 'omp';
/** Bounded marker scan under ~/PM-Experiments for custom --session-dir roots. */
export const OMP_SCAN_MAX_DEPTH = 6;
export const OMP_SCAN_MAX_DIRS = 5000;
export const OMP_SCAN_MAX_ROOTS = 128;
export const OMP_SCAN_CACHE_TTL_MS = 6 * 3_600_000;
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
 * A `sessions/` dir is an OMP root candidate when it holds a `*.jsonl` file
 * within two levels. Custom `--session-dir` layouts name files freely (the
 * observed corpus mixes `<ts>_<uuid>.jsonl`, `rollout-*.jsonl` and
 * `session.jsonl`), so the marker is presence, not naming; the OMP line
 * parser rejects every non-OMP record inside.
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
        if (entry.name.endsWith('.jsonl')) return true;
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
async function scanSessionRoots(base: string, bounds: OmpScanBounds = {}): Promise<string[]> {
  const maxDepth = bounds.maxDepth ?? OMP_SCAN_MAX_DEPTH;
  const maxDirs = bounds.maxDirs ?? OMP_SCAN_MAX_DIRS;
  const maxEntries = bounds.maxEntries ?? 2_000_000;
  const deadline = Date.now() + Math.max(1, bounds.deadlineMs ?? 15_000);
  const found: string[] = [];
  try {
    if (!(await fs.promises.stat(base)).isDirectory()) return found;
  } catch {
    return found;
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
    if (path.basename(current.directory) === 'sessions' && current.directory !== base) {
      if (await sessionsDirHasMarker(current.directory)) found.push(current.directory);
      continue;
    }
    if (current.depth >= maxDepth) continue;
    for (const entry of entries) {
      if (entry.name === 'node_modules' || entry.name === '.git') continue;
      if (entry.isDirectory())
        pending.push({
          directory: path.join(current.directory, entry.name),
          depth: current.depth + 1,
        });
    }
  }
  return found;
}

const ROOTS_CACHE_FILE = 'omp-session-roots-v1.json';
const ROOTS_CACHE_TTL_MS = 6 * 3_600_000;

interface OmpRootsCache {
  version: 1;
  scannedAt: number;
  roots: string[];
}

function readRootsCache(cacheDir: string, now: number): string[] | null {
  try {
    const raw = fs.readFileSync(path.join(cacheDir, ROOTS_CACHE_FILE), 'utf8');
    const value = JSON.parse(raw) as OmpRootsCache;
    if (
      value.version !== 1 ||
      !Number.isFinite(value.scannedAt) ||
      now - value.scannedAt > ROOTS_CACHE_TTL_MS ||
      !Array.isArray(value.roots) ||
      value.roots.length > OMP_SCAN_MAX_ROOTS ||
      value.roots.some((root) => typeof root !== 'string' || !path.isAbsolute(root))
    )
      return null;
    return [...value.roots];
  } catch {
    return null;
  }
}

function writeRootsCache(cacheDir: string, roots: string[], scannedAt: number): void {
  try {
    fs.mkdirSync(cacheDir, { recursive: true, mode: 0o700 });
    const file = path.join(cacheDir, ROOTS_CACHE_FILE);
    const temporary = `${file}.${process.pid}.tmp`;
    fs.writeFileSync(
      temporary,
      JSON.stringify({ version: 1, scannedAt, roots: roots.slice(0, OMP_SCAN_MAX_ROOTS) }),
      { mode: 0o600 }
    );
    fs.renameSync(temporary, file);
    fs.chmodSync(file, 0o600);
  } catch {
    /* The next collection scans again. */
  }
}

/**
 * OMP session roots: the default `~/.omp/agent/sessions`, `$PI_CODING_AGENT_DIR`
 * sessions when set, `$OMP_SESSION_DIRS` extras, and custom `--session-dir`
 * roots found by a bounded marker scan under `~/PM-Experiments`. The scan is
 * cached for six hours because the tree is large; explicit roots never wait.
 * The scan awaits each directory read, so it never blocks the event loop.
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
  const base = path.join(homeDir, 'PM-Experiments');
  if (options.cacheDir) {
    const cached = readRootsCache(options.cacheDir, now);
    if (cached) {
      for (const found of cached) add(found);
      return roots.slice(0, OMP_SCAN_MAX_ROOTS);
    }
  }
  // A complete walk of a large tree; bounded by depth, directory count and a
  // deadline, and cached afterwards. A bounded walk that finds nothing keeps
  // no stale roots: defaults and explicit roots still apply.
  const scanned = await scanSessionRoots(base, {
    maxDirs: 100_000,
    deadlineMs: 30_000,
    ...options.scanBounds,
  });
  if (options.cacheDir) writeRootsCache(options.cacheDir, scanned, now);
  for (const found of scanned) add(found);
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
