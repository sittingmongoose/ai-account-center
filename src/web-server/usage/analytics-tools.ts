import { TOOL_TABLE, type Assert, type Same, type TableAnalyticsTool } from '../../shared/tables';

/**
 * Analytics wire names: config/tables/tools.json `analyticsTools`. Add a tool there, then
 * implement its typed worker payload.
 */
export const ANALYTICS_TOOL_REGISTRY: Readonly<
  Record<TableAnalyticsTool, { readonly experiment: boolean }>
> = Object.fromEntries(
  Object.entries(TOOL_TABLE.analyticsTools).map(([tool, row]) => [
    tool,
    { experiment: row.experiment },
  ])
) as Record<TableAnalyticsTool, { readonly experiment: boolean }>;
export type AnalyticsTool = TableAnalyticsTool;
/**
 * The experiment tools as a type. JSON types widen `true` to `boolean`, so this list names them
 * once more for the type only; tests/spec/tables.test.ts checks it against `experiment: true`.
 */
export const EXPERIMENT_TOOL_TYPES = ['claude', 'codex', 'muse', 'zcode'] as const;
export type ExperimentAnalyticsTool = (typeof EXPERIMENT_TOOL_TYPES)[number];
export type ExperimentToolsAreTools = Assert<
  Same<Extract<ExperimentAnalyticsTool, AnalyticsTool>, ExperimentAnalyticsTool>
>;
export const ANALYTICS_TOOLS: readonly AnalyticsTool[] = Object.freeze(
  Object.keys(ANALYTICS_TOOL_REGISTRY) as AnalyticsTool[]
);
export const EXPERIMENT_ANALYTICS_TOOLS: readonly ExperimentAnalyticsTool[] = Object.freeze(
  ANALYTICS_TOOLS.filter(
    (tool): tool is ExperimentAnalyticsTool => ANALYTICS_TOOL_REGISTRY[tool].experiment
  )
);
export type ActivityKind = AnalyticsTool | 'jsonl';
export function isAnalyticsTool(value: string): value is AnalyticsTool {
  return Object.prototype.hasOwnProperty.call(ANALYTICS_TOOL_REGISTRY, value);
}
export function isActivityKind(value: string): value is ActivityKind {
  return value === 'jsonl' || isAnalyticsTool(value);
}
