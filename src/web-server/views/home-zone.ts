/** Display-zone offsets for the retained Status feed. */
import type { HomeZone } from './home-context';

const ZONE_DAY = 86_400_000;
const ZONE_STEP = 6 * 3_600_000;
/** Matches view-model.mjs `HOME_ZONE_DAYS`. */
export const HOME_ZONE_DAYS = { past: 120, future: 400 } as const;
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
