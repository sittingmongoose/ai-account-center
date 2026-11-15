/**
 * M3 part 3 inputs and reply gates for the account read routes. Inputs carry only what the guarded
 * TS handler already read from its owners; nothing here reads a file, a store or a collector.
 * An input that JSON cannot carry exactly (undefined members, non-finite numbers, class instances)
 * returns null, which makes the bridge log a TS fallback instead of asking Rust.
 */
import type { AccountAnalyticsComposeInput } from './services/account-analytics-compose';
import type { AccountAnalyticsRangeInput } from './services/account-analytics-compose';
import type { RegistryListingFacts } from './services/account-lifecycle-listing';
import { parseVisibilityBody } from './services/account-visibility';
import { isDashboardPreferences } from './services/dashboard-preferences';
import { notePrivateStoreFolder } from './services/private-json-store';
import { rustReadInput } from './rust-read-input';

function object(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}

type Entry = [string, unknown] | [string];

/**
 * An object the composer spreads, as ordered entries: `[key, value]`, or `[key]` for an own
 * member whose value is undefined. JSON drops such members, but a spread keeps their position,
 * so a later assignment to that key keeps its place. Null for anything that is not a plain
 * object of string keys.
 */
function spreadEntries(
  value: unknown,
  nested?: Record<string, (inner: unknown) => unknown>
): Entry[] | null {
  if (!object(value)) return null;
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null) return null;
  const entries: Entry[] = [];
  for (const key of Object.keys(value)) {
    if (key === '__proto__') return null;
    const member = value[key];
    if (member === undefined) entries.push([key]);
    else if (nested?.[key]) {
      const encoded = nested[key](member);
      if (encoded === null) return null;
      entries.push([key, encoded]);
    } else entries.push([key, member]);
  }
  return entries;
}

/** Text, or absent: the only kinds the composer compares or formats as text. */
function textOrAbsent(value: unknown): boolean {
  return value === undefined || value === null || typeof value === 'string';
}

/** A window: identity fields are text, the sample clock is text when present. */
function windowEntries(window: unknown): Entry[] | null {
  if (!object(window)) return null;
  if (
    !textOrAbsent(window.key) ||
    !textOrAbsent(window.kind) ||
    !textOrAbsent(window.unit) ||
    !textOrAbsent(window.poolId) ||
    !textOrAbsent(window.sampledAt)
  )
    return null;
  return spreadEntries(window);
}

function windowList(value: unknown): Entry[][] | null {
  if (!Array.isArray(value)) return null;
  const encoded: Entry[][] = [];
  for (const window of value) {
    const entries = windowEntries(window);
    if (!entries) return null;
    encoded.push(entries);
  }
  return encoded;
}

function finiteOrNull(value: number | undefined): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

/**
 * The Analytics composition input in the shared presentation envelope, or null when JSON cannot
 * carry it exactly. Rust takes `Date.parse`, `toISOString` and the provider-id `localeCompare`
 * ranks from the envelope's facts; it formats no clock or day words, so none are computed.
 */
export function rustAnalyticsInput(facts: {
  rangeInput: AccountAnalyticsRangeInput;
  compose: AccountAnalyticsComposeInput;
}): unknown {
  const { rangeInput, compose } = facts;
  if (rangeInput.oldestQuota !== Infinity && !Number.isFinite(rangeInput.oldestQuota)) return null;
  const coverageOldest = rangeInput.coverageOldestHourAt ?? null;
  if (coverageOldest !== null && !Number.isFinite(coverageOldest)) return null;
  const accounts: Entry[][] = [];
  for (const account of compose.dashboard.accounts as unknown[]) {
    if (
      !object(account) ||
      typeof account.provider !== 'string' ||
      typeof account.id !== 'string' ||
      !textOrAbsent(account.email)
    )
      return null;
    const entries = spreadEntries(account, { windows: windowList });
    if (!entries || !Array.isArray(account.windows)) return null;
    accounts.push(entries);
  }
  let history: unknown = null;
  if (compose.history) {
    const records: unknown[] = [];
    for (const record of compose.history.records as unknown[]) {
      if (
        !object(record) ||
        typeof record.identity !== 'string' ||
        typeof record.sampledAt !== 'string' ||
        typeof record.source !== 'string'
      )
        return null;
      const windows = windowList(record.windows);
      if (!windows) return null;
      records.push({ ...record, windows });
    }
    history = { collectedSince: compose.history.collectedSince, records };
  }
  const oldest = Math.min(rangeInput.oldestQuota, rangeInput.coverageOldestHourAt ?? Infinity);
  const { providers, settings } = compose.dashboard;
  const data = {
    query: compose.query,
    now: rangeInput.now,
    retainedFrom: rangeInput.retainedFrom,
    bounds: rangeInput.bounds,
    oldestQuota: finiteOrNull(rangeInput.oldestQuota),
    coverageOldestHourAt: coverageOldest,
    provisional: rangeInput.provisional,
    todayMidnight: Number.isFinite(oldest) ? null : rangeInput.todayMidnight(),
    expected: { availableFrom: compose.availableFrom, range: compose.range },
    tz: compose.tz,
    resolution: compose.resolution ?? null,
    available: compose.available,
    stored: compose.stored,
    history,
    dashboard: {
      accounts,
      ...(providers !== undefined ? { providers } : {}),
      ...(settings !== undefined ? { settings } : {}),
    },
    providerTable: compose.providerTable,
    providersWithActivity: [...compose.providersWithActivity],
  };
  return rustReadInput(
    data,
    compose.tz,
    compose.providerTable.map((entry) => entry.id),
    { rankAll: false, clockAndDay: false }
  );
}

const ANALYTICS_KEYS = [
  'schemaVersion',
  'updatedAt',
  'range',
  'filters',
  'history',
  'summary',
  'providers',
  'accounts',
];

export function isRustAnalyticsComposed(value: unknown): boolean {
  return (
    object(value) &&
    Object.keys(value).join('\n') === ANALYTICS_KEYS.join('\n') &&
    value.schemaVersion === 1 &&
    typeof value.updatedAt === 'string' &&
    object(value.range) &&
    object(value.filters) &&
    object(value.history) &&
    object(value.summary) &&
    Array.isArray(value.providers) &&
    Array.isArray(value.accounts)
  );
}

/** The registry composition input: each row's view fields and owner facts, never a credential. */
export function rustRegistryInput(facts: RegistryListingFacts): unknown {
  const pick = (source: Record<string, unknown>, keys: string[]) =>
    Object.fromEntries(
      keys
        .filter(
          (key) => Object.prototype.hasOwnProperty.call(source, key) && source[key] !== undefined
        )
        .map((key) => [key, source[key]])
    );
  const rows = facts.rows.map((fact) => {
    const row = pick(fact.row as unknown as Record<string, unknown>, [
      'id',
      'provider',
      'label',
      'email',
      'platform',
      'hidden',
      'trayHidden',
      'lifecycle',
    ]);
    switch (fact.kind) {
      case 'codex':
        return { kind: fact.kind, row, refusal: fact.refusal, available: fact.available };
      case 'claude':
        return {
          kind: fact.kind,
          row,
          claudePlatforms: fact.row.capabilities?.claudePlatforms,
          profile: fact.profile,
          removeAllowed: fact.removeAllowed,
        };
      case 'antigravity':
        return {
          kind: fact.kind,
          row,
          refusal: fact.refusal,
          reason: fact.reason,
          removeAllowed: fact.removeAllowed,
        };
      case 'entry':
        return {
          kind: fact.kind,
          row,
          entry: pick(fact.entry as unknown as Record<string, unknown>, [
            'id',
            'provider',
            'platform',
            'sshHost',
            'label',
          ]),
          credentialKind: fact.entry.credential?.kind,
          key: fact.key
            ? pick(fact.key as unknown as Record<string, unknown>, [
                'last4',
                'fingerprint',
                'storedOn',
              ])
            : null,
          museSignIn: fact.museSignIn,
          running: fact.running,
        };
      case 'other':
        return { kind: fact.kind, row, running: fact.running, walletRemove: fact.walletRemove };
    }
  });
  return { providers: facts.providers, rows, jobs: facts.jobs, trash: facts.trash };
}

export function isRustRegistryListing(value: unknown): boolean {
  return (
    object(value) &&
    Object.keys(value).join('\n') === 'providers\naccounts\njobs\ntrash' &&
    Array.isArray(value.providers) &&
    Array.isArray(value.accounts) &&
    Array.isArray(value.jobs) &&
    Array.isArray(value.trash)
  );
}

/** Rust's visibility reply: the four lists and the one-time folder lines the TS reader logs. */
export function acceptRustVisibility(
  directory: string
): (body: unknown) => { value: unknown; effect: () => void } | null {
  return (body) => {
    if (!object(body) || Object.keys(body).join('\n') !== 'visibility\nfolderWarnings') return null;
    const lists = body.visibility;
    const warnings = body.folderWarnings;
    if (
      !object(lists) ||
      Object.keys(lists).join('\n') !==
        'hiddenProviders\nhiddenAccountIds\ntrayHiddenProviders\ntrayHiddenAccountIds' ||
      !Array.isArray(warnings) ||
      !warnings.every((warning) => warning === 'group-writable')
    )
      return null;
    const parsed = parseVisibilityBody(lists);
    if (!parsed || Object.keys(parsed).length !== 4) return null;
    for (const key of Object.keys(parsed) as Array<keyof typeof parsed>)
      if ((parsed[key] ?? []).length !== (lists[key] as unknown[]).length) return null;
    return {
      value: lists,
      effect: () => {
        for (const warning of warnings) notePrivateStoreFolder(directory, warning);
      },
    };
  };
}

export function acceptRustPreferences(body: unknown): { value: unknown } | null {
  return object(body) &&
    Object.keys(body).join('\n') === 'timeZone\nsnapshotCleanup\nusageLogSources' &&
    isDashboardPreferences(body)
    ? { value: body }
    : null;
}
