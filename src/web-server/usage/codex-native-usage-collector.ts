import type { RawUsageEntry } from '../jsonl-parser';

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
