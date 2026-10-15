/** OMP session usage: line parser and session-root enumeration for Analytics. */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { RawUsageEntry } from '../jsonl-parser';
import { discoverOmpSessionRoots, type OmpDiscoveryBounds } from './omp-session-root-discovery';

export const OMP_TARGET = 'omp';
/** Bounded marker scan under ~/PM-Experiments for custom --session-dir roots. */
export const OMP_SCAN_MAX_DEPTH = 64;
/** Bump when discovery/skip rules change, independently of the cache file schema. */
export const OMP_ROOTS_RULE_VERSION = 4;
export const OMP_SCAN_MAX_DIRS = 100_000;
export const OMP_SCAN_MAX_ROOTS = 512;
export const OMP_SCAN_CACHE_TTL_MS = 24 * 3_600_000;
/**
 * Sandbox marker: synthetic-log generators (gen-data.mjs, sandbox.sh) write
 * this file at their data-tree root, and the session-root scan skips any
 * subtree whose directory holds it, so measurement fixtures never count as
 * real usage. Explicit roots (env vars, profiles, extra sources) are exempt:
 * configuring a path explicitly means it should count.
 */
export const AAC_SANDBOX_MARKER = '.aac-synthetic';
/**
 * Directory names the root scans never descend into (fixtures, builds); the experiment-root walk
 * (experiment-usage-roots.ts) shares the list.
 */
export const SCAN_SKIP_DIRS: ReadonlySet<string> = new Set([
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
  discovery?: { truncated: boolean };
}

export type OmpScanBounds = OmpDiscoveryBounds;

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
 * Default, profile and explicitly configured OMP roots, plus custom roots discovered
 * under ~/PM-Experiments. Each collection advances saved per-experiment DFS cursors
 * within one shared budget; completed experiments are reused for 24 hours or until
 * their directory identity/mtime changes. New experiments run first, and unfinished
 * experiments rotate across collections. Only an unfinished or structurally limited
 * cycle reports discovery truncated; a large, fully visited tree reports complete.
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
  const discovered = await discoverOmpSessionRoots(
    path.join(homeDir, 'PM-Experiments'),
    {
      cacheDir: options.cacheDir,
      now,
      bounds: { maxDirs: OMP_SCAN_MAX_DIRS, ...options.scanBounds },
    },
    {
      version: OMP_ROOTS_RULE_VERSION,
      maxDepth: OMP_SCAN_MAX_DEPTH,
      maxRoots: OMP_SCAN_MAX_ROOTS,
      ttlMs: OMP_SCAN_CACHE_TTL_MS,
      skipDirs: SCAN_SKIP_DIRS,
      isSessionFilename: isOmpSessionFilename,
      isSandbox: isSandboxTree,
      hasSessionMarker: sessionsDirHasMarker,
    }
  );
  for (const found of discovered.roots) add(found);
  if (options.discovery)
    options.discovery.truncated = discovered.truncated || roots.length > OMP_SCAN_MAX_ROOTS;
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
