import type { LoggingConfig, LoggingLevel } from '../../config/unified-config-types';

export type { LoggingConfig, LoggingLevel };

/**
 * Canonical request lifecycle stages.
 *
 * Order represents typical flow but stages may be skipped or repeated.
 * - intake:    inbound request received at an entry edge (HTTP handler, CLI dispatch)
 * - route:     destination/profile/target resolution
 * - auth:      authentication/authorization (token exchange, profile auth)
 * - dispatch:  outbound request prepared / child process spawned
 * - upstream:  upstream call in flight (provider HTTP / spawned child running)
 * - transform: payload translation (request/response shape conversion)
 * - respond:   response written / dispatched to caller (latencyMs typically populated here)
 * - cleanup:   error path, abort, teardown
 */
export type LogStage =
  'intake' | 'route' | 'auth' | 'dispatch' | 'upstream' | 'transform' | 'respond' | 'cleanup';

export const LOG_STAGES: readonly LogStage[] = [
  'intake',
  'route',
  'auth',
  'dispatch',
  'upstream',
  'transform',
  'respond',
  'cleanup',
] as const;

export interface LogErrorInfo {
  name: string;
  message: string;
  code?: string;
  stack?: string;
  cause?: LogErrorInfo;
}

export interface LogEntry {
  id: string;
  timestamp: string;
  level: LoggingLevel;
  source: string;
  event: string;
  message: string;
  processId: number;
  runId: string;
  context?: Record<string, unknown>;
  /** Correlates entries belonging to a single inbound request across stages. */
  requestId?: string;
  /** Lifecycle stage tag — see {@link LogStage}. */
  stage?: LogStage;
  /** Elapsed time in milliseconds, typically attached to `respond`/`cleanup`. */
  latencyMs?: number;
  /** Structured error metadata; never stores raw token strings. */
  error?: LogErrorInfo;
}

export interface LogSourceSummary {
  source: string;
  label: string;
  kind: 'native' | 'legacy';
  count: number;
  lastTimestamp: string | null;
}

export interface ReadLogEntriesOptions {
  source?: string;
  level?: LoggingLevel;
  search?: string;
  limit?: number;
}

export const LOG_LEVELS: readonly LoggingLevel[] = ['error', 'warn', 'info', 'debug'];

const LOG_LEVEL_PRIORITY: Record<LoggingLevel, number> = {
  error: 0,
  warn: 1,
  info: 2,
  debug: 3,
};

export function shouldWriteLogLevel(level: LoggingLevel, configuredLevel: LoggingLevel): boolean {
  return LOG_LEVEL_PRIORITY[level] <= LOG_LEVEL_PRIORITY[configuredLevel];
}

export function isLoggingLevel(value: string | undefined): value is LoggingLevel {
  return typeof value === 'string' && LOG_LEVELS.includes(value as LoggingLevel);
}

export function isLogStage(value: string | undefined): value is LogStage {
  return typeof value === 'string' && LOG_STAGES.includes(value as LogStage);
}
