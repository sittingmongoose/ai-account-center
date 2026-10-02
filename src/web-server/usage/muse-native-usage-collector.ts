/** Muse session usage: line parser and root resolution for Analytics. */
import * as os from 'os';
import * as path from 'path';
import type { RawUsageEntry } from '../jsonl-parser';

export const MUSE_TARGET = 'muse';

/** `recorded_at` units vary (ms/us/ns); normalize by dividing while past 2030. */
export function normalizeMuseRecordedAt(value: unknown): number | null {
  if (typeof value !== 'number' || !Number.isFinite(value) || value <= 0) return null;
  let seconds = Math.floor(value);
  while (seconds > 4102444800) seconds = Math.floor(seconds / 1000);
  return seconds <= 0 ? null : seconds * 1000;
}

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

interface MuseEvent {
  recordedAt: unknown;
  event: Record<string, unknown>;
}

function directEvent(record: unknown): MuseEvent | null {
  if (!record || typeof record !== 'object' || Array.isArray(record)) return null;
  const entry = record as Record<string, unknown>;
  const payload = entry.payload as Record<string, unknown> | undefined;
  const event = payload?.event as Record<string, unknown> | undefined;
  if (!event || event.kind !== 'model_completed') return null;
  return { recordedAt: entry.recorded_at, event };
}

/**
 * Parse one Muse session line. Only `kind=model_completed` records carry
 * usage; `goal_usage_attribution` carries the same quantities and is never
 * counted, or every token would be doubled. Older lines wrap the record in
 * `children[].record_json` (unpacked one level).
 */
export function parseMuseUsageLine(line: string, sessionId: string): RawUsageEntry | null {
  // The collector pre-filters on `model_completed` before this runs, so
  // conversation content is never parsed.
  let record: unknown;
  try {
    record = JSON.parse(line);
  } catch {
    return null;
  }
  let found = directEvent(record);
  if (!found && record && typeof record === 'object' && !Array.isArray(record)) {
    const children = (record as Record<string, unknown>).children;
    if (Array.isArray(children)) {
      for (const child of children) {
        if (!child || typeof child !== 'object' || Array.isArray(child)) continue;
        let embedded = (child as Record<string, unknown>).record_json as unknown;
        if (typeof embedded === 'string') {
          try {
            embedded = JSON.parse(embedded) as unknown;
          } catch {
            continue;
          }
        }
        const inner = directEvent(embedded);
        if (inner) {
          found = {
            recordedAt: (record as Record<string, unknown>).recorded_at,
            event: inner.event,
          };
          break;
        }
      }
    }
  }
  if (!found) return null;
  const epoch = normalizeMuseRecordedAt(found.recordedAt);
  if (epoch === null) return null;
  const model = cleanModel(found.event.model);
  if (!model) return null;
  const usage = found.event.usage as Record<string, unknown> | undefined;
  if (!usage) return null;
  const input = nonNegative(usage.input_tokens);
  const output = nonNegative(usage.output_tokens);
  // cached_tokens duplicates cache_read_tokens: count once. Output already
  // includes reasoning per the Codex convention, so reasoning_tokens is not
  // added. input_tokens is gross: it includes the cache reads (every sampled
  // record has input >= cache read, and the 31-day sums are 2.17B against
  // 2.09B), so the uncached input is the difference, as for zcode.
  const cacheRead = nonNegative(usage.cache_read_tokens);
  const cacheWrite = nonNegative(usage.cache_write_tokens);
  if (input === null || output === null || cacheRead === null || cacheWrite === null) return null;
  return {
    inputTokens: Math.max(0, Math.floor(input) - Math.floor(cacheRead)),
    outputTokens: Math.floor(output),
    cacheCreationTokens: Math.floor(cacheWrite),
    cacheReadTokens: Math.floor(cacheRead),
    model,
    sessionId,
    timestamp: new Date(epoch).toISOString(),
    projectPath: '',
    target: MUSE_TARGET,
    // Muse logs no routing provider; rates are looked up by model name alone.
    provider: '',
  };
}

/** Session id is the enclosing directory name (`<uuid>/session.jsonl`). */
export function museSessionIdForFile(file: string): string {
  return path.basename(path.dirname(file)).slice(0, 160);
}

export interface MuseRootOptions {
  env?: NodeJS.ProcessEnv;
  homeDir?: string;
}

/** Muse sessions root, `~/.local/share/muse/sessions` unless overridden. */
export function resolveMuseSessionsDir(options: MuseRootOptions = {}): string {
  const env = options.env ?? process.env;
  const override = env.MUSE_SESSIONS_DIR;
  if (override && override.trim() && path.isAbsolute(override.trim())) return override.trim();
  return path.join(options.homeDir ?? os.homedir(), '.local', 'share', 'muse', 'sessions');
}
