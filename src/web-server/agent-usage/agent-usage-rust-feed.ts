/**
 * G18 strangler wiring: when `rustService.groups.usageSnapshot` is on (and the service mode is
 * on), the Rust service owns the aac-usage share writer. The front door pushes every finished
 * analytics sample to the service's `/internal/usage-snapshot/sample`, carrying the sign-in,
 * renewal and rollup facts the TypeScript publisher's deps would have fetched. The TypeScript
 * publisher stands down; off keeps it. The Rust publisher keeps the same guards, atomic writes,
 * staleness and publish-time deadline recheck, so the files stay byte-equal.
 *
 * Every sample is pushed, even one whose write the service skips, so the service's one-minute
 * reading ring counts it exactly as the in-process ring does. While no folder is set (or there
 * are no accounts) the sign-in probes never start — the same guard the TypeScript publisher
 * applies — and only the readings travel, not the history.
 */
import {
  onAccountAnalyticsSample,
  type AccountAnalyticsSampleEvent,
} from '../services/account-analytics-service';
import { getSignInHealth } from '../services/signin-health-service';
import { readClaudeSignInRenewal } from '../services/claude-signin-renewal';
import { getAacStore } from '../../store/store-service';
import { buildRollupExports } from '../../store/usage-exports';
import { projectAgentUsage } from './agent-usage-projection';
import { readSnapshotSettings } from './agent-usage-settings';
import type { RustService } from '../rust-service';

/** The sign-in fetch waits at most this long, as in the TypeScript publisher. */
const SIGNIN_FETCH_LIMIT_MS = 60_000;

/** Subscribes the Rust snapshot feed to every analytics sample; returns the unsubscribe call. */
export function startRustUsageSnapshotFeed(rustService: RustService): () => void {
  return onAccountAnalyticsSample((event) => {
    void pushSample(rustService, event).catch(() => {
      /* A push never fails a sample; the transport tells the supervisor. */
    });
  });
}

async function pushSample(
  rustService: RustService,
  event: AccountAnalyticsSampleEvent
): Promise<void> {
  const settings = await readSnapshotSettings().catch(() => null);
  const active = settings !== null && event.accounts.length > 0;
  let signIn: { rows: unknown[]; failed: boolean; since: number } = {
    rows: [],
    failed: false,
    since: Date.now(),
  };
  let signInRenewal: unknown = null;
  let rollups: unknown = null;
  if (active) {
    const since = Date.now();
    signIn = await fetchSignIn(since);
    signInRenewal = await fetchRenewal();
    rollups = await fetchRollups(event);
  }
  const payload = JSON.stringify({
    event: {
      accounts: event.accounts,
      history: active ? event.history : null,
      sampledAt: event.sampledAt,
      longBurn: active && event.longBurn ? [...event.longBurn.entries()] : null,
    },
    signIn,
    signInRenewal,
    rollups,
  });
  await rustService.pushUsageSample(payload);
}

/** The sign-in expiry rows, under the publisher's 60-second limit; a failure is named, not empty. */
async function fetchSignIn(
  since: number
): Promise<{ rows: unknown[]; failed: boolean; since: number }> {
  const fetched = await Promise.race([
    getSignInHealth().then(
      (health) => ({ rows: Object.values(health.machines).flat() as unknown[], failed: false }),
      () => ({ rows: [] as unknown[], failed: true })
    ),
    new Promise<{ rows: unknown[]; failed: boolean }>((resolve) => {
      const timer = setTimeout(() => resolve({ rows: [], failed: true }), SIGNIN_FETCH_LIMIT_MS);
      timer.unref?.();
    }),
  ]);
  return { ...fetched, since };
}

/** The renewal runner's summary, best effort, as the publisher's dep reads it. */
async function fetchRenewal(): Promise<unknown> {
  try {
    const renewal = await readClaudeSignInRenewal();
    return renewal
      ? {
          runner: renewal.runner,
          generatedAt: renewal.generatedAt,
          stale: renewal.stale,
          needsYou: renewal.needsYou,
        }
      : null;
  } catch {
    return null;
  }
}

/** The store's hourly and daily rollup files, or null without the store; a failure retries next sample. */
async function fetchRollups(event: AccountAnalyticsSampleEvent): Promise<unknown> {
  try {
    const store = getAacStore();
    if (!store) return null;
    const projected = projectAgentUsage(event.accounts, {
      now: event.sampledAt,
      includeHidden: true,
    });
    const files = await buildRollupExports(
      store,
      event.accounts,
      projected.accounts,
      event.sampledAt
    );
    return { hourly: files.hourly, daily: files.daily };
  } catch {
    return { failed: true };
  }
}
