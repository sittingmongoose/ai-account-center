/**
 * Codex dashboard row goldens for the native collector's renewal overlay: the saved
 * login's renewal note, the active-login suppression (`activated?.name`) and the
 * `needsReauth` status the note reads (`codexAccount` in account-dashboard-projection.ts).
 *
 * The Rust row builder (`crates/aac-http/src/account_usage/rows.rs`, `codex`) and the
 * producer's note decision (`renewal_row_note`) are checked byte for byte against these
 * cases by `cargo test -p aac-http` on Nas1 (see `producer::tests`).
 *
 * Every case stays inside what the native collector can produce: an `ok`/`cached` row with
 * windows, a `needsReauth` row, or a parked (no login) row. The TS-only `error` status
 * (quotaStatus error with no reauth flag) is not producible natively, so no case uses it.
 *
 *   bun tests/spec/codex-renewal-row-parity.ts               (verify against the golden)
 *   bun tests/spec/codex-renewal-row-parity.ts --regenerate  (rewrite it; review the diff)
 */
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  CODEX_RENEWAL_MESSAGES,
  type CodexRenewalReason,
  type CodexRenewalState,
} from '../../src/codex-auth/codex-renewal-types';
import type { CodexProfileRenewalProfileStatus } from '../../src/codex-auth/codex-profile-renewal';
import { codexAccount } from '../../src/web-server/services/account-dashboard-projection';
import type { BarSummaryRow } from '../../src/web-server/services/account-dashboard-types';

const repo = fileURLToPath(new URL('../..', import.meta.url));
const GOLDEN = path.join(repo, 'crates/aac-http/tests/fixtures/codex-renewal-rows.json');
const NOW = Date.parse('2026-10-01T12:00:00Z');
const FETCHED = '2026-10-01T11:55:00.000Z';

const WINDOWS = [
  {
    key: 'five_hour',
    label: '5h',
    usedPercent: 30,
    remainingPercent: 70,
    resetAt: '2026-10-01T15:00:00.000Z',
    windowMinutes: 300,
  },
];

interface Input {
  /** Fixture case name; the saved profile is always `gmail` unless a case changes it. */
  case: string;
  name: string;
  active: boolean;
  authValid: boolean;
  needsReauth: boolean;
  quotaStatus: 'ok' | 'error';
  cached: boolean;
  quotaSource: 'network' | 'local';
  email: string | null;
  plan: string | null;
  fetchedAt: string;
  sampledAt: string;
  windows: unknown[];
  renewal: { state: CodexRenewalState; reason: CodexRenewalReason; accessExpiresAt?: string } | null;
}

const base = (over: Partial<Input> = {}): Input => ({
  case: 'row',
  name: 'gmail',
  active: false,
  authValid: true,
  needsReauth: false,
  quotaStatus: 'ok',
  cached: false,
  quotaSource: 'network',
  email: 'gmail@example.com',
  plan: 'pro',
  fetchedAt: FETCHED,
  sampledAt: FETCHED,
  windows: WINDOWS,
  renewal: null,
  ...over,
});

const entry = (
  state: CodexRenewalState,
  reason: CodexRenewalReason,
  accessExpiresAt?: string
): NonNullable<Input['renewal']> => ({ state, reason, accessExpiresAt });

const cases: Input[] = [
  base({ case: 'note-failed-dead', renewal: entry('failed', 'dead') }),
  base({ case: 'note-failed-identity', renewal: entry('failed', 'identity_mismatch') }),
  base({
    case: 'note-needs-reauth-due',
    needsReauth: true,
    quotaStatus: 'error',
    windows: [],
    renewal: entry('due', 'due'),
  }),
  base({
    case: 'note-needs-reauth-renewing',
    needsReauth: true,
    quotaStatus: 'error',
    windows: [],
    renewal: entry('renewing', 'renewing'),
  }),
  base({
    case: 'note-needs-reauth-fresh-quiet',
    needsReauth: true,
    quotaStatus: 'error',
    windows: [],
    renewal: entry('ok', 'fresh'),
  }),
  base({
    case: 'note-retry-within-48h',
    renewal: entry('retrying', 'transient', '2026-10-02T08:00:00.000Z'),
  }),
  base({
    case: 'note-retry-past-expiry',
    renewal: entry('retrying', 'transient', '2026-09-30T00:00:00.000Z'),
  }),
  base({
    case: 'note-retry-far-from-expiry',
    renewal: entry('retrying', 'transient', '2026-10-03T12:01:00.000Z'),
  }),
  base({
    case: 'note-stuck-shared-family',
    renewal: entry('skipped', 'shared_family', '2026-10-04T12:00:00.000Z'),
  }),
  base({
    case: 'note-stuck-in-use',
    renewal: entry('skipped', 'in_use', '2026-10-04T12:00:00.000Z'),
  }),
  base({
    case: 'note-self-clearing-activation-busy',
    renewal: entry('skipped', 'activation_busy', '2026-10-02T12:00:00.000Z'),
  }),
  base({ case: 'active-suppresses-note', active: true, renewal: entry('failed', 'dead') }),
  base({
    case: 'live-reason-suppresses-note',
    renewal: entry('skipped', 'live', '2026-10-02T12:00:00.000Z'),
  }),
  base({
    case: 'cached-row-keeps-note',
    cached: true,
    renewal: entry('retrying', 'transient', '2026-10-02T08:00:00.000Z'),
  }),
  base({ case: 'local-quota-source' }),
  base({ case: 'parked-no-login', authValid: false, email: null, plan: null, windows: [] }),
  // A saved login copied into a custom `.codex*`/process home: the planner marks it shared
  // and the row names why renewal leaves it alone.
  base({
    case: 'custom-active-home-shared',
    renewal: entry('skipped', 'shared_family', '2026-10-02T08:00:00.000Z'),
  }),
];

function rowOf(input: Input): BarSummaryRow {
  const row: Partial<BarSummaryRow> = {
    profile: input.name,
    provider: 'codex',
    quotaStatus: input.needsReauth ? 'error' : input.quotaStatus,
    cached: input.cached,
    needsReauth: input.needsReauth,
    quotaSource: input.quotaSource,
    quotaWindows: input.windows as BarSummaryRow['quotaWindows'],
    balanceWindows: [],
    fetchedAt: input.fetchedAt,
    ...(input.sampledAt === input.fetchedAt ? {} : { staleAsOf: input.sampledAt }),
  };
  return row as BarSummaryRow;
}

function renewalOf(input: Input): CodexProfileRenewalProfileStatus | undefined {
  if (!input.renewal) return undefined;
  return {
    name: input.name,
    state: input.renewal.state,
    reason: input.renewal.reason,
    message: CODEX_RENEWAL_MESSAGES[input.renewal.reason],
    lastRenewedAt: null,
    lastAttemptAt: null,
    lastOutcome: null,
    nextAttemptAt: null,
    accessExpiresAt: input.renewal.accessExpiresAt ?? null,
  } as CodexProfileRenewalProfileStatus;
}


const casesOut = cases.map((input) => {
  const profile = {
    name: input.name,
    email: input.email,
    plan: input.plan,
    accountId: null,
    lastUsed: null,
    authValid: input.authValid,
    codexHome: '/fixture/saved',
  };
  const activated = input.active
    ? { name: input.name, email: input.email ?? 'active@example.com', plan: 'pro', codexHome: '/fixture/live' }
    : null;
  const realNow = Date.now;
  Date.now = () => NOW;
  try {
    const row = codexAccount(
      profile as never,
      activated as never,
      rowOf(input),
      renewalOf(input)
    );
    const { case: name, ...rest } = input;
    return { name, input: rest, row };
  } finally {
    Date.now = realNow;
  }
});

const fixture = { version: 1, now: NOW, cases: casesOut };
const text = `${JSON.stringify(fixture, null, 2)}\n`;
if (process.argv.includes('--regenerate')) {
  fs.mkdirSync(path.dirname(GOLDEN), { recursive: true });
  fs.writeFileSync(GOLDEN, text);
  console.log(`wrote ${path.relative(repo, GOLDEN)} (${casesOut.length} cases)`);
} else {
  const current = fs.existsSync(GOLDEN) ? fs.readFileSync(GOLDEN, 'utf8') : '';
  if (current !== text) {
    console.error(
      `${path.relative(repo, GOLDEN)} differs from the TS row; rerun with --regenerate and review the diff.`
    );
    process.exit(1);
  }
  console.log(`TS Codex renewal rows match the golden (${casesOut.length} cases)`);
}
