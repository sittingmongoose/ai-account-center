import type {
  AccountAnalyticsActivityProvider,
  AccountAnalyticsAnomalies,
} from './account-analytics-types';

/** Port of the original CCS insights (f45fa923 `detectAnomalies`), with the same constants and messages. */
const MAX_ANOMALIES = 100;
const ANOMALY_THRESHOLDS: AccountAnalyticsAnomalies['thresholds'] = {
  costSpikeMultiplier: 2,
  highInputTokens: 10_000_000,
  highIoRatio: 100,
  highCacheReadTokens: 1_000_000_000,
};

function formatTokenCount(tokens: number): string {
  if (tokens >= 1_000_000_000) return `${(tokens / 1_000_000_000).toFixed(1)}B`;
  if (tokens >= 1_000_000) return `${(tokens / 1_000_000).toFixed(1)}M`;
  if (tokens >= 1_000) return `${(tokens / 1_000).toFixed(1)}K`;
  return tokens.toString();
}

/**
 * The original CCS insights rules (f45fa923 `detectAnomalies`), on local days:
 * a cost spike is a day above twice the average of days with activity; the
 * token rules apply per provider and model per day, before the "Other models" fold.
 */
export function detectAccountAnalyticsAnomalies(
  days: Array<{ date: string; cost: number }>,
  models: Array<{
    date: string;
    provider: AccountAnalyticsActivityProvider;
    model: string;
    inputTokens: number;
    outputTokens: number;
    cacheReadTokens: number;
  }>
): AccountAnalyticsAnomalies {
  const items: AccountAnalyticsAnomalies['items'] = [];
  const totalCost = days.reduce((sum, day) => sum + day.cost, 0);
  const average = days.length > 0 ? totalCost / days.length : 0;
  const spikeThreshold = average * ANOMALY_THRESHOLDS.costSpikeMultiplier;
  const modelsByDate = new Map<string, typeof models>();
  for (const row of models) {
    const list = modelsByDate.get(row.date) ?? [];
    list.push(row);
    modelsByDate.set(row.date, list);
  }
  const dates = [...new Set([...days.map((day) => day.date), ...modelsByDate.keys()])].sort();
  const costs = new Map(days.map((day) => [day.date, day.cost]));
  for (const date of dates) {
    const cost = costs.get(date);
    if (cost !== undefined && average > 0 && cost > spikeThreshold) {
      const multiplier = Math.round((cost / average) * 10) / 10;
      items.push({
        date,
        type: 'cost_spike',
        provider: null,
        model: null,
        value: cost,
        threshold: average,
        message: `Cost ${multiplier}x above daily average ($${Math.round(cost)} vs $${Math.round(average)})`,
      });
    }
    for (const row of modelsByDate.get(date) ?? []) {
      if (row.inputTokens > ANOMALY_THRESHOLDS.highInputTokens) {
        const multiplier =
          Math.round((row.inputTokens / ANOMALY_THRESHOLDS.highInputTokens) * 10) / 10;
        items.push({
          date,
          type: 'high_input',
          provider: row.provider,
          model: row.model,
          value: row.inputTokens,
          threshold: ANOMALY_THRESHOLDS.highInputTokens,
          message: `Input tokens ${multiplier}x above threshold (${formatTokenCount(row.inputTokens)})`,
        });
      }
      if (row.outputTokens > 0) {
        const ratio = row.inputTokens / row.outputTokens;
        if (ratio > ANOMALY_THRESHOLDS.highIoRatio) {
          const multiplier = Math.round((ratio / ANOMALY_THRESHOLDS.highIoRatio) * 10) / 10;
          items.push({
            date,
            type: 'high_io_ratio',
            provider: row.provider,
            model: row.model,
            value: ratio,
            threshold: ANOMALY_THRESHOLDS.highIoRatio,
            message: `I/O ratio ${multiplier}x above threshold (${Math.round(ratio)}:1)`,
          });
        }
      }
      if (row.cacheReadTokens > ANOMALY_THRESHOLDS.highCacheReadTokens) {
        const multiplier =
          Math.round((row.cacheReadTokens / ANOMALY_THRESHOLDS.highCacheReadTokens) * 10) / 10;
        items.push({
          date,
          type: 'high_cache_read',
          provider: row.provider,
          model: row.model,
          value: row.cacheReadTokens,
          threshold: ANOMALY_THRESHOLDS.highCacheReadTokens,
          message: `Cache reads ${multiplier}x above threshold (${formatTokenCount(row.cacheReadTokens)})`,
        });
      }
    }
  }
  const sorted = items.sort((a, b) => b.date.localeCompare(a.date));
  const daysOf = (type: AccountAnalyticsAnomalies['items'][number]['type']) =>
    new Set(sorted.filter((item) => item.type === type).map((item) => item.date)).size;
  return {
    thresholds: { ...ANOMALY_THRESHOLDS },
    items: sorted.slice(0, MAX_ANOMALIES),
    summary: {
      totalAnomalies: sorted.length,
      highInputDays: daysOf('high_input'),
      highIoRatioDays: daysOf('high_io_ratio'),
      costSpikeDays: daysOf('cost_spike'),
      highCacheReadDays: daysOf('high_cache_read'),
    },
  };
}
