/** Accounts generation inputs for the retained collection publisher. */
import type { RustService } from '../rust-service';
import { localeRanks } from '../rust-read-input';
import { jsonExact } from '../rust-profile-input';
import type { AccountDashboard } from '../services/account-dashboard-types';
import { homeSortWords } from './home-context';
import { BootFeeds, ensure, type Generation } from './feed-publication';

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

const plainObject = (value: object): boolean => {
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
};
const jsonPlain = (value: unknown): boolean =>
  value === null ||
  typeof value === 'string' ||
  typeof value === 'boolean' ||
  (typeof value === 'number' && Number.isFinite(value));
/** An own member the pass reads is absent, or an enumerable plain JSON value. */
const faithfulMember = (object: object, key: string): boolean =>
  !Object.prototype.hasOwnProperty.call(object, key) ||
  (Object.prototype.propertyIsEnumerable.call(object, key) &&
    jsonPlain((object as Record<string, unknown>)[key]));

/**
 * Review L1: the per-response pass (`markPassedResets`) reads each account's `sampledAt` and each window's
 * `resetAt`, `sampledAt` and `resetPassed` from the value itself, while the service reads them from its JSON text.
 * They agree only when those members are plain JSON values on plain objects: an own member JSON drops
 * (`resetPassed: undefined` keeps its place in TS's spread, but Rust would append the flag), changes (a `Date`,
 * a non-finite number, which `??` keeps but JSON writes as `null`) or cannot see (non-enumerable, inherited) is
 * refused, and today's handler answers. Checked on every request: the change key is the text, which cannot tell
 * such a value from a plain one.
 */
export function passFaithful(dashboard: AccountDashboard): boolean {
  for (const account of dashboard.accounts as unknown[]) {
    // Anything but an object with a window list is the service's decline (TS's pass throws there).
    if (!account || typeof account !== 'object' || Array.isArray(account)) continue;
    if (!plainObject(account) || !faithfulMember(account, 'sampledAt')) return false;
    const windows = (account as Record<string, unknown>).windows;
    if (!Array.isArray(windows)) continue;
    for (const window of windows as unknown[]) {
      if (!window || typeof window !== 'object' || Array.isArray(window)) continue;
      if (
        !plainObject(window) ||
        !faithfulMember(window, 'resetAt') ||
        !faithfulMember(window, 'sampledAt') ||
        !faithfulMember(window, 'resetPassed')
      )
        return false;
    }
  }
  return true;
}

/** Shared dashboard bytes and Home's date/sort facts, serialized once for a publication. */
export function dashboardPublication(dashboard: AccountDashboard) {
  if (!jsonExact(dashboard)) return null;
  return {
    data: JSON.stringify({ ...dashboard, updatedAt: '' }),
    facts: JSON.stringify({
      order: localeRanks(homeSortWords(dashboard)),
      epochs: resetEpochs(dashboard),
    }),
  };
}

/** Shared generation for Accounts, trays and Home. */
export function ensureDashboardFeed(
  service: RustService,
  feeds: BootFeeds,
  platform: 'mac' | 'windows',
  secure: boolean,
  dashboard: AccountDashboard
): Promise<Generation> {
  const dashboardText = JSON.stringify({ ...dashboard, updatedAt: '' });
  return ensure(
    service,
    feeds,
    `home-dashboard-${platform}-${secure ? 1 : 0}`,
    dashboardText,
    () => {
      if (!jsonExact(dashboard)) return null;
      return {
        data: dashboardText,
        facts: JSON.stringify({
          order: localeRanks(homeSortWords(dashboard)),
          epochs: resetEpochs(dashboard),
        }),
      };
    }
  );
}
