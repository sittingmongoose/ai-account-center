/**
 * Usage Data Types
 *
 * Type definitions for aggregated usage data.
 * Compatible with better-ccusage interfaces for drop-in replacement.
 */

// ============================================================================
// MODEL BREAKDOWN
// ============================================================================

/** Per-model token and cost breakdown */
export interface ModelBreakdown {
  modelName: string;
  provider?: string;
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  cost: number;
  /**
   * USD of `cost` priced only at the unknown-model fallback rate: tokens with no logged
   * cost and no listed rate. Absent from readers that never price at the fallback.
   */
  fallbackCost?: number;
}

// ============================================================================
// AGGREGATED USAGE TYPES
// ============================================================================

/** Daily usage aggregation (YYYY-MM-DD) */
export interface DailyUsage {
  date: string;
  /** Stable CCS profile name when the source can be attributed to one. */
  profile?: string;
  /** Account email/id when the source can be attributed to a specific CLIProxy account. */
  accountId?: string;
  source: string;
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  cost: number;
  totalCost: number;
  modelsUsed: string[];
  modelBreakdowns: ModelBreakdown[];
}

/** Hourly usage aggregation (YYYY-MM-DD HH:00) */
export interface HourlyUsage {
  hour: string; // Format: "YYYY-MM-DD HH:00"
  /** Stable CCS profile name when the source can be attributed to one. */
  profile?: string;
  source: string;
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  cost: number;
  totalCost: number;
  modelsUsed: string[];
  modelBreakdowns: ModelBreakdown[];
  /** Sum of the breakdowns' `fallbackCost`, when the reader reports it. */
  fallbackCost?: number;
  /**
   * Raw request count for this hour bucket.
   * Optional for backward compatibility with previously persisted snapshots.
   */
  requestCount?: number;
}

/** Monthly usage aggregation (YYYY-MM) */
export interface MonthlyUsage {
  month: string;
  /** Stable CCS profile name when the source can be attributed to one. */
  profile?: string;
  source: string;
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  totalCost: number;
  modelsUsed: string[];
  modelBreakdowns: ModelBreakdown[];
}

/** Session-level usage aggregation */
export interface SessionUsage {
  sessionId: string;
  /** Stable CCS profile name when the source can be attributed to one. */
  profile?: string;
  projectPath: string;
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  cost: number;
  totalCost: number;
  lastActivity: string;
  /**
   * Earliest retained activity, when the reader reports it. Compacted native
   * rows keep the last event of each hour, so this can be up to an hour after
   * the session's true first event.
   */
  firstActivity?: string;
  versions: string[];
  modelsUsed: string[];
  modelBreakdowns: ModelBreakdown[];
  /** Sum of the breakdowns' `fallbackCost`, when the reader reports it. */
  fallbackCost?: number;
  source: string;
  /** Target CLI used for this session (default: 'claude') */
  target?: string;
}

// ============================================================================
// ANALYTICS INSIGHTS TYPES
// ============================================================================

/** Token category with count and cost */
export interface TokenCategoryCost {
  tokens: number;
  cost: number;
}

/** Breakdown of tokens by type with individual costs */
export interface TokenBreakdown {
  input: TokenCategoryCost;
  output: TokenCategoryCost;
  cacheCreation: TokenCategoryCost;
  cacheRead: TokenCategoryCost;
}

/** Anomaly types for usage pattern detection */
export type AnomalyType =
  | 'high_input' // >10M tokens/day/model
  | 'high_io_ratio' // >100x input/output ratio
  | 'cost_spike' // >2x daily average cost
  | 'high_cache_read'; // >1B cache read tokens

/** Single anomaly detection result */
export interface Anomaly {
  date: string;
  type: AnomalyType;
  model?: string;
  value: number;
  threshold: number;
  message: string;
}

/** Summary of all detected anomalies */
export interface AnomalySummary {
  totalAnomalies: number;
  highInputDays: number;
  highIoRatioDays: number;
  costSpikeDays: number;
  highCacheReadDays: number;
}

/** Insights API response */
export interface UsageInsights {
  anomalies: Anomaly[];
  summary: AnomalySummary;
}

/** Extended model usage with cost breakdown */
export interface ExtendedModelUsage {
  model: string;
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  tokens: number;
  cost: number;
  percentage: number;
  costBreakdown: TokenBreakdown;
  ioRatio: number;
}
