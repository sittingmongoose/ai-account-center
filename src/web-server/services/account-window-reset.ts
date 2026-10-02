import type { DashboardAccount, DashboardAccountWindow } from './account-dashboard-types';

/**
 * True when a window's reset is at or before `now` and its reading was sampled
 * before that reset, or at an unknown time. Such a reading describes the window
 * before its reset: it is history, not the current state, and never means 0%.
 */
export function readingPredatesReset(
  resetAt: string | null | undefined,
  sampledAt: string | null | undefined,
  now: number
): boolean {
  if (typeof resetAt !== 'string') return false;
  const reset = Date.parse(resetAt);
  if (!Number.isFinite(reset) || reset > now) return false;
  const sampled = typeof sampledAt === 'string' ? Date.parse(sampledAt) : Number.NaN;
  return !Number.isFinite(sampled) || sampled < reset;
}

/**
 * Mark every window whose reset has passed since its reading with `resetPassed: true`.
 * The reading itself stays in place as history; clients show "Reset at <time> · new
 * reading pending". Computed per response, because a reset can pass while a sample
 * is still cached (the Claude live cache keeps readings for up to 24 hours).
 */
export function markPassedResets<
  W extends DashboardAccountWindow,
  A extends Pick<DashboardAccount, 'sampledAt'> & { windows: W[] },
>(account: A, now: number): A {
  let changed = false;
  const windows = account.windows.map((window): W => {
    const passed = readingPredatesReset(window.resetAt, window.sampledAt ?? account.sampledAt, now);
    if (passed === (window.resetPassed === true)) return window;
    changed = true;
    if (passed) return { ...window, resetPassed: true };
    const { resetPassed: _stale, ...current } = window;
    return current as W;
  });
  return changed ? { ...account, windows } : account;
}
