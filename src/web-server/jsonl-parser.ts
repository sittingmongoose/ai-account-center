/**
 * JSONL line parser for Claude Code Usage Analytics
 *
 * Parses `~/.claude/projects/` JSONL lines into raw usage entries.
 * - Only parses "assistant" entries with usage data
 * - Graceful error handling for malformed entries (they count as no usage, never as an error)
 */

/** Raw usage data from JSONL entry */
export interface RawUsageEntry {
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  model: string;
  sessionId: string;
  timestamp: string;
  projectPath: string;
  version?: string;
  target?: string;
  /**
   * Routing provider used to look up list rates (OMP `message.provider`, zcode
   * `provider_id`), kept apart from `target`, the tool that logged the entry.
   * `''` means the log names none. When absent, `target` is the provider
   * (Claude Code and Codex).
   */
  provider?: string;
  /**
   * Logged USD cost for this entry (OMP `usage.cost.total`), present only
   * when nonzero. A logged 0 is "not logged", never free, and falls through
   * to the price resolver.
   */
  costUsd?: number;
  /**
   * One API response, not one content block: Claude Code writes several
   * assistant lines per response (same message id and request id, usage
   * repeated, last line complete). The collector collapses consecutive
   * same-key lines and counts the response once. Absent when the line names
   * no message id; such lines count solo.
   */
  responseKey?: string;
}

/** Internal structure matching JSONL assistant entries */
interface JsonlAssistantEntry {
  type: 'assistant';
  sessionId: string;
  timestamp: string;
  version?: string;
  cwd?: string;
  target?: string;
  message: {
    model: string;
    usage: {
      input_tokens: number;
      output_tokens: number;
      cache_creation_input_tokens?: number;
      cache_read_input_tokens?: number;
    };
  };
}

/**
 * Parse a single JSONL line into RawUsageEntry if valid
 * Returns null for non-assistant entries or entries without usage data
 */
function toNonNegativeNumber(value: unknown): number {
  const numeric = typeof value === 'number' ? value : Number(value);
  if (!Number.isFinite(numeric) || numeric < 0) {
    return 0;
  }
  return numeric;
}

function parseRequiredNonNegativeNumber(value: unknown): number | null {
  const numeric = typeof value === 'number' ? value : Number(value);
  if (!Number.isFinite(numeric) || numeric < 0) {
    return null;
  }
  return numeric;
}

function isValidTimestamp(value: unknown): value is string {
  return typeof value === 'string' && Number.isFinite(Date.parse(value));
}

export function parseUsageEntry(line: string, projectPath: string): RawUsageEntry | null {
  // Strip UTF-8 BOM if present (can occur on first line of some files)
  const cleanLine = line.replace(/^\uFEFF/, '').trim();
  if (!cleanLine) return null;

  try {
    const entry = JSON.parse(cleanLine);

    // Only process assistant entries with usage data
    if (entry.type !== 'assistant') return null;
    if (!entry.message?.usage) return null;
    if (!entry.message?.model) return null;

    const usage = entry.message.usage;
    const assistant = entry as JsonlAssistantEntry;
    const inputTokens = parseRequiredNonNegativeNumber(usage.input_tokens);
    const outputTokens = parseRequiredNonNegativeNumber(usage.output_tokens);
    if (inputTokens === null || outputTokens === null || !isValidTimestamp(assistant.timestamp)) {
      return null;
    }
    const normalizedProjectPath =
      typeof assistant.cwd === 'string' && assistant.cwd.trim().length > 0
        ? assistant.cwd
        : projectPath;
    const messageId = (entry.message as { id?: unknown })?.id;
    const requestId =
      (entry as { requestId?: unknown }).requestId ??
      (entry.message as { requestId?: unknown })?.requestId;
    // Overlong ids fail open to solo-counting; real ids are tens of chars.
    const responseKey =
      typeof messageId === 'string' &&
      messageId.length > 0 &&
      messageId.length <= 200 &&
      (requestId === undefined || (typeof requestId === 'string' && requestId.length <= 200))
        ? `${messageId}\n${typeof requestId === 'string' ? requestId : ''}`
        : undefined;

    return {
      inputTokens,
      outputTokens,
      cacheCreationTokens: toNonNegativeNumber(usage.cache_creation_input_tokens),
      cacheReadTokens: toNonNegativeNumber(usage.cache_read_input_tokens),
      model: assistant.message.model,
      sessionId: assistant.sessionId || '',
      timestamp: assistant.timestamp,
      projectPath: normalizedProjectPath,
      ...(responseKey === undefined ? {} : { responseKey }),
      version: assistant.version,
      target:
        typeof (entry as { target?: unknown }).target === 'string'
          ? ((entry as { target?: string }).target as string)
          : undefined,
    };
  } catch {
    // Malformed JSON - skip silently
    return null;
  }
}
