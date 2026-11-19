/**
 * G14: the Analytics writers' feed producers. When `rustService.groups.analytics-writers` is on, the
 * Rust service owns quota sampling and activity collection; the front door feeds it the inputs it
 * reads — the coalesced dashboards, the activity plan (computers, kinds, roots), the resolved pricing
 * facts, and the control switch — and stands its own writers down. With the switch off nothing here
 * runs and the TypeScript writers keep working unchanged.
 *
 * Pricing resolution stays here (the models.dev registry, CCS static prices and policy overrides the
 * front door owns): the facts feed carries the resolved rates the Rust aggregation applies. Root
 * discovery and fingerprinting ride in the plan feed until their own ports land.
 */
import { createHash } from 'node:crypto';
import type { RustService } from '../rust-service';
import { BootFeeds, ensure } from '../views/home-feed';
import { getCcsDir } from '../../utils/config-manager';
import { rustAnalyticsWritersOn } from '../rust-service-groups';
import { getAccountRefreshIntervalSeconds } from './account-refresh-settings';
import { getAccountDashboard } from './account-dashboard-service';
import { analyticsWriterPlan } from './analytics-remote-sources';
import { getModelPricingWithSource } from '../model-pricing';
import { getCachedModelsDevRegistry } from '../models-dev/registry-cache';
import { normalizeUsageProvider } from '../usage/model-identity';

const boots = new Map<string, BootFeeds>();
let timer: ReturnType<typeof setTimeout> | undefined;
let running = false;
let pricingCache: { key: string; facts: Record<string, unknown> } | null = null;

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

/** The resolved pricing facts the Rust aggregation replays: every registry model under its
 * normalized provider and the no-provider form, plus the unknown-model fallback the reader uses. */
function pricingFacts(): Record<string, unknown> {
  const registry = getCachedModelsDevRegistry();
  const key = registry ? contentKey(Object.keys(registry).sort()) : 'none';
  if (pricingCache && pricingCache.key === key) return pricingCache.facts;
  const facts: Record<string, unknown> = {};
  const put = (model: string, provider: string | undefined): void => {
    const resolved = getModelPricingWithSource(model, { provider });
    facts[`${model}\u0000${provider ?? ''}`] = {
      rates: {
        input: resolved.pricing.inputPerMillion,
        output: resolved.pricing.outputPerMillion,
        cacheCreation: resolved.pricing.cacheCreationPerMillion,
        cacheRead: resolved.pricing.cacheReadPerMillion,
      },
      source: resolved.source,
    };
  };
  if (registry)
    for (const [providerId, provider] of Object.entries(registry)) {
      const normalized = normalizeUsageProvider(providerId);
      const models = (provider as { models?: Record<string, unknown> }).models ?? {};
      for (const model of Object.keys(models)) {
        put(model, normalized);
        put(model, undefined);
      }
    }
  pricingCache = { key, facts };
  return facts;
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
  const facts = pricingFacts();
  await ensure(service, feeds, 'analytics-pricing', contentKey(facts), () => ({
    data: JSON.stringify({ facts }),
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