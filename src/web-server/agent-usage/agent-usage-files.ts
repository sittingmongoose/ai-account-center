import {
  longBurnKey,
  type AccountAnalyticsSampleEvent,
} from '../services/account-analytics-service';
import type { LongBurnSpan } from '../../store/store-service';
import {
  HISTORY_HOURS,
  burnKey,
  usageHistory,
  type BurnRate,
  type HistorySeries,
  type RecentReadings,
} from './agent-usage-history';
import { projectAgentUsage, type AgentUsageWindow } from './agent-usage-projection';
import type {
  AgentUsageSnapshot,
  SnapshotAccount,
  SnapshotSignInProfile,
  SnapshotSignInRenewal,
} from './agent-usage-markdown';

export interface AgentUsageHistoryFile {
  version: 1;
  generatedAt: string;
  hours: number;
  /** Each point is [time, used percent, remaining amount, reset time]; null when that reading had none. */
  series: HistorySeries[];
}

/** The snapshot and history files for one sample. */
export function buildSnapshotFiles(
  event: AccountAnalyticsSampleEvent,
  refreshSeconds: number,
  recent: RecentReadings | null = null,
  signIn: SnapshotSignInProfile[] = [],
  signInRenewal: SnapshotSignInRenewal | null = null
): { snapshot: AgentUsageSnapshot; history: AgentUsageHistoryFile } {
  const now = event.sampledAt;
  const projected = projectAgentUsage(event.accounts, { now, includeHidden: true });
  const { rates, series } = usageHistory(
    event.accounts,
    projected.accounts,
    event.history,
    now,
    recent
  );
  // A pace says where usage is heading from now, so it is published only for a current reading: not
  // after a passed reset (the reading is history), not for an account that failed or needs a sign-in,
  // and not for a reading older than the snapshot's own stale limit (a retained or cached one).
  const freshMs = staleAfterSeconds(refreshSeconds) * 1000;
  const current = (account: { status: string }, window: { sampledAt: string | null }): boolean => {
    if (account.status !== 'ok' && account.status !== 'cached') return false;
    const sampled = Date.parse(window.sampledAt ?? '');
    return Number.isFinite(sampled) && now - sampled <= freshMs;
  };
  // With the history store, a current pace also carries the 7- and 30-day burn across resets.
  const longFor = (accountId: string, window: AgentUsageWindow): Partial<BurnRate> => {
    if (!event.longBurn) return {};
    const row = event.accounts.find((candidate) => candidate.id === accountId);
    const source = row?.windows?.find(
      (candidate) =>
        candidate.key === window.key &&
        (candidate.unit ?? null) === window.unit &&
        (candidate.poolId ?? null) === window.poolId
    );
    const long = row && source ? event.longBurn.get(longBurnKey(row, source)) : undefined;
    if (!long) return {};
    const rate = (span: LongBurnSpan): number | null => (span.coverage >= 0.5 ? span.rate : null);
    return {
      last7Days: rate(long.last7Days),
      last30Days: rate(long.last30Days),
      coverage7Days: long.last7Days.coverage,
      coverage30Days: long.last30Days.coverage,
    };
  };
  const accounts: SnapshotAccount[] = projected.accounts.map((account) => ({
    ...account,
    windows: account.windows.map((window) => {
      const rate =
        window.resetPassed || !current(account, window)
          ? null
          : (rates.get(account.id)?.get(burnKey(window.key, window.unit, window.poolId)) ?? null);
      return { ...window, burn: rate ? { ...rate, ...longFor(account.id, window) } : null };
    }),
  }));
  const generatedAt = new Date(now).toISOString();
  return {
    snapshot: {
      version: 1,
      source: 'AI Account Center',
      generatedAt,
      refreshSeconds,
      staleAfterSeconds: staleAfterSeconds(refreshSeconds),
      accounts,
      ...(signIn.length > 0 ? { signIn } : {}),
      ...(signInRenewal ? { signInRenewal } : {}),
    },
    history: { version: 1, generatedAt, hours: HISTORY_HOURS, series },
  };
}

/** Three missed samples, and never under 5 minutes. */
function staleAfterSeconds(refreshSeconds: number): number {
  return Math.max(300, refreshSeconds * 3);
}
