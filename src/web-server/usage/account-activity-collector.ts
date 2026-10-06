/** Bounded, resumable native history for the Accounts Analytics page. */
import * as fs from 'fs';
import * as path from 'path';
import { createHash } from 'crypto';
import { CCSError } from '../../errors/error-types';
import { parseUsageEntry, type RawUsageEntry } from '../jsonl-parser';
import {
  createCodexNativeParserState,
  parseCodexNativeUsageLine,
  type CodexNativeParserState,
} from './codex-native-usage-collector';
import {
  isOmpSessionFilename,
  ompSessionCopyKey,
  ompSessionIdForFile,
  parseOmpUsageLine,
} from './omp-native-usage-collector';
import { museSessionIdForFile, parseMuseUsageLine } from './muse-native-usage-collector';
import {
  queryLocalZcodeUsage,
  type ZcodeFingerprint,
  type ZcodeHelperSessionRow,
} from './zcode-native-usage-collector';
import { analyticsSessionKey } from './analytics-session-key';
import type { AccountAnalyticsActivityProvider } from '../services/account-analytics-types';
import { getModelPricingWithSource, type ModelPricingResolution } from '../model-pricing';
import { getModelsUsed, normalizeUsageProvider } from './model-identity';
import type { ModelBreakdown } from './types';
import {
  CodexPartitionError,
  spawnCodexPartitionReader,
  type CodexPartitionSpec,
  type JsonlFieldMapping,
  type UsageWorkerRequest,
  type UsageWorkerResult,
} from './worker-client';
import { availableCpus } from './collector-concurrency';

export interface CompactEntry {
  entry: RawUsageEntry;
  events: number;
}
interface Checkpoint {
  version: 2;
  size: number;
  mtimeMs: number;
  identity: string;
  head: string;
  tail: string;
  offset: number;
  minDate: number;
  complete: boolean;
  discardingLine: boolean;
  skippedLines: number;
  largeLineParserVersion?: 3;
  unfinishedTail?: boolean;
  /** Row-shape version for kinds whose parser changed (PARSER_VERSION). */
  parser?: number;
  state: CodexNativeParserState;
  rows: CompactEntry[];
  /**
   * Claude's current API response: consecutive same-response lines collapse
   * to the last (complete) usage, so the group-in-progress rides here until
   * the next response arrives or the file ends. Survives pass cuts; a corrupt
   * value invalidates the checkpoint like any other field.
   */
  pendingClaude?: { key: string; entry: RawUsageEntry };
}
export interface AccountActivityScanOptions {
  minDate: number;
  cacheDir: string;
  /** Testable inner budget, always less than the HTTP service worker deadline. */
  budgetMs?: number;
  maxBytesPerFile?: number;
  /** Smaller fixture/embedding budgets cannot raise the production ceilings. */
  traversalLimits?: { maxDepth?: number; maxDirectories?: number; maxEntries?: number };
  /**
   * Unread codex bytes that trigger a fanned-out scan (N14). Below it the
   * worker reads inline, exactly as before, so warm cycles never pay for a
   * fan-out they do not need. Defaults to CODEX_FANOUT_THRESHOLD_BYTES.
   */
  fanoutThresholdBytes?: number;
  /**
   * Internal: set only by a codex partition reader. The default per-file byte
   * cap (256 MB) exists so one giant file cannot starve the other files of a
   * single reader's pass; a partition reader owns its chunk alone, so it is
   * bounded by the shared scan deadline instead and the byte cap is lifted.
   * An explicitly set maxBytesPerFile is still honored (see
   * collectCodexPartition).
   */
  codexPartitionRead?: boolean;
}
const MAX_FILES = 20_000;
const MAX_DEPTH = 64;
const MAX_DIRECTORIES = 10_000;
const MAX_ENTRIES = 100_000;
const MAX_LINE_BYTES = 8 * 1024 * 1024;
const MAX_CACHE_BYTES = 8 * 1024 * 1024;
const MAX_FILE_ROWS = 10_000;
const MAX_TOTAL_ROWS = 100_000;
/**
 * Append-only usage logs never gain in-window records after their last write,
 * so a file whose mtime predates the window cannot hold in-window events. The
 * 24 h margin absorbs clock jitter between a record's own timestamp and the
 * write that flushed it, timezone-naive timestamps some tools write, and
 * mtimes from a NAS clock when a network share is added as an extra source.
 * Skipping such files is a pure optimization: addEntry already drops
 * out-of-window records, so totals are unchanged while cold starts read far
 * less (Codex ~24.7 GB -> ~11.8 GB), so Analytics converges sooner.
 */
const MTIME_SKIP_MARGIN_MS = 24 * 60 * 60 * 1000;

function hash(value: string | Buffer): string {
  return createHash('sha256').update(value).digest('hex');
}

type ActivityKind = 'claude' | 'codex' | 'omp' | 'muse' | 'zcode' | 'jsonl';
/**
 * Checkpoints of these kinds hold rows from an older parser and are read
 * again from the start: OMP rows now keep the routing provider and never mix
 * logged and unlogged events; Muse input no longer includes cache reads;
 * Claude rows now count one API response, not one content block, and the
 * open response is never committed at EOF (R2-1: old rows may hold an
 * early-committed half response that a later scan would count again).
 */
const PARSER_VERSION: Partial<Record<ActivityKind, number>> = { omp: 2, muse: 2, claude: 2 };

function wantedFile(kind: string, name: string): boolean {
  if (kind === 'claude') return name.endsWith('.jsonl');
  if (kind === 'codex') return name.endsWith('.jsonl') && name.startsWith('rollout-');
  // OMP session files (`<ts>_<uuid>[.jsonl]`, `__advisor.jsonl`, `SubAgent/`
  // records); Muse keeps `session.jsonl` per session and subagent.
  if (kind === 'omp') return name.endsWith('.jsonl') || isOmpSessionFilename(name);
  if (kind === 'muse') return name === 'session.jsonl';
  if (kind === 'jsonl') return name.endsWith('.jsonl');
  return false;
}

/** One dot-path step into a generic JSONL record; arrays are never traversed. */
function mappedField(record: unknown, dotPath: string): unknown {
  let current = record;
  for (const part of dotPath.split('.')) {
    if (!current || typeof current !== 'object' || Array.isArray(current)) return undefined;
    current = (current as Record<string, unknown>)[part];
  }
  return current;
}

function mappedCount(value: unknown): number {
  return typeof value === 'number' && Number.isFinite(value) && value > 0 ? Math.floor(value) : 0;
}

/**
 * A generic JSONL usage record through a saved field mapping. The timestamp
 * accepts ISO strings and epoch seconds or milliseconds; token and cost
 * fields accept non-negative numbers, missing as zero/unlogged. Anything
 * else on the line (including malformed JSON) is skipped, never an error.
 */
export function parseJsonlMappedUsageLine(
  line: string,
  mapping: JsonlFieldMapping
): RawUsageEntry | null {
  let record: unknown;
  try {
    record = JSON.parse(line);
  } catch {
    return null;
  }
  if (!record || typeof record !== 'object' || Array.isArray(record)) return null;
  const stamp = mappedField(record, mapping.timestamp);
  let timestamp = '';
  if (typeof stamp === 'string' && stamp) timestamp = stamp;
  else if (typeof stamp === 'number' && Number.isFinite(stamp) && stamp > 0)
    timestamp = new Date(stamp < 1e11 ? stamp * 1000 : stamp).toISOString();
  if (!timestamp || !Number.isFinite(Date.parse(timestamp))) return null;
  const model = mapping.model ? mappedField(record, mapping.model) : undefined;
  const cost = mapping.cost ? mappedField(record, mapping.cost) : undefined;
  const entry: RawUsageEntry = {
    inputTokens: mapping.inputTokens ? mappedCount(mappedField(record, mapping.inputTokens)) : 0,
    outputTokens: mapping.outputTokens ? mappedCount(mappedField(record, mapping.outputTokens)) : 0,
    cacheCreationTokens: 0,
    cacheReadTokens: 0,
    model: typeof model === 'string' && model ? model.slice(0, 256) : '',
    sessionId: '',
    timestamp,
    projectPath: '',
  };
  // A logged 0 is "not logged", never free, like every other kind.
  if (typeof cost === 'number' && Number.isFinite(cost) && cost > 0) entry.costUsd = cost;
  return entry;
}
/**
 * The candidate log files under one root. Like the reads, the walk is synchronous on purpose: it
 * runs on the collector's own worker thread, so several collectors walking at once never queue
 * on the process's shared libuv pool, which the server's own file work also needs.
 */
function filesUnder(
  root: string,
  kind: string,
  issues: { failed: number },
  deadline: number,
  limits: AccountActivityScanOptions['traversalLimits']
): string[] {
  const ceiling = (value: number | undefined, maximum: number): number =>
    Number.isSafeInteger(value) && (value as number) >= 1
      ? Math.min(value as number, maximum)
      : maximum;
  const maxDepth = ceiling(limits?.maxDepth, MAX_DEPTH);
  const maxDirectories = ceiling(limits?.maxDirectories, MAX_DIRECTORIES);
  const maxEntries = ceiling(limits?.maxEntries, MAX_ENTRIES);
  const result: string[] = [];
  const pending = [{ directory: root, depth: 0 }];
  let visitedDirectories = 0;
  let visitedEntries = 0;
  while (pending.length) {
    if (Date.now() >= deadline || visitedDirectories >= maxDirectories) {
      issues.failed++;
      break;
    }
    const current = pending.pop();
    if (!current) break;
    visitedDirectories++;
    try {
      // Streaming iteration also bounds directories containing many irrelevant
      // entries; neither empty directories nor non-JSONL files evade the cap.
      const directory = fs.opendirSync(current.directory);
      try {
        for (let item = directory.readSync(); item !== null; item = directory.readSync()) {
          if (
            Date.now() >= deadline ||
            visitedEntries >= maxEntries ||
            result.length >= MAX_FILES
          ) {
            issues.failed++;
            return result;
          }
          visitedEntries++;
          const file = path.join(current.directory, item.name);
          if (item.isDirectory()) {
            if (
              current.depth >= maxDepth ||
              pending.length + visitedDirectories >= maxDirectories
            ) {
              issues.failed++;
            } else pending.push({ directory: file, depth: current.depth + 1 });
          } else if (item.isFile() && wantedFile(kind, item.name)) result.push(file);
        }
      } finally {
        directory.closeSync();
      }
    } catch {
      issues.failed++;
    }
  }
  return result;
}
function fingerprint(fd: number, start: number, length: number): string {
  const buffer = Buffer.alloc(length);
  const read = fs.readSync(fd, buffer, 0, length, start);
  return hash(buffer.subarray(0, read));
}
function fresh(stats: fs.Stats, minDate: number, kind: ActivityKind): Checkpoint {
  return {
    version: 2,
    size: stats.size,
    mtimeMs: stats.mtimeMs,
    identity: `${stats.dev}:${stats.ino}`,
    head: '',
    tail: '',
    offset: 0,
    minDate,
    complete: false,
    discardingLine: false,
    skippedLines: 0,
    largeLineParserVersion: 3,
    unfinishedTail: false,
    ...(PARSER_VERSION[kind] === undefined ? {} : { parser: PARSER_VERSION[kind] }),
    state: createCodexNativeParserState(),
    rows: [],
  };
}
function loadCheckpoint(
  cache: string,
  file: string,
  stats: fs.Stats,
  minDate: number,
  kind: ActivityKind
): Checkpoint {
  try {
    if (fs.statSync(cache).size > MAX_CACHE_BYTES) return fresh(stats, minDate, kind);
    const value = JSON.parse(fs.readFileSync(cache, 'utf8')) as Checkpoint;
    if (
      value.version !== 2 ||
      value.identity !== `${stats.dev}:${stats.ino}` ||
      !Number.isSafeInteger(value.offset) ||
      value.offset < 0 ||
      value.offset > stats.size ||
      value.size > stats.size ||
      !Number.isFinite(value.minDate) ||
      value.minDate > minDate ||
      !Array.isArray(value.rows) ||
      value.rows.length > MAX_FILE_ROWS ||
      !value.state ||
      typeof value.state.sessionId !== 'string' ||
      (value.size === stats.size && value.mtimeMs !== stats.mtimeMs) ||
      (value.skippedLines > 0 && value.largeLineParserVersion !== 3) ||
      value.parser !== PARSER_VERSION[kind] ||
      (value.pendingClaude !== undefined &&
        (typeof value.pendingClaude.key !== 'string' ||
          !value.pendingClaude.entry ||
          typeof value.pendingClaude.entry !== 'object'))
    )
      return fresh(stats, minDate, kind);
    const fd = fs.openSync(file, 'r');
    try {
      // Check the consumed prefix and boundary before resuming an append. A
      // replaced/truncated/re-written log never inherits cumulative counters.
      if (
        value.head !== fingerprint(fd, 0, Math.min(256, value.offset)) ||
        value.tail !== fingerprint(fd, Math.max(0, value.offset - 256), Math.min(256, value.offset))
      )
        return fresh(stats, minDate, kind);
    } finally {
      fs.closeSync(fd);
    }
    value.rows = value.rows.filter((row) => Date.parse(row.entry.timestamp) >= minDate);
    if (stats.size > value.size) value.complete = false;
    return value;
  } catch {
    return fresh(stats, minDate, kind);
  }
}
function saveCheckpoint(cache: string, file: string, value: Checkpoint, stats: fs.Stats): void {
  const fd = fs.openSync(file, 'r');
  try {
    value.head = fingerprint(fd, 0, Math.min(256, value.offset));
    value.tail = fingerprint(fd, Math.max(0, value.offset - 256), Math.min(256, value.offset));
  } finally {
    fs.closeSync(fd);
  }
  value.size = stats.size;
  value.mtimeMs = stats.mtimeMs;
  const body = JSON.stringify(value);
  if (Buffer.byteLength(body) > MAX_CACHE_BYTES)
    throw new CCSError('Native checkpoint exceeds limit');
  const temporary = `${cache}.${process.pid}.tmp`;
  fs.writeFileSync(temporary, body, { mode: 0o600 });
  fs.renameSync(temporary, cache);
  fs.chmodSync(cache, 0o600);
}
/**
 * One compact row per hour, model, session, tool and routing provider. Events
 * with a logged cost and events without one never share a row, so a row is
 * either wholly logged or wholly unlogged and no unlogged token is ever taken
 * as covered by a logged cost.
 */
function compactKey(entry: RawUsageEntry, timestamp: string): string {
  const logged = entry.costUsd !== undefined && entry.costUsd > 0 ? 'L' : 'U';
  return `${timestamp.slice(0, 13)}\0${entry.model}\0${entry.sessionId}\0${entry.target ?? ''}\0${entry.provider ?? ''}\0${logged}`;
}
function addEntry(rows: Map<string, CompactEntry>, entry: RawUsageEntry, minDate: number): boolean {
  const epoch = Date.parse(entry.timestamp);
  if (!Number.isFinite(epoch) || epoch < minDate) return true;
  const timestamp = new Date(epoch).toISOString();
  const key = compactKey(entry, timestamp);
  const existing = rows.get(key);
  if (existing) {
    for (const field of [
      'inputTokens',
      'outputTokens',
      'cacheCreationTokens',
      'cacheReadTokens',
    ] as const)
      existing.entry[field] += entry[field];
    if (entry.costUsd !== undefined)
      existing.entry.costUsd = (existing.entry.costUsd ?? 0) + entry.costUsd;
    existing.entry.timestamp =
      existing.entry.timestamp > timestamp ? existing.entry.timestamp : timestamp;
    existing.events++;
  } else {
    if (rows.size >= MAX_FILE_ROWS) return false;
    rows.set(key, { entry: { ...entry, timestamp, projectPath: '' }, events: 1 });
  }
  return true;
}
function rowKey(row: CompactEntry): string {
  return compactKey(row.entry, row.entry.timestamp);
}

/**
 * One pre-aggregated per-session-model row, as the analytics helper reports it: the session's
 * published key (the helper hashed the log's own id on the host that read it), first/last event,
 * tokens and cost.
 */
export interface SessionAggregateRow {
  sessionId: string;
  model: string;
  provider?: string;
  target?: string;
  firstMs: number;
  lastMs: number;
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  cost?: number;
  events: number;
}

const MAX_SESSION_ROWS = 10_000;

/**
 * Session rows from pre-aggregated helper rows (remote hosts, local zcode).
 * The hourly rows of the same answer already carry the tokens; these carry
 * the session keys, so the two are aggregated separately and never doubled.
 * `sessionId` is already the published key — derived where the log was read —
 * so it is never hashed again here. Pricing mirrors `aggregateRows`: logged
 * cost wins, the rest prices at list.
 */
export function aggregateSessionAggregates(
  rows: SessionAggregateRow[],
  source: string
): Pick<UsageWorkerResult, 'session'> {
  interface Bucket {
    models: Map<string, ModelBreakdown>;
    loggedCost: Map<string, number>;
    unlogged: Map<string, { input: number; output: number; write: number; read: number }>;
    firstMs: number;
    lastMs: number;
    events: number;
    target?: string;
  }
  const sessions = new Map<string, Bucket>();
  const pricing = new Map<string, ModelPricingResolution>();
  const count = (value: unknown): number =>
    typeof value === 'number' && Number.isFinite(value) && value >= 0 ? Math.floor(value) : 0;
  for (const row of rows) {
    if (!row.sessionId || !Number.isFinite(row.firstMs) || !Number.isFinite(row.lastMs)) continue;
    if (row.lastMs < row.firstMs) continue;
    let bucket = sessions.get(row.sessionId);
    if (!bucket) {
      if (sessions.size >= MAX_SESSION_ROWS) continue;
      bucket = {
        models: new Map(),
        loggedCost: new Map(),
        unlogged: new Map(),
        firstMs: row.firstMs,
        lastMs: row.lastMs,
        events: 0,
        ...(row.target ? { target: row.target } : {}),
      };
      sessions.set(row.sessionId, bucket);
    }
    if (row.firstMs < bucket.firstMs) bucket.firstMs = row.firstMs;
    if (row.lastMs > bucket.lastMs) bucket.lastMs = row.lastMs;
    bucket.events += count(row.events);
    if (row.target) bucket.target = row.target;
    const provider = normalizeUsageProvider(row.provider ?? row.target);
    const modelKey = `${provider ?? ''}\0${row.model}`;
    const model = bucket.models.get(modelKey) ?? {
      modelName: row.model,
      ...(provider && { provider }),
      inputTokens: 0,
      outputTokens: 0,
      cacheCreationTokens: 0,
      cacheReadTokens: 0,
      cost: 0,
    };
    model.inputTokens += count(row.inputTokens);
    model.outputTokens += count(row.outputTokens);
    model.cacheCreationTokens += count(row.cacheCreationTokens);
    model.cacheReadTokens += count(row.cacheReadTokens);
    if (row.cost !== undefined && row.cost > 0) {
      bucket.loggedCost.set(modelKey, (bucket.loggedCost.get(modelKey) ?? 0) + row.cost);
    } else {
      const pending = bucket.unlogged.get(modelKey) ?? { input: 0, output: 0, write: 0, read: 0 };
      pending.input += count(row.inputTokens);
      pending.output += count(row.outputTokens);
      pending.write += count(row.cacheCreationTokens);
      pending.read += count(row.cacheReadTokens);
      bucket.unlogged.set(modelKey, pending);
    }
    bucket.models.set(modelKey, model);
  }
  const session = [...sessions]
    .map(([sessionId, bucket]) => {
      const modelBreakdowns = [...bucket.models.values()];
      for (const model of modelBreakdowns) {
        const key = `${model.provider ?? ''}\0${model.modelName}`;
        let resolved = pricing.get(key);
        if (!resolved) {
          resolved = getModelPricingWithSource(model.modelName, { provider: model.provider });
          pricing.set(key, resolved);
        }
        const rates = resolved.pricing;
        const unlogged = bucket.unlogged.get(key) ?? { input: 0, output: 0, write: 0, read: 0 };
        const listed =
          (unlogged.input / 1_000_000) * rates.inputPerMillion +
          (unlogged.output / 1_000_000) * rates.outputPerMillion +
          (unlogged.write / 1_000_000) * rates.cacheCreationPerMillion +
          (unlogged.read / 1_000_000) * rates.cacheReadPerMillion;
        model.cost = (bucket.loggedCost.get(key) ?? 0) + listed;
        if (resolved.source === 'fallback' && listed > 0) model.fallbackCost = listed;
      }
      const fallbackCost = modelBreakdowns.reduce((sum, item) => sum + (item.fallbackCost ?? 0), 0);
      modelBreakdowns.sort((left, right) => right.cost - left.cost);
      return {
        sessionId,
        projectPath: '',
        source,
        inputTokens: modelBreakdowns.reduce((sum, item) => sum + item.inputTokens, 0),
        outputTokens: modelBreakdowns.reduce((sum, item) => sum + item.outputTokens, 0),
        cacheCreationTokens: modelBreakdowns.reduce(
          (sum, item) => sum + item.cacheCreationTokens,
          0
        ),
        cacheReadTokens: modelBreakdowns.reduce((sum, item) => sum + item.cacheReadTokens, 0),
        cost: modelBreakdowns.reduce((sum, item) => sum + item.cost, 0),
        totalCost: modelBreakdowns.reduce((sum, item) => sum + item.cost, 0),
        ...(fallbackCost > 0 && { fallbackCost }),
        modelsUsed: getModelsUsed(modelBreakdowns),
        modelBreakdowns,
        firstActivity: new Date(bucket.firstMs).toISOString(),
        lastActivity: new Date(bucket.lastMs).toISOString(),
        versions: [],
        target: bucket.target,
      };
    })
    .sort((left, right) => right.lastActivity.localeCompare(left.lastActivity));
  return { session };
}

/**
 * Pricing is stable for one bounded read, so resolve each native model once. `sessionTool` names
 * the tool whose raw session ids `rows` carry, so each becomes its published key here and neither
 * the snapshot on disk nor the page ever holds an id. Rows that carry no session (a remote hourly
 * row, a local zcode hour) simply add no session bucket.
 */
export function aggregateRows(
  rows: CompactEntry[],
  source: string,
  sessionTool: AccountAnalyticsActivityProvider
): Pick<UsageWorkerResult, 'hourly' | 'session'> {
  interface Bucket {
    models: Map<string, ModelBreakdown>;
    /** OMP logged cost per model; those tokens are not priced again. */
    loggedCost: Map<string, number>;
    unlogged: Map<string, { input: number; output: number; write: number; read: number }>;
    requestCount: number;
    firstActivity: string;
    lastActivity: string;
    versions: Set<string>;
    target?: string;
  }
  const hours = new Map<string, Bucket>();
  const sessions = new Map<string, Bucket>();
  const pricing = new Map<string, ModelPricingResolution>();
  const blankBucket = (): Bucket => ({
    models: new Map(),
    loggedCost: new Map(),
    unlogged: new Map(),
    requestCount: 0,
    firstActivity: '',
    lastActivity: '',
    versions: new Set(),
  });
  const add = (map: Map<string, Bucket>, key: string, row: CompactEntry): void => {
    const entry = row.entry;
    const bucket: Bucket = map.get(key) ?? blankBucket();
    // The routing provider prices the row; the tool is the provider only for
    // Claude Code and Codex entries, which carry no separate one.
    const provider = normalizeUsageProvider(entry.provider ?? entry.target);
    const modelKey = `${provider ?? ''}\0${entry.model}`;
    const model = bucket.models.get(modelKey) ?? {
      modelName: entry.model,
      ...(provider && { provider }),
      inputTokens: 0,
      outputTokens: 0,
      cacheCreationTokens: 0,
      cacheReadTokens: 0,
      cost: 0,
    };
    for (const field of [
      'inputTokens',
      'outputTokens',
      'cacheCreationTokens',
      'cacheReadTokens',
    ] as const)
      model[field] += entry[field];
    if (entry.costUsd !== undefined && entry.costUsd > 0) {
      bucket.loggedCost.set(modelKey, (bucket.loggedCost.get(modelKey) ?? 0) + entry.costUsd);
    } else {
      const pending = bucket.unlogged.get(modelKey) ?? { input: 0, output: 0, write: 0, read: 0 };
      pending.input += entry.inputTokens;
      pending.output += entry.outputTokens;
      pending.write += entry.cacheCreationTokens;
      pending.read += entry.cacheReadTokens;
      bucket.unlogged.set(modelKey, pending);
    }
    // Readers that record a route count events per route, so a mixed hour divides by provider exactly.
    if (entry.provider !== undefined) model.requestCount = (model.requestCount ?? 0) + row.events;
    bucket.models.set(modelKey, model);
    bucket.requestCount += row.events;
    if (entry.timestamp > bucket.lastActivity) bucket.lastActivity = entry.timestamp;
    if (!bucket.firstActivity || entry.timestamp < bucket.firstActivity)
      bucket.firstActivity = entry.timestamp;
    if (entry.version) bucket.versions.add(entry.version);
    if (entry.target) bucket.target = entry.target;
    map.set(key, bucket);
  };
  for (const row of rows) {
    add(hours, `${row.entry.timestamp.slice(0, 10)} ${row.entry.timestamp.slice(11, 13)}:00`, row);
    if (row.entry.sessionId)
      add(sessions, analyticsSessionKey(sessionTool, row.entry.sessionId), row);
  }
  const values = (bucket: Bucket) => {
    const modelBreakdowns = [...bucket.models.values()];
    for (const model of modelBreakdowns) {
      const key = `${model.provider ?? ''}\0${model.modelName}`;
      let resolved = pricing.get(key);
      if (!resolved) {
        resolved = getModelPricingWithSource(model.modelName, { provider: model.provider });
        pricing.set(key, resolved);
      }
      const rates = resolved.pricing;
      // Logged OMP cost wins where present; the rest prices at list rates.
      const unlogged = bucket.unlogged.get(key) ?? { input: 0, output: 0, write: 0, read: 0 };
      const listed =
        (unlogged.input / 1_000_000) * rates.inputPerMillion +
        (unlogged.output / 1_000_000) * rates.outputPerMillion +
        (unlogged.write / 1_000_000) * rates.cacheCreationPerMillion +
        (unlogged.read / 1_000_000) * rates.cacheReadPerMillion;
      model.cost = (bucket.loggedCost.get(key) ?? 0) + listed;
      // The part priced only at the unknown-model fallback: no logged cost and no listed rate.
      // Present only when nonzero, so listed-rate rows keep their exact shape.
      if (resolved.source === 'fallback' && listed > 0) model.fallbackCost = listed;
    }
    const fallbackCost = modelBreakdowns.reduce((sum, item) => sum + (item.fallbackCost ?? 0), 0);
    modelBreakdowns.sort((left, right) => right.cost - left.cost);
    return {
      source,
      inputTokens: modelBreakdowns.reduce((sum, item) => sum + item.inputTokens, 0),
      outputTokens: modelBreakdowns.reduce((sum, item) => sum + item.outputTokens, 0),
      cacheCreationTokens: modelBreakdowns.reduce((sum, item) => sum + item.cacheCreationTokens, 0),
      cacheReadTokens: modelBreakdowns.reduce((sum, item) => sum + item.cacheReadTokens, 0),
      cost: modelBreakdowns.reduce((sum, item) => sum + item.cost, 0),
      totalCost: modelBreakdowns.reduce((sum, item) => sum + item.cost, 0),
      ...(fallbackCost > 0 && { fallbackCost }),
      modelsUsed: getModelsUsed(modelBreakdowns),
      modelBreakdowns,
    };
  };
  return {
    hourly: [...hours]
      .map(([hour, bucket]) => ({ hour, ...values(bucket), requestCount: bucket.requestCount }))
      .sort((left, right) => right.hour.localeCompare(left.hour)),
    session: [...sessions]
      .map(([sessionId, bucket]) => ({
        sessionId,
        projectPath: '',
        ...values(bucket),
        firstActivity: bucket.firstActivity,
        lastActivity: bucket.lastActivity,
        versions: [...bucket.versions],
        target: bucket.target,
      }))
      .sort((left, right) => right.lastActivity.localeCompare(left.lastActivity)),
  };
}

/**
 * Whether an oversized (> MAX_LINE_BYTES) line could carry a countable usage
 * record for the kind, so a skipped oversized line only blocks scan completion
 * when it may have held usage. Muse writes direct usage in small fixed-envelope
 * `model_completed` events (~730 B, never oversized); its oversized lines are
 * `context_projection_checkpoint`/retained content records the parser never
 * counts (cumulative, would double tokens), so skipping them must not pin the
 * whole activity partial forever (N9). But older lines can wrap a usage record
 * in `children[].record_json`, and a `children` batch can be oversized, so an
 * oversized line with a `children` key in its prefix stays honestly partial.
 * (The checkpoint kind itself sits ~25 KB into the line, past a huge
 * instructions string, so the prefix cannot match on kind directly.) Codex's
 * oversized lines are `response_item` conversation content, never token_count.
 * Claude/OMP can carry usage in a large assistant/message line, so stay honest.
 */
function oversizedLineCarriesUsage(
  kind: ActivityKind,
  recordType: string | undefined,
  prefix: string
): boolean {
  if (kind === 'muse') return /"children"\s*:/.test(prefix);
  if (kind === 'codex') return recordType !== 'response_item';
  return true;
}

/** Bytes read per chunk; each chunk is a fresh buffer because line fragments keep slices of it. */
const READ_CHUNK_BYTES = 1024 * 1024;

/**
 * Read one file's next records into its checkpoint. Reads are synchronous on purpose: this runs
 * inside a collector's worker thread, never on the server's event loop, so a blocking read only
 * holds that thread. Several collectors running at once then read in parallel on their own
 * (low-priority) threads instead of queueing on the process's small shared libuv pool, which the
 * server's own file and crypto work also needs.
 */
function readBatch(
  file: string,
  value: Checkpoint,
  stats: fs.Stats,
  kind: ActivityKind,
  options: AccountActivityScanOptions,
  deadline: number,
  mapping?: JsonlFieldMapping
): void {
  if (value.complete && value.offset === stats.size) return;
  value.unfinishedTail = false;
  const rows = new Map(value.rows.map((row) => [rowKey(row), row]));
  let fragments: Buffer[] = [];
  let lineBytes = 0;
  let discarding = value.discardingLine;
  let position = value.offset;
  // A partition reader owns its chunk alone: no file can starve another, so the
  // default per-file byte cap is lifted and the scan deadline bounds the read.
  const maxBytes =
    options.codexPartitionRead === true && options.maxBytesPerFile === undefined
      ? Number.MAX_SAFE_INTEGER
      : Math.max(1, Math.min(512 * 1024 * 1024, options.maxBytesPerFile ?? 256 * 1024 * 1024));
  const end = Math.min(stats.size, position + maxBytes);
  if (end <= position) {
    value.complete = position === stats.size && !discarding;
    return;
  }
  const fileSessionId =
    kind === 'omp' ? ompSessionIdForFile(file) : kind === 'muse' ? museSessionIdForFile(file) : '';
  // Claude writes several assistant lines per API response (one per content
  // block, usage repeated, last line complete). Consecutive same-response
  // lines collapse to the last; the group-in-progress rides in the checkpoint
  // so a pass cut mid-response resumes rather than double-counts. A group
  // only commits when the next response (or solo line) arrives — never at
  // EOF, where the file may still be growing (R2-1).
  const flushPendingClaude = (): void => {
    const pending = value.pendingClaude;
    if (kind !== 'claude' || !pending) return;
    value.pendingClaude = undefined;
    if (!addEntry(rows, pending.entry, options.minDate)) value.skippedLines++;
  };
  const consume = (buffer: Buffer): void => {
    const line = buffer.toString('utf8');
    // Avoid parsing conversations/prompts: only actual native usage and the
    // Codex metadata required to interpret its cumulative counters are read.
    let entry: RawUsageEntry | null = null;
    if (kind === 'codex') entry = parseCodexNativeUsageLine(line, value.state);
    else if (kind === 'claude') {
      if (/"type"\s*:\s*"assistant"/.test(line) && /"usage"\s*:/.test(line)) {
        const parsed = parseUsageEntry(line, '');
        if (parsed) {
          if (parsed.responseKey === undefined) {
            flushPendingClaude();
            entry = parsed;
          } else if (value.pendingClaude?.key === parsed.responseKey) {
            value.pendingClaude.entry = parsed;
          } else {
            flushPendingClaude();
            value.pendingClaude = { key: parsed.responseKey, entry: parsed };
          }
        }
      }
    } else if (kind === 'omp') {
      if (/"type"\s*:\s*"message"/.test(line) && /"usage"\s*:/.test(line))
        entry = parseOmpUsageLine(line, fileSessionId);
    } else if (kind === 'muse') {
      if (line.includes('model_completed')) entry = parseMuseUsageLine(line, fileSessionId);
    } else if (kind === 'jsonl') {
      if (mapping) entry = parseJsonlMappedUsageLine(line, mapping);
    }
    if (entry && !addEntry(rows, entry, options.minDate)) value.skippedLines++;
  };
  const fd = fs.openSync(file, 'r');
  try {
    for (let readAt = position; readAt < end; ) {
      const buffer = Buffer.allocUnsafe(Math.min(READ_CHUNK_BYTES, end - readAt));
      const read = fs.readSync(fd, buffer, 0, buffer.length, readAt);
      if (read <= 0) break;
      readAt += read;
      const chunk = read === buffer.length ? buffer : buffer.subarray(0, read);
      let start = 0;
      while (start < chunk.length) {
        const newline = chunk.indexOf(10, start);
        const stop = newline < 0 ? chunk.length : newline;
        const slice = chunk.subarray(start, stop);
        if (!discarding) {
          lineBytes += slice.length;
          if (lineBytes > MAX_LINE_BYTES) {
            discarding = true;
            // A native response_item contains conversation content, never a
            // token_count. Its unambiguous leading top-level type lets us skip
            // even enormous payload strings without losing any usage records.
            const prefixParts: Buffer[] = [];
            let prefixBytes = 0;
            for (const fragment of fragments.length ? fragments : [slice]) {
              const part = fragment.subarray(0, 512 - prefixBytes);
              prefixParts.push(part);
              prefixBytes += part.length;
              if (prefixBytes >= 512) break;
            }
            const prefix = Buffer.concat(prefixParts, prefixBytes).toString('utf8');
            const recordType = prefix.match(
              /^\s*\{\s*(?:"timestamp"\s*:\s*"[^"\\]*"\s*,\s*)?(?:"ordinal"\s*:\s*\d+\s*,\s*)?"type"\s*:\s*"([^"\\]+)"/
            );
            if (oversizedLineCarriesUsage(kind, recordType?.[1], prefix)) value.skippedLines++;
            fragments = [];
          } else fragments.push(slice);
        }
        position += stop - start + (newline >= 0 ? 1 : 0);
        if (newline >= 0) {
          if (!discarding)
            consume(fragments.length === 1 ? fragments[0] : Buffer.concat(fragments, lineBytes));
          fragments = [];
          lineBytes = 0;
          discarding = false;
          value.offset = position;
        }
        start = stop + (newline >= 0 ? 1 : 0);
      }
      if (Date.now() >= deadline) break;
    }
    if (position === stats.size && lineBytes > 0 && !discarding) {
      const final = Buffer.concat(fragments, lineBytes);
      // A complete final record without a newline is valid. An unfinished
      // record remains at its prior boundary until the writer finishes it.
      try {
        JSON.parse(final.toString('utf8'));
        consume(final);
        value.offset = position;
      } catch {
        value.unfinishedTail = true;
      }
    } else if (discarding) value.offset = position;
    value.discardingLine = discarding;
    // The open response is never committed at EOF: the last group may still
    // be growing, and committing it here counts it again when the next scan
    // continues it (R2-1). It stays in the checkpoint and is folded into the
    // reported totals only (see collectAccountActivity).
    value.complete = value.offset === stats.size && !discarding;
    value.rows = [...rows.values()];
  } finally {
    fs.closeSync(fd);
  }
}

interface ZcodeCache {
  /** 3: per-session aggregates ride with the hourly rows (2 held hours only). */
  version: 3;
  minDate: number;
  fingerprints: Record<string, ZcodeFingerprint>;
  rows: CompactEntry[];
  sessions: ZcodeHelperSessionRow[];
}

function blankZcodeCache(minDate: number): ZcodeCache {
  return { version: 3, minDate, fingerprints: {}, rows: [], sessions: [] };
}

function zcodeCachePath(directory: string, dbPath: string): string {
  return path.join(directory, `${hash(`zcode:${dbPath}`)}.json`);
}

function loadZcodeCache(cache: string, minDate: number): ZcodeCache {
  try {
    if (fs.statSync(cache).size > MAX_CACHE_BYTES) return blankZcodeCache(minDate);
    const value = JSON.parse(fs.readFileSync(cache, 'utf8')) as ZcodeCache;
    if (
      value.version !== 3 ||
      !Number.isFinite(value.minDate) ||
      value.minDate > minDate ||
      !value.fingerprints ||
      typeof value.fingerprints !== 'object' ||
      !Array.isArray(value.rows) ||
      value.rows.length > MAX_TOTAL_ROWS ||
      !Array.isArray(value.sessions) ||
      value.sessions.length > MAX_TOTAL_ROWS
    )
      return blankZcodeCache(minDate);
    value.rows = value.rows.filter((row) => Date.parse(row.entry.timestamp) >= minDate);
    value.sessions = value.sessions.filter((row) => typeof row.z === 'number' && row.z >= minDate);
    return value;
  } catch {
    return blankZcodeCache(minDate);
  }
}

function saveZcodeCache(cache: string, value: ZcodeCache): void {
  const body = JSON.stringify({
    ...value,
    rows: value.rows.slice(0, MAX_TOTAL_ROWS),
    sessions: value.sessions.slice(0, MAX_TOTAL_ROWS),
  });
  if (Buffer.byteLength(body) > MAX_CACHE_BYTES)
    throw new CCSError('Native checkpoint exceeds limit');
  const temporary = `${cache}.${process.pid}.tmp`;
  fs.writeFileSync(temporary, body, { mode: 0o600 });
  fs.renameSync(temporary, cache);
  fs.chmodSync(cache, 0o600);
}

function sameZcodePrint(left: ZcodeFingerprint | undefined, right: ZcodeFingerprint | undefined) {
  return (
    !!left &&
    !!right &&
    left.size === right.size &&
    left.mtimeMs === right.mtimeMs &&
    (left.walSize ?? 0) === (right.walSize ?? 0) &&
    (left.walMtimeMs ?? 0) === (right.walMtimeMs ?? 0)
  );
}

async function collectZcodeAccountActivity(
  dbPath: string,
  options: AccountActivityScanOptions,
  directory: string,
  deadline: number
): Promise<UsageWorkerResult> {
  const cache = zcodeCachePath(directory, dbPath);
  const cached = loadZcodeCache(cache, options.minDate);
  let failed = 0;
  let truncated = false;
  if (Date.now() < deadline) {
    try {
      const fresh = queryLocalZcodeUsage(dbPath, options.minDate, cached.fingerprints, {
        timeoutMs: Math.max(1000, deadline - Date.now()),
      });
      if (fresh.state === 'not_installed') throw new CCSError('Native log sources are unavailable');
      if (fresh.state === 'error') {
        // The database could not be read (for example mid-write): keep what
        // was read before, and say the scan is incomplete.
        failed++;
      } else {
        truncated = fresh.truncated;
        const prior = cached.fingerprints;
        const next: Record<string, ZcodeFingerprint> = { ...fresh.fingerprints };
        // A cut scan leaves unconfirmed databases at their previous state.
        if (truncated) for (const [key, print] of Object.entries(prior)) next[key] ??= print;
        // The helper re-sends every row of a changed database, so its cached
        // rows are replaced, never added to; unchanged databases send none.
        const rows = cached.rows.filter((row) => {
          const fileKey = (row.entry as { fileKey?: string }).fileKey ?? '';
          return (
            fileKey in next &&
            sameZcodePrint(prior[fileKey], next[fileKey]) &&
            Date.parse(row.entry.timestamp) >= options.minDate
          );
        });
        const byKey = new Map(rows.map((row) => [rowKey(row), row]));
        for (const helperRow of fresh.rows) {
          const timestamp = `${helperRow.h.replace(' ', 'T')}:00Z`;
          if (Date.parse(timestamp) < options.minDate) continue;
          const entry: RawUsageEntry & { fileKey: string } = {
            inputTokens: helperRow.i,
            outputTokens: helperRow.o,
            cacheCreationTokens: helperRow.cw,
            cacheReadTokens: helperRow.cr,
            model: helperRow.m,
            sessionId: '',
            timestamp,
            projectPath: '',
            target: 'zcode',
            provider: helperRow.p ?? '',
            fileKey: helperRow.f,
          };
          // Helper rows are already per model and hour; merge duplicates.
          const key = rowKey({ entry, events: 0 });
          const existing = byKey.get(key);
          if (existing) {
            existing.entry.inputTokens += entry.inputTokens;
            existing.entry.outputTokens += entry.outputTokens;
            existing.entry.cacheCreationTokens += entry.cacheCreationTokens;
            existing.entry.cacheReadTokens += entry.cacheReadTokens;
            existing.events += helperRow.n;
          } else if (byKey.size < MAX_TOTAL_ROWS) {
            byKey.set(key, { entry, events: helperRow.n });
          } else failed++;
        }
        cached.rows = [...byKey.values()];
        // Session aggregates replace per changed database, like hourly rows.
        const keptSessions = cached.sessions.filter(
          (row) =>
            row.f in next && sameZcodePrint(prior[row.f], next[row.f]) && row.z >= options.minDate
        );
        const seenSessions = new Set(
          keptSessions.map((row) => `${row.f}\0${row.s}\0${row.m}\0${row.p ?? ''}`)
        );
        for (const helperRow of fresh.sessions) {
          if (helperRow.z < options.minDate) continue;
          const key = `${helperRow.f}\0${helperRow.s}\0${helperRow.m}\0${helperRow.p ?? ''}`;
          if (seenSessions.has(key)) continue;
          seenSessions.add(key);
          keptSessions.push(helperRow);
        }
        cached.sessions = keptSessions.slice(0, MAX_TOTAL_ROWS);
        cached.fingerprints = next;
        cached.minDate = options.minDate;
        saveZcodeCache(cache, cached);
      }
    } catch (error) {
      if (error instanceof CCSError) throw error;
      failed++;
    }
  }
  if (!cached.rows.length && failed > 0) throw new CCSError('Native log sources could not be read');
  const { hourly } = aggregateRows(cached.rows, 'zcode-native', 'zcode');
  // Sessions aggregate from the session rows alone; the hourly rows above
  // already carry the same tokens.
  const { session } = aggregateSessionAggregates(
    cached.sessions.map((row) => ({
      sessionId: row.s,
      model: row.m,
      ...(row.p ? { provider: row.p } : {}),
      target: 'zcode',
      firstMs: row.a,
      lastMs: row.z,
      inputTokens: row.i,
      outputTokens: row.o,
      cacheCreationTokens: row.cw,
      cacheReadTokens: row.cr,
      ...(row.c > 0 ? { cost: row.c } : {}),
      events: row.n,
    })),
    'zcode-native'
  );
  return {
    daily: [],
    monthly: [],
    hourly,
    session,
    eventCount: cached.rows.reduce((sum, row) => sum + row.events, 0),
    scan: {
      complete: !truncated && failed === 0,
      completedFiles: failed === 0 ? 1 : 0,
      totalFiles: 1,
      skippedLines: 0,
      failedFiles: failed,
      readBytes: 0,
      unfinishedFiles: 0,
    },
  };
}

/** True when `small` is a byte prefix of `large`: the same head and the same bytes where `small` ends. */
function isPrefixCopy(
  small: { file: string; stats: fs.Stats },
  large: { file: string; stats: fs.Stats }
): boolean {
  const length = small.stats.size;
  if (length > large.stats.size) return false;
  let left: number | undefined;
  let right: number | undefined;
  try {
    left = fs.openSync(small.file, 'r');
    right = fs.openSync(large.file, 'r');
    const head = Math.min(256, length);
    const tail = Math.max(0, length - 256);
    return (
      fingerprint(left, 0, head) === fingerprint(right, 0, head) &&
      fingerprint(left, tail, length - tail) === fingerprint(right, tail, length - tail)
    );
  } catch {
    return false;
  } finally {
    if (left !== undefined) fs.closeSync(left);
    if (right !== undefined) fs.closeSync(right);
  }
}

/**
 * A resumed OMP run copies the session file into its new root and appends to
 * the copy, so every record of the older copy is also in the newer one. Per
 * session, the longest file is kept and every other file that is a byte
 * prefix of a kept one is dropped. Files that diverge are all kept.
 */
export function dropOmpResumeCopies<T extends { file: string; stats: fs.Stats }>(files: T[]): T[] {
  const groups = new Map<string, T[]>();
  for (const item of files) {
    const key = ompSessionCopyKey(item.file);
    if (!key) continue;
    const group = groups.get(key) ?? [];
    group.push(item);
    groups.set(key, group);
  }
  const dropped = new Set<string>();
  for (const group of groups.values()) {
    if (group.length < 2) continue;
    group.sort(
      (left, right) => right.stats.size - left.stats.size || left.file.localeCompare(right.file)
    );
    const kept: T[] = [];
    for (const item of group) {
      if (kept.some((larger) => isPrefixCopy(item, larger))) dropped.add(item.file);
      else kept.push(item);
    }
  }
  return dropped.size ? files.filter((item) => !dropped.has(item.file)) : files;
}

/**
 * Byte-identical copies of one session log — e.g. a Claude subagent transcript
 * written under two project dirs when a session spans two cwds (observed: 147
 * duplicated `subagents/agent-*.jsonl` files, ~25k double-counted records), or a
 * manually copied session. Each copy holds the same records, so reading both
 * double-counts. Group by the same (size, head, tail) fingerprint the checkpoints
 * use and keep one per group. OMP copies are prefix copies handled by
 * dropOmpResumeCopies (a superset of exact copies), so this covers the rest.
 */
export function dropExactDuplicateFiles<T extends { file: string; stats: fs.Stats }>(
  files: T[]
): T[] {
  const seen = new Set<string>();
  const result: T[] = [];
  let dropped = 0;
  for (const item of files) {
    let fd: number | undefined;
    let key: string | null;
    try {
      fd = fs.openSync(item.file, 'r');
      const size = item.stats.size;
      const tailStart = Math.max(0, size - 256);
      key = `${size}:${fingerprint(fd, 0, Math.min(256, size))}:${fingerprint(fd, tailStart, size - tailStart)}`;
    } catch {
      key = null;
    } finally {
      if (fd !== undefined) fs.closeSync(fd);
    }
    // Unreadable here: keep it so readBatch surfaces the failure honestly.
    if (key !== null) {
      if (seen.has(key)) {
        dropped++;
        continue;
      }
      seen.add(key);
    }
    result.push(item);
  }
  return dropped ? result : files;
}

/**
 * A fanned-out codex scan (N14). One collector reading ~13 GB alone needs ~6
 * passes of its 12 s budget to converge cold; several readers, each with the
 * full budget on its own low-priority thread, converge in ~2. Only codex
 * fans out: it is the only kind whose cold read spans many passes.
 *
 * Shape of a fanned-out scan, all inside this file plus one spawner:
 * discovery (walk, stat, mtime pre-filter, exact-dedup, newest-first sort)
 * runs once in the parent, exactly as the inline scan does. The parent loads
 * every checkpoint, splits the files with unread bytes into contiguous
 * balanced chunks, and hands each chunk to a nested partition reader with the
 * shared checkpoint directory: exactly one reader per file, per-file
 * checkpoints, the same readBatch, the same rows. The parent merges the
 * per-file outcomes back in global newest-first order (files no reader saw
 * tally from the just-loaded checkpoints), so row order, the row cap and
 * every tally match the inline scan exactly.
 *
 * Below CODEX_FANOUT_THRESHOLD_BYTES of unread data the parent reads inline
 * instead — the warm path is byte-for-byte today's code path, so warm cycles
 * never get slower. Checkpoint names, directories and content are unchanged,
 * so existing caches survive the upgrade with no forced re-read.
 */
export const CODEX_FANOUT_THRESHOLD_BYTES = 64 * 1024 * 1024;
/**
 * Never more readers than this per scan, however much unread data or however
 * many CPUs. Note this bounds each scan, not the process: every extra saved
 * Codex source fans out its own scan independently. That is fine in practice
 * (each scan only fans out past 64 MB of unread data, and extra Codex sources
 * are rare), and a true process-wide cap would need cross-thread coordination
 * for a case that never happens.
 */
export const CODEX_MAX_READERS = 4;
/** About this much unread data per reader; fewer readers when there is less. */
const CODEX_READER_BYTES = 512 * 1024 * 1024;
/**
 * How long past the shared read deadline the parent waits for a reader's
 * checkpoint saves and answer before terminating it. Readers stop new reads
 * at the deadline themselves; this only covers finishing the current file.
 */
const CODEX_PARTITION_GRACE_MS = 2000;

/**
 * How many readers split one codex scan: 1 (inline) unless at least two
 * files with unread bytes hold at least the threshold between them, then up
 * to CODEX_MAX_READERS, bounded by ~512 MB per reader, a quarter of the CPUs
 * (the rest stay free for the server and whatever else shares the host) and
 * the unread-file count. `fileCount` counts only files with unread bytes.
 * Pure, so tests pin it without workers.
 */
export function codexReaderCount(
  unreadBytes: number,
  fileCount: number,
  cpus: number,
  thresholdBytes: number = CODEX_FANOUT_THRESHOLD_BYTES
): number {
  if (fileCount < 2 || unreadBytes < thresholdBytes) return 1;
  const byData = Math.ceil(unreadBytes / CODEX_READER_BYTES);
  const byMachine = Math.max(2, Math.floor(cpus / 4));
  return Math.max(2, Math.min(CODEX_MAX_READERS, byData, byMachine, fileCount));
}

/**
 * Split files, in their global newest-first order, into at most `readerCount`
 * contiguous chunks balanced by unread bytes. Contiguous chunks keep the merge
 * order identical to the inline scan; balancing by unread bytes (not file
 * count) keeps one live-tailed giant from idling every other reader. Pure.
 */
export function partitionCodexFiles(
  entries: ReadonlyArray<{ file: string; unreadBytes: number }>,
  readerCount: number
): string[][] {
  const readers = Math.max(1, Math.min(readerCount, entries.length));
  if (readers === 1) return [entries.map((entry) => entry.file)];
  const prefix: number[] = [];
  let running = 0;
  for (const entry of entries) {
    running += Math.max(0, entry.unreadBytes);
    prefix.push(running);
  }
  const target = running / readers;
  // Cut after the entry nearest each multiple of the target. Every cut must
  // leave room for the cuts still to come plus a non-empty remainder, so a
  // cut never lands on the last entry and chunks are never empty (trailing
  // zero-unread files just yield fewer chunks than readers).
  const cuts: number[] = [];
  let previous = -1;
  for (let j = 1; j <= readers - 1; j++) {
    const goal = target * j;
    const low = previous + 1;
    const high = entries.length - 1 - readers + j;
    if (low > high) break;
    let cut = low;
    while (cut < high && prefix[cut] < goal) cut++;
    if (cut > low && goal - prefix[cut - 1] < prefix[cut] - goal) cut--;
    cuts.push(cut);
    previous = cut;
  }
  const chunks: string[][] = [];
  let start = 0;
  for (const cut of cuts) {
    chunks.push(entries.slice(start, cut + 1).map((entry) => entry.file));
    start = cut + 1;
  }
  chunks.push(entries.slice(start).map((entry) => entry.file));
  return chunks;
}

/** One file's outcome: what the inline loop tallies per file, as data. */
export interface CodexPartitionFileResult {
  file: string;
  rows: CompactEntry[];
  complete: boolean;
  skippedLines: number;
  readBytes: number;
  unfinishedTail: boolean;
  failed: boolean;
}

/**
 * The child side of a fanned-out scan: read exactly the assigned files
 * against the shared checkpoint directory, one file at a time, stopping new
 * reads at the shared deadline. The per-file logic mirrors the inline loop
 * (load, read, save-if-moved, tally); a file that cannot be read reports
 * `failed`, like a throw in the inline loop.
 */
export async function collectCodexPartition(
  spec: CodexPartitionSpec,
  options: AccountActivityScanOptions
): Promise<CodexPartitionFileResult[]> {
  // Deadline-bounded (the default byte cap is lifted in readBatch); an
  // explicitly set maxBytesPerFile still bounds the read, as inline.
  const readerOptions =
    options.maxBytesPerFile === undefined ? { ...options, codexPartitionRead: true } : options;
  const outcomes: CodexPartitionFileResult[] = [];
  for (const file of spec.files) {
    const cache = path.join(spec.checkpointDir, `${hash(file)}.json`);
    try {
      const stats = fs.statSync(file);
      const value = loadCheckpoint(cache, file, stats, options.minDate, 'codex');
      const before = value.offset;
      if (Date.now() < spec.deadline) {
        readBatch(file, value, stats, 'codex', readerOptions, spec.deadline);
        if (value.offset !== before || !fs.existsSync(cache))
          saveCheckpoint(cache, file, value, stats);
      }
      outcomes.push({
        file,
        rows: value.rows,
        complete: value.complete,
        skippedLines: value.skippedLines,
        readBytes: Math.max(0, value.offset - before),
        unfinishedTail: value.unfinishedTail === true,
        failed: false,
      });
    } catch {
      outcomes.push({
        file,
        rows: [],
        complete: false,
        skippedLines: 0,
        readBytes: 0,
        unfinishedTail: false,
        failed: true,
      });
    }
  }
  return outcomes;
}

/** Runs one partition; the default spawns a nested reader thread. Injected by tests. */
export type CodexPartitionRunner = (
  request: Extract<UsageWorkerRequest, { kind: 'codex' }>,
  options: AccountActivityScanOptions,
  partition: CodexPartitionSpec,
  timeoutMs: number
) => Promise<CodexPartitionFileResult[]>;

export interface CollectAccountActivityDeps {
  partitionRunner?: CodexPartitionRunner;
}

interface CodexFileTallies {
  rows: CompactEntry[];
  complete: boolean;
  skippedLines: number;
  readBytes: number;
  unfinishedTail: boolean;
}

interface CodexCounters {
  completed: number;
  skippedLines: number;
  failed: number;
  readBytes: number;
  unfinishedFiles: number;
}

/**
 * Fold one file's tallies into the running rows and counters, in global
 * order. Both codex paths (inline and merged) share this, so the row cap and
 * every tally match by construction.
 */
function tallyCodexFile(
  rows: CompactEntry[],
  tallies: CodexFileTallies,
  counters: CodexCounters
): void {
  counters.readBytes += tallies.readBytes;
  if (tallies.complete) counters.completed++;
  if (tallies.unfinishedTail) counters.unfinishedFiles++;
  counters.skippedLines += tallies.skippedLines;
  const available = MAX_TOTAL_ROWS - rows.length;
  rows.push(...tallies.rows.slice(0, Math.max(0, available)));
  if (tallies.rows.length > available) counters.failed++;
}

/**
 * Read every codex file, inline below the fan-out threshold (today's behavior,
 * reusing the just-loaded checkpoints) or fanned out across nested readers
 * above it. Only files with unread bytes go to readers; the rest tally from
 * the just-loaded checkpoints. Results come back in global newest-first order
 * either way.
 *
 * A reader that never answers (failed to start, crashed, OOM) reports this
 * pass's starting rows for its files — dropping a whole partition would dip
 * Codex on the page for a cycle — with failed++ so the next pass re-reads
 * them. A reader that overruns its bound is every one of its files past the
 * deadline (starting rows, no failed++): its finished files already saved
 * their checkpoints, so the next pass resumes them and the scan converges. A
 * single file a live reader could not read throws like the inline loop (no
 * rows, failed++).
 */
async function readCodexFiles(
  request: Extract<UsageWorkerRequest, { kind: 'codex' }>,
  options: AccountActivityScanOptions,
  directory: string,
  deadline: number,
  scanned: ReadonlyArray<{ file: string; stats: fs.Stats }>,
  deps?: CollectAccountActivityDeps
): Promise<{ rows: CompactEntry[] } & CodexCounters> {
  const counters: CodexCounters = {
    completed: 0,
    skippedLines: 0,
    failed: 0,
    readBytes: 0,
    unfinishedFiles: 0,
  };
  const rows: CompactEntry[] = [];
  const loaded = scanned.map(({ file, stats }) => {
    const cache = path.join(directory, `${hash(file)}.json`);
    return {
      file,
      stats,
      cache,
      value: loadCheckpoint(cache, file, stats, options.minDate, 'codex'),
    };
  });
  const unreadBytes = loaded.reduce(
    (sum, item) => sum + Math.max(0, item.stats.size - item.value.offset),
    0
  );
  // N1: only files with unread bytes go to readers; the rest are tallied from
  // the just-loaded checkpoints, so no reader starts with nothing to read.
  const needsRead = loaded.filter((item) => item.stats.size - item.value.offset > 0);
  const readers = codexReaderCount(
    unreadBytes,
    needsRead.length,
    availableCpus(),
    options.fanoutThresholdBytes ?? CODEX_FANOUT_THRESHOLD_BYTES
  );
  if (readers < 2 || Date.now() >= deadline || needsRead.length === 0) {
    for (const item of loaded) {
      try {
        const before = item.value.offset;
        if (Date.now() < deadline) {
          readBatch(item.file, item.value, item.stats, 'codex', options, deadline);
          if (item.value.offset !== before || !fs.existsSync(item.cache))
            saveCheckpoint(item.cache, item.file, item.value, item.stats);
        }
        tallyCodexFile(
          rows,
          {
            rows: item.value.rows,
            complete: item.value.complete,
            skippedLines: item.value.skippedLines,
            readBytes: Math.max(0, item.value.offset - before),
            unfinishedTail: item.value.unfinishedTail === true,
          },
          counters
        );
      } catch {
        counters.failed++;
      }
    }
    return { rows, ...counters };
  }
  const partitions = partitionCodexFiles(
    needsRead.map((item) => ({
      file: item.file,
      unreadBytes: Math.max(0, item.stats.size - item.value.offset),
    })),
    readers
  ).filter((chunk) => chunk.length > 0);
  // Readers stop new reads at the shared deadline; the grace only covers
  // their checkpoint saves and answers, so a fanned-out pass always ends
  // soon after the deadline, inside the worker's own time bound.
  const timeoutMs = Math.max(1, deadline - Date.now()) + CODEX_PARTITION_GRACE_MS;
  const runner = deps?.partitionRunner ?? spawnCodexPartitionReader;
  const settled = await Promise.all(
    partitions.map(async (files) => {
      try {
        return {
          files,
          outcomes: await runner(
            request,
            options,
            { files, checkpointDir: directory, deadline },
            timeoutMs
          ),
        };
      } catch (error) {
        return { files, error };
      }
    })
  );
  // Merge back in global newest-first order, so row order, the row cap and
  // every tally match the inline scan. Files no reader saw (nothing unread)
  // tally from the just-loaded checkpoints.
  const outcomes = new Map<string, CodexPartitionFileResult>();
  const errors = new Map<string, unknown>();
  const partitioned = new Set<string>();
  for (const part of settled) {
    for (const file of part.files) partitioned.add(file);
    if ('error' in part) {
      for (const file of part.files) errors.set(file, part.error);
    } else {
      for (const outcome of part.outcomes) outcomes.set(outcome.file, outcome);
    }
  }
  const startingTallies = (item: (typeof loaded)[number]): CodexFileTallies => ({
    rows: item.value.rows,
    complete: item.value.complete,
    skippedLines: item.value.skippedLines,
    readBytes: 0,
    unfinishedTail: item.value.unfinishedTail === true,
  });
  for (const item of loaded) {
    const error = errors.get(item.file);
    if (error !== undefined) {
      // S1: the reader never answered (failed to start, crashed, OOM) while
      // its files are presumably fine, so report this pass's starting rows:
      // dropping the whole partition would dip Codex on the page for a cycle
      // (partition 1 holds the newest files). failed++ keeps the scan
      // honestly incomplete so the next pass re-reads them. A timeout is a
      // file past the deadline: starting rows, no failed++.
      tallyCodexFile(rows, startingTallies(item), counters);
      if (!(error instanceof CodexPartitionError && error.timedOut)) counters.failed++;
      continue;
    }
    const outcome = outcomes.get(item.file);
    if (outcome === undefined) {
      tallyCodexFile(rows, startingTallies(item), counters);
      // A reader that answered but omitted an assigned file violated the
      // protocol: starting rows (its files are presumably fine), failed++.
      if (partitioned.has(item.file)) counters.failed++;
      continue;
    }
    // A single file a live reader could not read (vanished, unreadable) reads
    // as a file error with no rows, exactly like a throw in the inline loop.
    if (outcome.failed) counters.failed++;
    else tallyCodexFile(rows, outcome, counters);
  }
  return { rows, ...counters };
}

/** No legacy all-event cache is loaded; compact per-file checkpoints survive workers. */
export async function collectAccountActivity(
  request: Extract<UsageWorkerRequest, { kind: ActivityKind }>,
  options: AccountActivityScanOptions,
  deps?: CollectAccountActivityDeps
): Promise<UsageWorkerResult> {
  const roots =
    request.kind === 'claude'
      ? [request.projectsDir]
      : request.kind === 'codex'
        ? [path.join(request.codexHome, 'sessions')]
        : request.kind === 'omp'
          ? request.roots
          : request.kind === 'muse'
            ? [request.sessionsDir]
            : request.kind === 'jsonl'
              ? request.roots
              : [];
  // Two generic sources over one root with different mappings keep separate
  // checkpoints; otherwise the first mapping's rows would poison the second.
  const mappingKey = request.kind === 'jsonl' ? `\n${JSON.stringify(request.mapping)}` : '';
  const directory = path.join(
    options.cacheDir,
    'account-activity-v1',
    hash(`${request.kind}:${roots.join('\n')}${mappingKey}`)
  );
  fs.mkdirSync(directory, { recursive: true, mode: 0o700 });
  fs.chmodSync(directory, 0o700);
  const deadline = Date.now() + Math.max(1, Math.min(12_000, options.budgetMs ?? 12_000));
  if (request.kind === 'zcode')
    return collectZcodeAccountActivity(request.dbPath, options, directory, deadline);
  const issues = { failed: 0 };
  const files: Array<{ file: string; stats: fs.Stats }> = [];
  // Whether traversal saw any candidate file at all, before the mtime pre-filter
  // drops out-of-window ones. "Found files but all are stale" means no usage in
  // the window (a complete, empty scan), never an unavailable source.
  let sawAnyFile = false;
  for (const root of roots) {
    for (const file of filesUnder(root, request.kind, issues, deadline, options.traversalLimits)) {
      sawAnyFile = true;
      if (Date.now() >= deadline) {
        issues.failed++;
        break;
      }
      try {
        const stats = fs.statSync(file);
        if (stats.mtimeMs < options.minDate - MTIME_SKIP_MARGIN_MS) continue;
        files.push({ file, stats });
      } catch {
        issues.failed++;
      }
      if (files.length >= MAX_FILES) {
        issues.failed++;
        break;
      }
    }
    if (files.length >= MAX_FILES || Date.now() >= deadline) break;
  }
  if (!sawAnyFile && issues.failed) throw new CCSError('Native log sources are unavailable');
  // Resumed OMP runs copy a session into a new root; its records count once.
  const scanned =
    request.kind === 'omp' ? dropOmpResumeCopies(files) : dropExactDuplicateFiles(files);
  scanned.sort(
    (left, right) => right.stats.mtimeMs - left.stats.mtimeMs || left.file.localeCompare(right.file)
  );
  const rows: CompactEntry[] = [];
  let completed = 0;
  let skippedLines = 0;
  let failed = issues.failed;
  let readBytes = 0;
  let unfinishedFiles = 0;
  if (request.kind === 'codex' && request.experimentRoots === undefined) {
    // Codex alone fans out across partition readers above the unread threshold
    // (N14); below it this reads inline, exactly like the loop below.
    // Experiment-roots requests (experimentRoots set) always take the loop:
    // their checkpoint mode differs, so they never fan out.
    const codex = await readCodexFiles(request, options, directory, deadline, scanned, deps);
    rows.push(...codex.rows);
    completed = codex.completed;
    skippedLines += codex.skippedLines;
    failed += codex.failed;
    readBytes = codex.readBytes;
    unfinishedFiles = codex.unfinishedFiles;
  } else {
    for (const { file, stats } of scanned) {
      const cache = path.join(directory, `${hash(file)}.json`);
      try {
        const value = loadCheckpoint(cache, file, stats, options.minDate, request.kind);
        const before = value.offset;
        if (Date.now() < deadline) {
          readBatch(
            file,
            value,
            stats,
            request.kind,
            options,
            deadline,
            request.kind === 'jsonl' ? request.mapping : undefined
          );
          if (value.offset !== before || !fs.existsSync(cache))
            saveCheckpoint(cache, file, value, stats);
        }
        readBytes += Math.max(0, value.offset - before);
        if (value.complete) completed++;
        if (value.unfinishedTail) unfinishedFiles++;
        skippedLines += value.skippedLines;
        const available = MAX_TOTAL_ROWS - rows.length;
        if (request.kind === 'claude' && value.pendingClaude) {
          // Report the open response without storing it (R2-1): merging it
          // into a copy of this file's rows shows exactly what committing
          // would, while the checkpoint keeps it pending for the next scan.
          // The rows are cloned (not just re-keyed) so the merge can never
          // mutate checkpoint-held objects no matter when the save runs.
          // A cap miss here is transient (retried every scan), never stored.
          const fileRows = new Map(
            value.rows.map((row) => [rowKey(row), { entry: { ...row.entry }, events: row.events }])
          );
          if (!addEntry(fileRows, value.pendingClaude.entry, options.minDate)) skippedLines++;
          rows.push(...[...fileRows.values()].slice(0, Math.max(0, available)));
          if (fileRows.size > available) failed++;
        } else {
          rows.push(...value.rows.slice(0, Math.max(0, available)));
          if (value.rows.length > available) failed++;
        }
      } catch {
        failed++;
      }
      // Keep already-checkpointed records available even after the scan budget.
      // Loading every remaining small cache is bounded by MAX_FILES/MAX_TOTAL_ROWS.
    }
  }
  const source =
    request.kind === 'codex'
      ? 'codex-native'
      : request.kind === 'omp'
        ? 'omp-native'
        : request.kind === 'muse'
          ? 'muse-native'
          : 'custom-parser';
  if (!rows.length && scanned.length > 0 && failed >= scanned.length && failed > 0)
    throw new CCSError('Native log sources could not be read');
  const { hourly, session } = aggregateRows(rows, source, request.kind);
  return {
    daily: [],
    monthly: [],
    hourly,
    session,
    eventCount: rows.reduce((sum, row) => sum + row.events, 0),
    scan: {
      complete:
        completed === scanned.length && files.length < MAX_FILES && !skippedLines && !failed,
      completedFiles: completed,
      totalFiles: scanned.length,
      skippedLines,
      failedFiles: failed,
      readBytes,
      unfinishedFiles,
    },
  };
}
