/**
 * Analytics usage history (docs/store.md section 12): what the store holds per computer (its oldest
 * hour, last read and gaps, computed in Rust) for the page, and the "Included usage" grid from the
 * store's last reads.
 */
import type { ActivitySources } from './usage-report';
import type {
  AccountAnalyticsHistoryCoverage,
  AccountAnalyticsSource,
} from '../web-server/services/account-analytics-types';

function iso(ms: number): string {
  return new Date(ms).toISOString();
}

export type StoreActivityCoverage = AccountAnalyticsHistoryCoverage;

/** The "Included usage" grid from the store's last reads, for an answer before this run's first pass. */
export function gridFromStore(history: StoreActivityCoverage): AccountAnalyticsSource[] {
  const tools = ['claude', 'codex', 'omp', 'muse', 'zcode', 'jsonl', 'antigravity', 'cursor'];
  const grid: AccountAnalyticsSource[] = [];
  for (const s of history.sources) {
    if (!tools.includes(s.tool)) continue;
    const state: AccountAnalyticsSource['state'] =
      s.state === 'ok'
        ? 'ok'
        : s.state === 'no_usage'
          ? 'no_usage'
          : s.state === 'partial' || s.state === 'cached'
            ? 'cached'
            : s.state === 'scanning'
              ? 'scanning'
              : s.state === 'not_installed' || s.state === 'installed_no_logs'
                ? 'not_installed'
                : 'unavailable';
    grid.push({
      tool: s.tool as AccountAnalyticsSource['tool'],
      host: s.host as AccountAnalyticsSource['host'],
      state,
      lastScanAt: s.lastReadAt,
      rowCount: 0,
      detail:
        state === 'unavailable'
          ? s.state === 'no_reader'
            ? 'no local usage log'
            : (s.cause ?? 'the last read failed')
          : null,
    });
  }
  return grid;
}

export function coverageOf(sources: ActivitySources): StoreActivityCoverage {
  const at = (ms: number | null | undefined) => (typeof ms === 'number' ? iso(ms) : null);
  const backfill = sources.sources
    .filter((s) => s.backfill)
    .map((s) => ({
      host: s.computer,
      tool: s.tool,
      state: s.backfill?.state ?? 'pending',
      filesRead: s.backfill?.files_read ?? null,
      filesTotal: s.backfill?.files_total ?? null,
    }));
  return {
    computers: sources.computers.map((c) => ({
      host: c.computer,
      oldestAt: at(c.oldestMs),
      newestAt: at(c.newestMs),
      lastReadAt: at(c.lastReadMs),
      gaps: c.gaps.map(([a, b]) => [iso(a), iso(b)] as [string, string]),
    })),
    sources: sources.sources.map((s) => ({
      host: s.computer,
      tool: s.tool,
      state: s.state,
      cause: s.cause,
      lastReadAt: at(s.lastReadMs),
      badSince: at(s.badSinceMs),
      oldestAt: at(s.oldestMs),
    })),
    backfill: {
      running: backfill.some((b) => b.state === 'pending' || b.state === 'running'),
      sources: backfill,
    },
  };
}
