/**
 * G14: the Analytics writers' feed producers. When `rustService.groups.analytics-writers` is on, the
 * Rust service owns quota sampling and activity collection; the front door feeds it the inputs it
 * reads — the coalesced dashboards, the activity plan (computers, kinds, roots) and the control
 * switch — and stands its own writers down. With the switch off nothing here runs and the
 * TypeScript writers keep working unchanged.
 *
 * Pricing is resolved in Rust (G15's `Resolver`, fed by the scheduled `ModelsDevRefresh`), so no
 * pricing feed is pushed. Root discovery and fingerprinting ride in the plan feed until ported.
 */
import { createHash } from 'node:crypto';
import type { RustService } from '../rust-service';
import { BootFeeds, ensure } from '../views/home-feed';
import { getCcsDir } from '../../utils/config-manager';
import { rustAnalyticsWritersOn } from '../rust-service-groups';
import { getAccountRefreshIntervalSeconds } from './account-refresh-settings';
import { getAccountDashboard } from './account-dashboard-service';
import { analyticsWriterPlan } from './analytics-remote-sources';

const boots = new Map<string, BootFeeds>();
let timer: ReturnType<typeof setTimeout> | undefined;
let running = false;

function contentKey(value: unknown): string {
  return createHash('sha256').update(JSON.stringify(value)).digest('hex');
}

function feedsOf(boot: string): BootFeeds {
  let feeds = boots.get(boot);
  if (!feeds) {
    feeds = new BootFeeds(boot);
    boots.set(boot, feeds);
  }
  return feeds;
}

async function pushOnce(service: RustService): Promise<void> {
  const enabled = rustAnalyticsWritersOn();
  const boot = service.feedBoot(getCcsDir());
  if (!boot) return;
  const feeds = feedsOf(boot);
  await ensure(service, feeds, 'analytics-writers', String(enabled), () => ({
    data: JSON.stringify({ enabled }),
    facts: '{}',
  }));
  if (!enabled) return;
  const settled = await Promise.allSettled(
    (['mac', 'windows'] as const).map((platform) => getAccountDashboard(platform, false))
  );
  const dashboards = settled
    .filter((r): r is PromiseFulfilledResult<Awaited<ReturnType<typeof getAccountDashboard>>> =>
      r.status === 'fulfilled'
    )
    .map((r) => r.value);
  await ensure(service, feeds, 'analytics-dashboard', contentKey(dashboards), () => ({
    data: JSON.stringify({ dashboards }),
    facts: '{}',
  }));
  const plan = analyticsWriterPlan();
  await ensure(service, feeds, 'analytics-plan', contentKey(plan), () => ({
    data: JSON.stringify(plan),
    facts: '{}',
  }));
}

/** Start the writers' feed loop on the account-refresh cadence (idempotent). */
export function startAnalyticsWriterFeeds(service: RustService): void {
  if (running) return;
  running = true;
  const tick = (): void => {
    if (!running) return;
    void pushOnce(service)
      .catch(() => undefined)
      .finally(() => {
        if (!running) return;
        const interval = Math.min(
          3_600_000,
          Math.max(30_000, getAccountRefreshIntervalSeconds() * 1000)
        );
        timer = setTimeout(tick, interval);
        timer.unref?.();
      });
  };
  tick();
}

/** Stop the feed loop (shutdown, or the writers switching off). */
export function stopAnalyticsWriterFeeds(): void {
  running = false;
  clearTimeout(timer);
  timer = undefined;
}