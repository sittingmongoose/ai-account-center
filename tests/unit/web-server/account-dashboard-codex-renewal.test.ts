import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import type { CodexAuthProfilesSummary } from '../../../src/codex-auth/codex-auth-dashboard-service';
import {
  CODEX_RENEWAL_MESSAGES,
  type CodexRenewalReason,
  type CodexRenewalState,
} from '../../../src/codex-auth/codex-renewal-types';
import type { CodexProfileRenewalProfileStatus } from '../../../src/codex-auth/codex-profile-renewal';
import type { BarSummaryRow } from '../../../src/web-server/routes/bar-routes';
import { codexAccount } from '../../../src/web-server/services/account-dashboard-projection';
import { renewalEntry } from './codex-renewal-entry-fixture';

const NOW = Date.parse('2026-10-01T12:00:00Z');
const ASCII = /^[\x20-\x7e]*$/;
const RETRY_NOTE = `${CODEX_RENEWAL_MESSAGES.transient} The saved login expires`;

type Profile = CodexAuthProfilesSummary['profiles'][number];

const profile: Profile = {
  name: 'gmail',
  email: 'gmail@example.com',
  plan: 'pro',
  accountId: 'private-workspace-sentinel',
  codexHome: '/private-saved-path',
  lastUsed: null,
  authValid: true,
};

const otherActive: NonNullable<CodexAuthProfilesSummary['activated']> = {
  name: 'party',
  email: 'party@example.com',
  plan: 'pro',
  codexHome: '/private-live-path',
};

function row(overrides: Partial<BarSummaryRow> = {}): BarSummaryRow {
  return {
    profile: 'gmail',
    provider: 'codex',
    account_id: 'private-quota-account-sentinel',
    displayName: 'gmail',
    tier: 'pro',
    paused: false,
    quota_percentage: 70,
    quotaStatus: 'ok',
    next_reset: null,
    is_default: false,
    last_activity_at: null,
    today_cost: null,
    health: 'ok',
    cached: false,
    fetchedAt: '2026-10-01T11:55:00Z',
    needsReauth: false,
    quotaSource: 'network',
    quotaWindows: [
      {
        key: 'five_hour',
        label: '5h',
        usedPercent: 30,
        remainingPercent: 70,
        resetAt: '2026-10-01T15:00:00Z',
        windowMinutes: 300,
      },
    ],
    ...overrides,
  };
}

function renewal(
  state: CodexRenewalState,
  reason: CodexRenewalReason,
  overrides: Partial<CodexProfileRenewalProfileStatus> = {}
): CodexProfileRenewalProfileStatus {
  return renewalEntry('gmail', state, reason, overrides);
}

beforeEach(() => {
  spyOn(Date, 'now').mockReturnValue(NOW);
});

afterEach(() => {
  mock.restore();
});

describe('Codex dashboard rows and saved-login renewal', () => {
  it('shows a rejected saved login as needing sign in, while keeping its quota windows', () => {
    const account = codexAccount(profile, null, row(), renewal('failed', 'dead'));
    expect(account).toMatchObject({
      status: 'needs_sign_in',
      message: CODEX_RENEWAL_MESSAGES.dead,
    });
    expect(account.windows).toHaveLength(1);
  });

  it('names an identity mismatch the same way', () => {
    const account = codexAccount(profile, null, row(), renewal('failed', 'identity_mismatch'));
    expect(account).toMatchObject({
      status: 'needs_sign_in',
      message: CODEX_RENEWAL_MESSAGES.identity_mismatch,
    });
  });

  it('says renewal is under way for a 401 whose saved login is due or renewing', () => {
    const expired = row({ quotaStatus: 'error', needsReauth: true });
    for (const state of ['due', 'renewing'] as const) {
      expect(codexAccount(profile, null, expired, renewal(state, state))).toMatchObject({
        status: 'needs_sign_in',
        message: 'The saved login expired. AAC is renewing it automatically.',
      });
    }
  });

  it('keeps the usual 401 message when no renewal is due', () => {
    const expired = row({ quotaStatus: 'error', needsReauth: true });
    expect(codexAccount(profile, null, expired, renewal('ok', 'fresh'))).toMatchObject({
      status: 'needs_sign_in',
      message: 'This saved Codex login needs to be renewed.',
    });
    expect(codexAccount(profile, null, expired).message).toBe(
      'This saved Codex login needs to be renewed.'
    );
  });

  it('adds the expiry to a retry once the saved login is within 48 hours of expiring', () => {
    const account = codexAccount(
      profile,
      null,
      row(),
      renewal('retrying', 'transient', { accessExpiresAt: '2026-10-02T08:00:00Z' })
    );
    expect(account).toMatchObject({
      status: 'ok',
      message: `${RETRY_NOTE} 2026-10-02 08:00 UTC.`,
    });
  });

  it('keeps the status of a retry row and names an expiry that is already past', () => {
    const account = codexAccount(
      profile,
      null,
      row({ quotaStatus: 'error' }),
      renewal('retrying', 'transient', { accessExpiresAt: '2026-09-30T00:00:00Z' })
    );
    expect(account).toMatchObject({
      status: 'error',
      message: `${RETRY_NOTE} 2026-09-30 00:00 UTC.`,
    });
  });

  it('leaves a retry alone while the saved login is more than 48 hours from expiry', () => {
    const account = codexAccount(
      profile,
      null,
      row(),
      renewal('retrying', 'transient', { accessExpiresAt: '2026-10-03T12:01:00Z' })
    );
    expect(account).toMatchObject({ status: 'ok', message: null });
  });

  it('names a skipped login that renewal cannot clear once it is within four days of expiry', () => {
    const reasons = [
      'shared_family',
      'family_unverifiable',
      'unverifiable',
      'in_use',
      'process_scan_failed',
      'live_unverifiable',
      'unsafe_file',
    ] as const;
    for (const reason of reasons) {
      const account = codexAccount(
        profile,
        null,
        row(),
        renewal('skipped', reason, { accessExpiresAt: '2026-10-04T12:00:00Z' })
      );
      expect(account).toMatchObject({ status: 'ok', message: CODEX_RENEWAL_MESSAGES[reason] });
    }
  });

  it('stays quiet about skips outside that window or that renewal clears by itself', () => {
    const farFromExpiry = renewal('skipped', 'shared_family', {
      accessExpiresAt: '2026-10-06T12:00:00Z',
    });
    const soonButSelfClearing = [
      renewal('skipped', 'activation_busy', { accessExpiresAt: '2026-10-02T12:00:00Z' }),
      renewal('skipped', 'changed', { accessExpiresAt: '2026-10-02T12:00:00Z' }),
      renewal('skipped', 'disabled', { accessExpiresAt: '2026-10-02T12:00:00Z' }),
    ];
    expect(codexAccount(profile, null, row(), farFromExpiry).message).toBeNull();
    for (const entry of soonButSelfClearing) {
      expect(codexAccount(profile, null, row(), entry).message).toBeNull();
    }
  });

  it('never adds a renewal note to the login Codex is using', () => {
    const active = codexAccount(
      profile,
      { ...otherActive, name: 'gmail' },
      row(),
      renewal('failed', 'dead')
    );
    expect(active).toMatchObject({ status: 'ok', message: null, isActive: true });

    const live = codexAccount(
      profile,
      otherActive,
      row(),
      renewal('skipped', 'live', { accessExpiresAt: '2026-10-02T12:00:00Z' })
    );
    expect(live).toMatchObject({ status: 'ok', message: null, isActive: false });
  });

  it("keeps today's row when no renewal entry exists for the profile", () => {
    expect(codexAccount(profile, otherActive, row())).toMatchObject({
      status: 'ok',
      message: null,
    });
  });

  it('builds every note from a fixed ASCII message, never from stored content', () => {
    const notes = [
      codexAccount(profile, null, row(), renewal('failed', 'dead')),
      codexAccount(
        profile,
        null,
        row(),
        renewal('retrying', 'transient', { accessExpiresAt: '2026-10-02T08:00:00Z' })
      ),
      codexAccount(
        profile,
        null,
        row(),
        renewal('skipped', 'in_use', { accessExpiresAt: '2026-10-02T08:00:00Z' })
      ),
    ];
    for (const account of notes) {
      expect(account.message).toMatch(ASCII);
      expect(account.message).not.toMatch(/token|secret|sk-/i);
    }
  });
});
