/**
 * R1 (Home, fed): the front door's zone check. On the fed path Rust writes every clock and day word from the page's
 * own zone offsets (`HomeContext.zone`), so no server word crosses per request. What D7's envelope still gave is a
 * guard: the server engine's words had to agree with the page's offsets, so a page whose facts disagree with its
 * zone (or an engine whose zone data differs) never got a view drawn in another zone. This keeps that guard as a
 * fact computed once per zone and day: the offsets this engine finds for the page's zone over the page's own window
 * (view-model.mjs `homeZoneFacts`, the same algorithm and constants) must be exactly the page's, or the request
 * declines `zone` and the original handler answers.
 */
import type { HomeContext, HomeZone } from './home-context';

const ZONE_DAY = 86_400_000;
const ZONE_STEP = 6 * 3_600_000;
/** view-model.mjs `HOME_ZONE_DAYS`; tests/unit/web-server/home-feed.test.ts binds the two. */
export const HOME_ZONE_DAYS = { past: 120, future: 400 } as const;
/** Zone-days kept (oldest first out). */
const KEEP = 32;

const known = new Map<string, string | null>();

/** view-model.mjs `homeZoneFacts(timeZone, now)` with this engine's `Intl`, or null when it cannot read the zone. */
export function serverZoneFacts(timeZone: string, now: number): HomeZone | null {
  let parts: Intl.DateTimeFormat;
  try {
    parts = new Intl.DateTimeFormat('en-US', {
      timeZone,
      hourCycle: 'h23',
      year: 'numeric',
      month: 'numeric',
      day: 'numeric',
      hour: 'numeric',
      minute: 'numeric',
      second: 'numeric',
    });
  } catch {
    return null;
  }
  const offset = (ms: number): number => {
    const p = Object.fromEntries(parts.formatToParts(ms).map(({ type, value }) => [type, value]));
    const local = Date.UTC(
      Number(p.year),
      Number(p.month) - 1,
      Number(p.day),
      Number(p.hour) % 24,
      Number(p.minute),
      Number(p.second)
    );
    return Math.round((local - Math.floor(ms / 1000) * 1000) / 60_000);
  };
  const day = Math.floor(now / ZONE_DAY) * ZONE_DAY;
  const from = day - HOME_ZONE_DAYS.past * ZONE_DAY;
  const to = day + HOME_ZONE_DAYS.future * ZONE_DAY;
  const offsets: [number, number][] = [[from, offset(from)]];
  for (let t = from; t < to;) {
    const current = offsets[offsets.length - 1][1];
    const next = Math.min(t + ZONE_STEP, to);
    if (offset(next) === current) {
      t = next;
      continue;
    }
    let lo = t / 1000;
    let hi = next / 1000;
    while (hi - lo > 1) {
      const mid = Math.floor((lo + hi) / 2);
      if (offset(mid * 1000) === current) lo = mid;
      else hi = mid;
    }
    offsets.push([hi * 1000, offset(hi * 1000)]);
    t = hi * 1000;
  }
  return { from, to, offsets };
}

/** True when the page's zone facts are exactly the ones this engine computes for its zone and day (cached). */
export function homeZoneAgrees(ctx: HomeContext): boolean {
  const key = `${ctx.timeZone}|${Math.floor(ctx.now / ZONE_DAY)}`;
  let facts = known.get(key);
  if (facts === undefined) {
    let computed: HomeZone | null = null;
    try {
      computed = serverZoneFacts(ctx.timeZone, ctx.now);
    } catch {
      computed = null;
    }
    facts = computed ? JSON.stringify(computed) : null;
    known.set(key, facts);
    while (known.size > KEEP) {
      const oldest = known.keys().next().value;
      if (oldest === undefined) break;
      known.delete(oldest);
    }
  }
  return facts !== null && facts === JSON.stringify(ctx.zone);
}
