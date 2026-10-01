import * as fs from 'fs';
import * as path from 'path';
import * as readline from 'readline';
import { getCcsDir } from '../../utils/config-manager';
import type { RawUsageEntry } from '../jsonl-parser';
import { resolveCodexConfigPaths } from '../services/codex-dashboard-service';

interface CodexNativeUsageCollectorOptions {
  env?: NodeJS.ProcessEnv;
  homeDir?: string;
  includeCliproxySessions?: boolean;
  cacheDir?: string;
  disableCache?: boolean;
}

// Invalidate cached entries whose input/cache and output/reasoning categories
// overlapped. The persisted structure stays compatible; token semantics do not.
const CODEX_NATIVE_USAGE_CACHE_VERSION = 2;
const SECURE_CACHE_DIR_MODE = 0o700;
const SECURE_CACHE_FILE_MODE = 0o600;

interface CachedRolloutFile {
  path: string;
  size: number;
  mtimeMs: number;
  entries: RawUsageEntry[];
}

interface CodexNativeUsageCache {
  version: typeof CODEX_NATIVE_USAGE_CACHE_VERSION;
  includeCliproxySessions: boolean;
  generatedAt: number;
  files: Record<string, CachedRolloutFile>;
}

export interface CodexTokenSnapshot {
  /** Uncached input only; the cache categories are disjoint. */
  inputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  /** Total output already includes reasoning output. */
  outputTokens: number;
}

function isCliProxyBackedProvider(value: string | undefined): boolean {
  return value === 'cliproxy' || value === 'ccs_runtime';
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function asString(value: unknown): string | null {
  return typeof value === 'string' && value.trim().length > 0 ? value : null;
}

function asNumber(value: unknown): number {
  return typeof value === 'number' && Number.isFinite(value) && value > 0 ? value : 0;
}

function hasUsage(snapshot: CodexTokenSnapshot): boolean {
  return (
    snapshot.inputTokens > 0 ||
    snapshot.cacheCreationTokens > 0 ||
    snapshot.cacheReadTokens > 0 ||
    snapshot.outputTokens > 0
  );
}

function normalizeTokenSnapshot(value: unknown): CodexTokenSnapshot | null {
  if (!isObject(value)) return null;
  // Codex's TokenUsage copies the Responses usage fields. Cached reads and
  // cache writes are details of input_tokens; reasoning is a detail of output.
  // https://github.com/openai/codex/blob/main/codex-rs/codex-api/src/sse/responses.rs
  const cacheReadTokens = asNumber(value.cached_input_tokens);
  const cacheCreationTokens = asNumber(value.cache_write_input_tokens);
  return {
    inputTokens: Math.max(0, asNumber(value.input_tokens) - cacheReadTokens - cacheCreationTokens),
    cacheReadTokens,
    cacheCreationTokens,
    outputTokens: asNumber(value.output_tokens),
  };
}

function subtractSnapshots(
  current: CodexTokenSnapshot,
  previous: CodexTokenSnapshot
): CodexTokenSnapshot {
  return {
    inputTokens: Math.max(0, current.inputTokens - previous.inputTokens),
    cacheCreationTokens: Math.max(0, current.cacheCreationTokens - previous.cacheCreationTokens),
    cacheReadTokens: Math.max(0, current.cacheReadTokens - previous.cacheReadTokens),
    outputTokens: Math.max(0, current.outputTokens - previous.outputTokens),
  };
}

function snapshotsEqual(left: CodexTokenSnapshot, right: CodexTokenSnapshot): boolean {
  return (
    left.inputTokens === right.inputTokens &&
    left.cacheCreationTokens === right.cacheCreationTokens &&
    left.cacheReadTokens === right.cacheReadTokens &&
    left.outputTokens === right.outputTokens
  );
}

async function collectRolloutFiles(dir: string): Promise<string[]> {
  if (!fs.existsSync(dir)) return [];

  const files: string[] = [];
  for (const entry of await fs.promises.readdir(dir, { withFileTypes: true })) {
    const entryPath = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await collectRolloutFiles(entryPath)));
      continue;
    }
    if (entry.isFile() && entry.name.startsWith('rollout-') && entry.name.endsWith('.jsonl')) {
      files.push(entryPath);
    }
  }

  return files.sort();
}

function getDefaultCacheDir(): string {
  return path.join(getCcsDir(), 'cache');
}

function getCacheFilePath(cacheDir: string, includeCliproxySessions: boolean): string {
  return path.join(
    cacheDir,
    includeCliproxySessions
      ? 'codex-native-usage-with-cliproxy-v1.json'
      : 'codex-native-usage-v1.json'
  );
}

function isCachedRolloutFile(value: unknown): value is CachedRolloutFile {
  if (!isObject(value)) return false;
  return (
    typeof value.path === 'string' &&
    typeof value.size === 'number' &&
    typeof value.mtimeMs === 'number' &&
    Array.isArray(value.entries)
  );
}

function isCodexNativeUsageCache(value: unknown): value is CodexNativeUsageCache {
  if (!isObject(value)) return false;
  if (value.version !== CODEX_NATIVE_USAGE_CACHE_VERSION) return false;
  if (typeof value.includeCliproxySessions !== 'boolean') return false;
  if (typeof value.generatedAt !== 'number') return false;
  if (!isObject(value.files)) return false;
  return Object.values(value.files).every(isCachedRolloutFile);
}

function chmodBestEffort(targetPath: string, mode: number): void {
  try {
    fs.chmodSync(targetPath, mode);
  } catch {
    // Cache reads and writes remain best-effort on filesystems that do not support chmod.
  }
}

function readUsageCache(
  cacheDir: string,
  includeCliproxySessions: boolean
): CodexNativeUsageCache | null {
  try {
    const cachePath = getCacheFilePath(cacheDir, includeCliproxySessions);
    if (!fs.existsSync(cachePath)) return null;

    const fd = fs.openSync(cachePath, 'r');
    const header = Buffer.alloc(1024);
    let length: number;
    try {
      length = fs.readSync(fd, header, 0, header.length, 0);
    } finally {
      fs.closeSync(fd);
    }
    const version = header
      .subarray(0, length)
      .toString('utf8')
      .match(/"version"\s*:\s*(\d+)/);
    if (!version || Number(version[1]) !== CODEX_NATIVE_USAGE_CACHE_VERSION) return null;
    const parsed = JSON.parse(fs.readFileSync(cachePath, 'utf8')) as unknown;
    if (!isCodexNativeUsageCache(parsed)) return null;
    if (parsed.includeCliproxySessions !== includeCliproxySessions) return null;

    return parsed;
  } catch {
    return null;
  }
}

function writeUsageCache(cacheDir: string, cache: CodexNativeUsageCache): void {
  try {
    fs.mkdirSync(cacheDir, { recursive: true, mode: SECURE_CACHE_DIR_MODE });
    chmodBestEffort(cacheDir, SECURE_CACHE_DIR_MODE);
    const cachePath = getCacheFilePath(cacheDir, cache.includeCliproxySessions);
    const tempPath = `${cachePath}.${process.pid}.tmp`;
    fs.writeFileSync(tempPath, JSON.stringify(cache), {
      encoding: 'utf8',
      mode: SECURE_CACHE_FILE_MODE,
    });
    chmodBestEffort(tempPath, SECURE_CACHE_FILE_MODE);
    fs.renameSync(tempPath, cachePath);
    chmodBestEffort(cachePath, SECURE_CACHE_FILE_MODE);
  } catch {
    // Best-effort only.
  }
}

function getMtimeMsFingerprint(stats: fs.Stats): number {
  return stats.mtimeMs;
}

function hasMatchingFingerprint(
  cached: CachedRolloutFile | undefined,
  filePath: string,
  stats: fs.Stats
): cached is CachedRolloutFile {
  return (
    !!cached &&
    cached.path === filePath &&
    cached.size === stats.size &&
    cached.mtimeMs === getMtimeMsFingerprint(stats)
  );
}

export interface CodexNativeParserState {
  sessionId: string;
  projectPath: string;
  version?: string;
  modelProvider?: string;
  model: string;
  previousTotal: CodexTokenSnapshot | null;
}

export function createCodexNativeParserState(): CodexNativeParserState {
  return { sessionId: '', projectPath: '', model: 'unknown-codex-model', previousTotal: null };
}

/** Stateful parser shared by full-history and resumable Analytics scans. */
export function parseCodexNativeUsageLine(
  line: string,
  state: CodexNativeParserState,
  includeCliproxySessions = false
): RawUsageEntry | null {
  // Native rollouts contain large prompts/tool outputs. They carry no usage.
  if (!/"type"\s*:\s*"(?:session_meta|turn_context|event_msg)"/.test(line)) return null;
  if (!line.includes('"token_count"') && !/"type"\s*:\s*"(?:session_meta|turn_context)"/.test(line))
    return null;
  let parsed: Record<string, unknown>;
  try {
    parsed = JSON.parse(line);
  } catch {
    return null;
  }
  if (parsed.type === 'session_meta' && isObject(parsed.payload)) {
    state.sessionId = asString(parsed.payload.id) ?? state.sessionId;
    state.projectPath = asString(parsed.payload.cwd) ?? state.projectPath;
    state.version = asString(parsed.payload.cli_version) ?? state.version;
    state.modelProvider = asString(parsed.payload.model_provider) ?? state.modelProvider;
    return null;
  }

  if (parsed.type === 'turn_context' && isObject(parsed.payload)) {
    state.model = asString(parsed.payload.model) ?? state.model;
    state.projectPath = asString(parsed.payload.cwd) ?? state.projectPath;
    return null;
  }

  if (
    parsed.type !== 'event_msg' ||
    !isObject(parsed.payload) ||
    parsed.payload.type !== 'token_count' ||
    !state.sessionId ||
    (isCliProxyBackedProvider(state.modelProvider) && !includeCliproxySessions)
  ) {
    return null;
  }

  const totalSnapshot = normalizeTokenSnapshot(
    parsed.payload.info && isObject(parsed.payload.info)
      ? (parsed.payload.info as Record<string, unknown>).total_token_usage
      : null
  );
  const lastSnapshot = normalizeTokenSnapshot(
    parsed.payload.info && isObject(parsed.payload.info)
      ? (parsed.payload.info as Record<string, unknown>).last_token_usage
      : null
  );

  if (!totalSnapshot) return null;
  if (state.previousTotal && snapshotsEqual(totalSnapshot, state.previousTotal)) return null;

  const delta =
    state.previousTotal === null
      ? lastSnapshot && hasUsage(lastSnapshot)
        ? lastSnapshot
        : totalSnapshot
      : subtractSnapshots(totalSnapshot, state.previousTotal);
  state.previousTotal = totalSnapshot;

  if (!hasUsage(delta)) return null;

  const timestamp = asString(parsed.timestamp);
  if (!timestamp) return null;

  return {
    inputTokens: delta.inputTokens,
    outputTokens: delta.outputTokens,
    cacheCreationTokens: delta.cacheCreationTokens,
    cacheReadTokens: delta.cacheReadTokens,
    model: state.model,
    sessionId: state.sessionId,
    timestamp,
    projectPath: state.projectPath || '/',
    version: state.version,
    target: 'codex',
  };
}

async function parseRolloutFile(
  filePath: string,
  includeCliproxySessions: boolean
): Promise<RawUsageEntry[]> {
  const entries: RawUsageEntry[] = [];
  const state = createCodexNativeParserState();
  const stream = fs.createReadStream(filePath, { encoding: 'utf8' });
  const reader = readline.createInterface({ input: stream, crlfDelay: Infinity });
  try {
    for await (const line of reader) {
      const entry = parseCodexNativeUsageLine(line, state, includeCliproxySessions);
      if (entry) entries.push(entry);
    }
  } finally {
    reader.close();
    stream.destroy();
  }
  return entries;
}

export async function scanCodexNativeUsageEntries(
  options: CodexNativeUsageCollectorOptions = {}
): Promise<RawUsageEntry[]> {
  const includeCliproxySessions = options.includeCliproxySessions === true;
  const { baseDir } = resolveCodexConfigPaths({
    env: options.env,
    homeDir: options.homeDir,
  });
  const rolloutFiles = await collectRolloutFiles(path.join(baseDir, 'sessions'));
  const entries: RawUsageEntry[] = [];

  if (options.disableCache === true) {
    for (const filePath of rolloutFiles) {
      entries.push(...(await parseRolloutFile(filePath, includeCliproxySessions)));
    }
    return entries;
  }

  const cacheDir = options.cacheDir ?? getDefaultCacheDir();
  const previousCache = readUsageCache(cacheDir, includeCliproxySessions);
  const nextCache: CodexNativeUsageCache = {
    version: CODEX_NATIVE_USAGE_CACHE_VERSION,
    includeCliproxySessions,
    generatedAt: Date.now(),
    files: {},
  };

  for (const filePath of rolloutFiles) {
    const stats = await fs.promises.stat(filePath);
    const cached = previousCache?.files[filePath];

    if (hasMatchingFingerprint(cached, filePath, stats)) {
      entries.push(...cached.entries);
      nextCache.files[filePath] = cached;
      continue;
    }

    const fileEntries = await parseRolloutFile(filePath, includeCliproxySessions);
    entries.push(...fileEntries);
    nextCache.files[filePath] = {
      path: filePath,
      size: stats.size,
      mtimeMs: getMtimeMsFingerprint(stats),
      entries: fileEntries,
    };
  }

  writeUsageCache(cacheDir, nextCache);
  return entries;
}
