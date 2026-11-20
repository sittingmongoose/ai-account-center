import { getAacStore, type LongBurnSpan } from '../../store/store-service';
import { readNativeCollection, workerReadsNative } from '../native-collection-feed';
import type { DashboardAccount, DashboardAccountWindow } from '../services/account-dashboard-types';
import {
  accountAnalyticsIdentity,
  analyticsWindowIdentity,
  type AccountAnalyticsHistoryData,
} from '../services/account-analytics-history';

const DAY = 86_400_000;

export interface AccountAnalyticsLongBurn {
  last7Days: LongBurnSpan;
  last30Days: LongBurnSpan;
}

export interface AccountAnalyticsSampleEvent {
  accounts: DashboardAccount[];
  history: AccountAnalyticsHistoryData | null;
  sampledAt: number;
  longBurn?: Map<string, AccountAnalyticsLongBurn>;
}

export function longBurnKey(account: DashboardAccount, window: DashboardAccountWindow): string {
  return JSON.stringify([accountAnalyticsIdentity(account), analyticsWindowIdentity(window)]);
}

export type AccountAnalyticsSampleListener = (event: AccountAnalyticsSampleEvent) => unknown;
const listeners = new Set<AccountAnalyticsSampleListener>();

export function onAccountAnalyticsSample(listener: AccountAnalyticsSampleListener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

async function readStoreFacts(event: AccountAnalyticsSampleEvent): Promise<void> {
  const store = getAacStore();
  if (!store) return;
  const now = event.sampledAt;
  try {
    const records = await store.historyRecords({
      fromMs: Math.max(0, now - 7 * DAY),
      toMs: now,
      nowMs: now,
      thinning: { kind: 'history' },
    });
    event.history = {
      schemaVersion: 1,
      collectedSince: records[0]?.sampledAt ?? new Date(now).toISOString(),
      records,
    };
  } catch {
    // The publisher reports missing history while the store read is unavailable.
  }
  const refs = event.accounts.flatMap((account) =>
    (account.windows ?? [])
      .filter(
        (window) =>
          typeof window.usedPercent === 'number' && (window.kind ?? 'rate_limit') === 'rate_limit'
      )
      .map((window) => ({
        identity: accountAnalyticsIdentity(account),
        window: analyticsWindowIdentity(window),
        key: longBurnKey(account, window),
      }))
  );
  if (!refs.length) return;
  try {
    const answer = await store.burnLong(
      refs.map(({ identity, window }) => ({ identity, window })),
      [7 * DAY, 30 * DAY],
      now
    );
    const longBurn = new Map<string, AccountAnalyticsLongBurn>();
    answer.forEach(({ spans: [last7Days, last30Days] }, index) => {
      const ref = refs[index];
      if (ref && last7Days && last30Days) longBurn.set(ref.key, { last7Days, last30Days });
    });
    event.longBurn = longBurn;
  } catch {
    // A temporarily unavailable burn answer leaves that span unknown.
  }
}

/** Relay the native sample to the remaining share publishers. The relay only reads. */
export function startNativeUsageSamples(): () => void {
  if (!workerReadsNative() || process.platform !== 'win32') return () => {};
  const stopped = new AbortController();
  let last = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  async function next(): Promise<void> {
    try {
      if (!listeners.size) return;
      const { sampledAt, accounts } = (await readNativeCollection('/sample', stopped.signal)) as {
        sampledAt: number;
        accounts: DashboardAccount[];
      };
      if (!Number.isSafeInteger(sampledAt) || sampledAt <= last || !Array.isArray(accounts)) return;
      const event: AccountAnalyticsSampleEvent = { sampledAt, accounts, history: null };
      await readStoreFacts(event);
      if (stopped.signal.aborted) return;
      last = sampledAt;
      for (const listener of listeners)
        void Promise.resolve()
          .then(() => listener(event))
          .catch(() => {});
    } catch {
      // The producer may have no completed sample yet.
    } finally {
      if (!stopped.signal.aborted) {
        timer = setTimeout(() => void next(), 2500);
        timer.unref();
      }
    }
  }
  void next();
  return () => {
    stopped.abort();
    clearTimeout(timer);
  };
}
