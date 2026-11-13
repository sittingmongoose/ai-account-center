/** Analytics wire names. Add a tool here, then implement its typed worker payload. */
export const ANALYTICS_TOOL_REGISTRY = {
  claude: { experiment: true },
  codex: { experiment: true },
  omp: { experiment: false },
  muse: { experiment: true },
  zcode: { experiment: true },
  antigravity: { experiment: false },
} as const;
export type AnalyticsTool = keyof typeof ANALYTICS_TOOL_REGISTRY;
export type ExperimentAnalyticsTool = {
  [Tool in AnalyticsTool]: (typeof ANALYTICS_TOOL_REGISTRY)[Tool]['experiment'] extends true
    ? Tool
    : never;
}[AnalyticsTool];
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
