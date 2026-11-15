// Stands in for view-model.mjs while generate.mjs records the Home builds the JS tests make (HOME_SPEC_RECORD).
import { appendFileSync } from 'node:fs';
import * as real from '../../../../web-dashboard/public/view-model.mjs';
import { getDisplayTimeZone } from '../../../../web-dashboard/public/time-format.mjs';

export * from '../../../../web-dashboard/public/view-model.mjs';

const clone = (value) => (value === undefined ? undefined : JSON.parse(JSON.stringify(value)));

export function dashboardViewModel(data, ctx = {}) {
  const file = process.env.HOME_SPEC_RECORD;
  const open = ctx.openProgress instanceof Map && ctx.openProgress.size > 0;
  // Home as the server builds it: no Details (includeHidden), no refresh in flight, no Open the page runs itself.
  if (file && data && typeof data === 'object' && !ctx.includeHidden && !ctx.refreshing && !open) {
    try {
      appendFileSync(
        file,
        `${JSON.stringify({
          source: process.env.HOME_SPEC_SOURCE || '',
          dashboard: clone(data),
          profiles: Array.isArray(ctx.profiles) ? clone(ctx.profiles) : [],
          antigravity: clone(ctx.antigravityInventory) ?? null,
          ctx: {
            now: ctx.now ?? Date.now(),
            timeZone: getDisplayTimeZone(),
            platform: ctx.platform || 'mac',
            intervalSeconds: ctx.intervalSeconds ?? 60,
            username: ctx.username ?? '',
            host: ctx.host ?? '',
          },
        })}\n`
      );
    } catch {
      /* an input JSON cannot carry is not a case */
    }
  }
  return real.dashboardViewModel(data, ctx);
}
