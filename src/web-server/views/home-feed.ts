/**
 * R1 (DESIGN-M3-COMPLETE 1.1): Home and the accounts dashboard, fed. The TS owners keep reading and collecting as
 * today; what changes is what crosses to the Rust service and back.
 *
 * - **Pushed only when it changed, with a generation number** (`PUT /internal/feed/<source>`):
 *   - `home-dashboard-<platform>-<0|1>`: AccountDashboardService's answer before its per-response pass, per
 *     platform and secure transport, as `JSON.stringify` writes it with `updatedAt: ""` (that text is the change
 *     key, so it is written once per request and sent only when it differs). Its facts, computed once per
 *     generation: the `localeCompare` ranks of the words Home sorts by and the `Date.parse` epochs of every
 *     `resetAt`/`sampledAt` string the pass reads;
 *   - `home-opens`: this process's Claude Open progress per profile (`ClaudeOpenOperations.forScope`);
 *   - `home-antigravity`: the runtime's inventory in the ISO envelope, which Rust composes when it arrives.
 * - **Per request** only the generation ids, the clock (`now`, `updatedAt`), the platform, the transport fact and
 *   the page's context cross (`x-aac-feed`, `x-aac-home-context`; no body). Rust reads the desktop manifest
 *   itself, applies `markPassedResets`, builds the view and answers the final bytes and `ETag`, which the front
 *   door writes without parsing (`sendRustView`).
 * - **Anything unproven declines** with a named reason and the original handler answers: no service or another
 *   root, a page zone this engine does not agree with, an input JSON cannot carry exactly, a push that did not
 *   land, a generation the service does not hold as named (`stale`), any decline of Rust's.
 *
 * Generations are per service boot: a new boot holds none, so the front door pushes again on the next request.
 */
import type { Request, Response } from 'express';
import type { RustService } from '../rust-service';
import { localeRanks } from '../rust-read-input';
import { jsonExact, rustAntigravityInventoryInput } from '../rust-profile-input';
import { sendRustView } from '../rust-view-reply';
import type { AccountDashboard } from '../services/account-dashboard-types';
import type { HomeDashboardRead } from '../services/account-dashboard-service';
import { getClaudeOpenOperations } from '../services/claude-open-operations';
import { homeSortWords, type HomeContext } from './home-context';
import { homeZoneAgrees } from './home-zone';

const ROUTE = '/api/views/home';
/** aac-views `home::fed::{VIEW, VERSION}`. */
export const HOME_FED_VIEW = 'home@3';

interface Held {
  key: string;
  /** The generation the service holds for `key`; null when the input could not be pushed exactly. */
  generation: string | null;
}

/** R2 (Status) reuses these: the generation a source holds, or why the request declines. */
export type Generation = { generation: string } | { declined: string };

/** What the front door has pushed to one boot. */
export class BootFeeds {
  readonly held = new Map<string, Held>();
  readonly pending = new Map<string, { key: string; done: Promise<Generation> }>();
  constructor(readonly boot: string) {}
}

/**
 * The last generation number this process issued. Numbers start from the clock (ms x 1000), so a front door that
 * attaches to a service an earlier process fed still pushes newer generations, and a push that finds a newer one
 * held moves past it ([`seen`]); both stay safe integers for centuries.
 */
let last = 0;
function nextGeneration(): string {
  last = Math.max(last + 1, Date.now() * 1000);
  return String(last);
}
function seen(held: string): void {
  const n = Number(held);
  if (Number.isSafeInteger(n) && n > last) last = n;
}
const boots = new WeakMap<RustService, BootFeeds>();

function feedsOf(service: RustService, boot: string): BootFeeds {
  let feeds = boots.get(service);
  if (!feeds || feeds.boot !== boot) {
    feeds = new BootFeeds(boot);
    boots.set(service, feeds);
  }
  return feeds;
}

/**
 * The generation the service holds for `source` with content `key`, pushing `build()` first when it does not.
 * Concurrent requests with the same new content share one push. A push that lost a race (the service names a
 * newer generation) or did not land is a decline for this request only.
 */
export function ensure(
  service: RustService,
  feeds: BootFeeds,
  source: string,
  key: string,
  build: () => { data: string; facts: string } | null
): Promise<Generation> {
  const held = feeds.held.get(source);
  if (held && held.key === key)
    return Promise.resolve(
      held.generation === null ? { declined: 'input' } : { generation: held.generation }
    );
  const pending = feeds.pending.get(source);
  if (pending && pending.key === key) return pending.done;
  let built: { data: string; facts: string } | null;
  try {
    built = build();
  } catch {
    built = null;
  }
  if (!built) {
    feeds.held.set(source, { key, generation: null });
    return Promise.resolve({ declined: 'input' });
  }
  const generation = nextGeneration();
  const done = service
    .pushFeed(feeds.boot, source, generation, built.data, built.facts)
    .then((now): Generation => {
      if (now === null) return { declined: 'feed' };
      if (now !== generation) {
        // Another push holds a newer generation: this request is today's; the next one pushes past it.
        seen(now);
        return { declined: 'stale' };
      }
      // A late reply of an older push never replaces what a newer one recorded.
      const current = feeds.held.get(source)?.generation;
      if (current === undefined || current === null || Number(current) < Number(generation))
        feeds.held.set(source, { key, generation });
      return { generation };
    })
    .finally(() => {
      if (feeds.pending.get(source)?.done === done) feeds.pending.delete(source);
    });
  feeds.pending.set(source, { key, done });
  return done;
}

/** Every string `markPassedResets` hands to `Date.parse` (window `resetAt`/`sampledAt`, account `sampledAt`). */
export function resetEpochs(dashboard: AccountDashboard): Record<string, number | null> {
  const strings = new Set<string>();
  const add = (value: unknown) => {
    if (typeof value === 'string') strings.add(value);
  };
  for (const account of dashboard.accounts as unknown[]) {
    if (!account || typeof account !== 'object') continue;
    const row = account as Record<string, unknown>;
    add(row.sampledAt);
    for (const window of Array.isArray(row.windows) ? (row.windows as unknown[]) : []) {
      if (!window || typeof window !== 'object') continue;
      add((window as Record<string, unknown>).resetAt);
      add((window as Record<string, unknown>).sampledAt);
    }
  }
  return Object.fromEntries(
    [...strings].map((text) => {
      const ms = Date.parse(text);
      return [text, Number.isFinite(ms) ? ms : null];
    })
  );
}

export interface HomeFedRequest {
  platform: 'mac' | 'windows';
  secure: boolean;
  /** The page's context as `parseHomeContext` read it; null: the answer has no view, as today. */
  context: HomeContext | null;
  /** The dashboard before its per-response pass, `server.views` included, and that pass's clock. */
  read: HomeDashboardRead;
  /** The Antigravity runtime's inventory, as the original handler reads it. */
  inventory: unknown;
  /** The CCS directory the TS readers use (`getCcsDir()`). */
  scope: string;
}

/**
 * Answer GET /api/views/home from fed generations. True when the Rust answer was written; false when the request
 * declined (counted with its reason) and the original handler must answer, with nothing written.
 */
export async function answerHomeFed(
  req: Request,
  res: Response,
  service: RustService,
  input: HomeFedRequest
): Promise<boolean> {
  const declined = (reason: string): false => {
    service.fedDeclined(ROUTE, reason);
    return false;
  };
  const boot = service.feedBoot(input.scope);
  if (!boot) return declined('root');
  const dashboard = input.read.dashboard;
  if (typeof dashboard.updatedAt !== 'string' || !Array.isArray(dashboard.accounts))
    return declined('input');
  if (input.context && !homeZoneAgrees(input.context)) return declined('zone');
  const feeds = feedsOf(service, boot);
  const dashboardText = JSON.stringify({ ...dashboard, updatedAt: '' });
  const opens = JSON.stringify(getClaudeOpenOperations().forScope(input.scope));
  const antigravity = JSON.stringify(rustAntigravityInventoryInput(input.inventory) ?? null);
  const generations = await Promise.all([
    ensure(
      service,
      feeds,
      `home-dashboard-${input.platform}-${input.secure ? 1 : 0}`,
      dashboardText,
      () =>
        jsonExact(dashboard)
          ? {
              data: dashboardText,
              facts: JSON.stringify({
                order: localeRanks(homeSortWords(dashboard)),
                epochs: resetEpochs(dashboard),
              }),
            }
          : null
    ),
    ensure(service, feeds, 'home-opens', opens, () => ({ data: opens, facts: 'null' })),
    ensure(service, feeds, 'home-antigravity', antigravity, () => ({
      data: antigravity,
      facts: 'null',
    })),
  ]);
  const ids: string[] = [];
  for (const generation of generations) {
    if ('declined' in generation) return declined(generation.declined);
    ids.push(generation.generation);
  }
  const headers: Record<string, string> = {
    'x-aac-feed': JSON.stringify({
      platform: input.platform,
      secure: input.secure,
      now: input.read.now,
      updatedAt: dashboard.updatedAt,
      dashboard: ids[0],
      opens: ids[1],
      antigravity: ids[2],
    }),
  };
  if (input.context)
    headers['x-aac-home-context'] = encodeURIComponent(JSON.stringify(input.context));
  const reply = await service.fedView(req, ROUTE, headers, HOME_FED_VIEW);
  if (typeof reply === 'string') {
    // The service no longer holds what this front door recorded (a push raced, or it lost its feeds): push
    // again on the next request instead of declining until the content changes.
    if (reply === 'stale') feeds.held.clear();
    return false;
  }
  if (!reply) return false;
  sendRustView(res, reply, 'no-store');
  return true;
}
