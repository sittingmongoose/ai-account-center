import { describe, expect, it } from 'bun:test';
import {
  markPassedResets,
  readingPredatesReset,
} from '../../../src/web-server/services/account-window-reset';
import type { DashboardAccountWindow } from '../../../src/web-server/services/account-dashboard-types';

const RESET = '2026-10-01T15:00:00.000Z';
const AT_RESET = Date.parse(RESET);

function window(overrides: Partial<DashboardAccountWindow> = {}): DashboardAccountWindow {
  return {
    key: 'five_hour',
    label: '5-hour limit',
    usedPercent: 91.25,
    remainingPercent: 8.75,
    resetAt: RESET,
    windowMinutes: 300,
    used: null,
    limit: null,
    unit: null,
    ...overrides,
  };
}

describe('reset-passed readings (review finding F6)', () => {
  it('treats a reading as history from the reset instant when it was sampled before it', () => {
    expect(readingPredatesReset(RESET, '2026-10-01T12:00:00Z', AT_RESET - 1)).toBe(false);
    expect(readingPredatesReset(RESET, '2026-10-01T12:00:00Z', AT_RESET)).toBe(true);
    expect(readingPredatesReset(RESET, '2026-10-01T14:59:59.999Z', AT_RESET + 86_400_000)).toBe(
      true
    );
  });

  it('keeps a reading sampled at or after the reset, and windows without a usable reset', () => {
    expect(readingPredatesReset(RESET, RESET, AT_RESET + 1)).toBe(false);
    expect(readingPredatesReset(RESET, '2026-10-01T15:00:01Z', AT_RESET + 60_000)).toBe(false);
    expect(readingPredatesReset(null, '2026-10-01T12:00:00Z', AT_RESET)).toBe(false);
    expect(readingPredatesReset(undefined, null, AT_RESET)).toBe(false);
    expect(readingPredatesReset('not a time', null, AT_RESET)).toBe(false);
  });

  it('treats a reading of unknown age after its reset as history', () => {
    expect(readingPredatesReset(RESET, null, AT_RESET)).toBe(true);
    expect(readingPredatesReset(RESET, undefined, AT_RESET)).toBe(true);
    expect(readingPredatesReset(RESET, 'not a time', AT_RESET)).toBe(true);
  });

  it('adds only the marker and keeps every reading field, never inventing 0%', () => {
    const account = {
      sampledAt: '2026-10-01T12:00:00.000Z',
      windows: [window(), window({ key: 'seven_day', resetAt: '2026-10-05T00:00:00.000Z' })],
    };
    const marked = markPassedResets(account, AT_RESET);
    expect(marked.windows[0]).toEqual({ ...window(), resetPassed: true });
    expect(marked.windows[1]).toBe(account.windows[1]);
    expect(account.windows[0].resetPassed).toBeUndefined();
  });

  it("prefers a retained window's own sample time over the account's", () => {
    const account = {
      sampledAt: '2026-10-01T15:30:00.000Z',
      windows: [
        window({ status: 'cached', sampledAt: '2026-10-01T14:00:00.000Z' }),
        window({ key: 'seven_day' }),
      ],
    };
    const marked = markPassedResets(account, AT_RESET + 3_600_000);
    expect(marked.windows[0].resetPassed).toBe(true);
    expect(marked.windows[1].resetPassed).toBeUndefined();
  });

  it('returns the same object when nothing changes and clears a mark that no longer holds', () => {
    const current = { sampledAt: '2026-10-01T15:30:00.000Z', windows: [window()] };
    expect(markPassedResets(current, AT_RESET + 3_600_000)).toBe(current);
    const stale = { sampledAt: current.sampledAt, windows: [window({ resetPassed: true })] };
    const cleared = markPassedResets(stale, AT_RESET + 3_600_000);
    expect(cleared.windows[0]).toEqual(window());
    expect('resetPassed' in cleared.windows[0]).toBe(false);
  });
});
