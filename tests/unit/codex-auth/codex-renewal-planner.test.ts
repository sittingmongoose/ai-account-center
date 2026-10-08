import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as path from 'path';
import {
  collectCodexRenewalSnapshot,
  planCodexProfileRenewals,
  type CodexRenewalPlanEntry,
} from '../../../src/codex-auth/codex-renewal-planner';
import { dryRunCodexProfileRenewal } from '../../../src/codex-auth/codex-profile-renewal';
import {
  ALPHA,
  BRAVO,
  CHARLIE,
  DAY,
  NOW,
  RenewalSandbox,
  codexProcess,
  expectNoSecrets,
  type LoginSpec,
} from './codex-renewal-fixtures';

let box: RenewalSandbox;

beforeEach(() => {
  box = new RenewalSandbox();
});

afterEach(() => {
  box.cleanup();
});

function plan(): Record<string, CodexRenewalPlanEntry> {
  const snapshot = collectCodexRenewalSnapshot(box.options());
  return Object.fromEntries(
    planCodexProfileRenewals(snapshot, box.clock).map((entry) => [entry.name, entry])
  );
}

const alpha = (overrides: Partial<LoginSpec> = {}): LoginSpec => ({
  account: ALPHA,
  refresh: 'rt-FAKE-alpha-1',
  label: 'alpha1',
  session: 'sess-alpha-family',
  authTime: 1_780_000_100,
  ...overrides,
});

describe('due threshold', () => {
  it('is fresh with more than 4 days of access left and names the due time', () => {
    box.addProfile('alpha', alpha({ accessExp: NOW + 5 * DAY }));
    const entry = plan().alpha;
    expect(entry.decision).toBe('fresh');
    expect(entry.reason).toBe('fresh');
    expect(entry.accessExpiresAt).toBe(new Date(NOW + 5 * DAY).toISOString());
    expect(entry.dueAt).toBe(new Date(NOW + DAY).toISOString());
    expect(entry.nextAttemptAt).toBe(entry.dueAt);
  });

  it('is due with 4 days or less left', () => {
    box.addProfile('alpha', alpha({ accessExp: NOW + 4 * DAY - 1000 }));
    expect(plan().alpha.decision).toBe('due');
  });

  it('is due when the access token already expired (the refresh token may still work)', () => {
    box.addProfile('alpha', alpha({ accessExp: NOW - 2 * DAY }));
    const entry = plan().alpha;
    expect(entry.decision).toBe('due');
    expect(Date.parse(entry.accessExpiresAt ?? '')).toBeLessThan(NOW);
  });

  it('falls back to last_refresh + 6 days without a decodable expiry', () => {
    box.addProfile(
      'alpha',
      alpha({ accessExp: null, lastRefresh: new Date(NOW - 5 * DAY).toISOString() })
    );
    box.addProfile('bravo', {
      account: BRAVO,
      refresh: 'rt-FAKE-bravo-1',
      label: 'bravo1',
      accessExp: null,
      lastRefresh: new Date(NOW - 7 * DAY).toISOString(),
    });
    const entries = plan();
    expect(entries.alpha.decision).toBe('fresh');
    expect(entries.alpha.dueAt).toBe(new Date(NOW + DAY).toISOString());
    expect(entries.bravo.decision).toBe('due');
  });

  it('moves the due time earlier for a manual lead, clamped to the 10-day token', () => {
    box.addProfile('alpha', alpha({ accessExp: NOW + 5 * DAY }));
    const planWith = (renewWithinMs: number) =>
      planCodexProfileRenewals(
        collectCodexRenewalSnapshot({ ...box.options(), renewWithinMs }),
        box.clock
      )[0];
    expect(planWith(6 * DAY).decision).toBe('due');
    // A shorter lead than the default never delays renewal.
    expect(planWith(DAY).dueAt).toBe(new Date(NOW + DAY).toISOString());
    expect(planWith(30 * DAY).dueAt).toBe(new Date(NOW - 5 * DAY).toISOString());
  });

  it('keeps every guard for a manual lead', () => {
    box.addProfile('alpha', alpha({ accessExp: NOW + 5 * DAY }));
    box.writeLive(alpha({ refresh: 'rt-FAKE-alpha-live', label: 'alphalive', session: 's2' }));
    const entry = planCodexProfileRenewals(
      collectCodexRenewalSnapshot({ ...box.options(), renewWithinMs: 9 * DAY }),
      box.clock
    )[0];
    expect(entry.reason).toBe('live');
  });
});

describe('guards', () => {
  it('skips every saved copy of the live account, not only the one the dashboard names', () => {
    box.addProfile('alpha', alpha({ accessExp: NOW - DAY }));
    box.addProfile(
      'alpha-old',
      alpha({
        refresh: 'rt-FAKE-alpha-old',
        label: 'alphaold',
        session: 'sess-other',
        authTime: 1_700_000_000,
        accessExp: NOW - DAY,
      })
    );
    box.addProfile('bravo', {
      account: BRAVO,
      refresh: 'rt-FAKE-bravo',
      label: 'bravo',
      accessExp: NOW - DAY,
    });
    box.writeLive(
      alpha({
        refresh: 'rt-FAKE-alpha-live',
        label: 'alphalive',
        session: 'sess-live',
        authTime: 1_790_000_000,
      })
    );
    const entries = plan();
    expect(entries.alpha.reason).toBe('live');
    expect(entries['alpha-old'].reason).toBe('live');
    expect(entries.bravo.decision).toBe('due');
  });

  it('skips all profiles while the live login cannot be read', () => {
    box.addProfile('alpha', alpha({ accessExp: NOW - DAY }));
    box.writeLive('{"tokens": {"id_token": ');
    expect(plan().alpha.reason).toBe('live_unverifiable');
  });

  it('ignores an API-key-only live login', () => {
    box.addProfile('alpha', alpha({ accessExp: NOW - DAY }));
    box.writeLive(JSON.stringify({ OPENAI_API_KEY: 'sk-FAKE', auth_mode: 'apikey', tokens: null }));
    expect(plan().alpha.decision).toBe('due');
  });

  it('skips a profile sharing its refresh token with another Codex home', () => {
    box.addProfile('alpha', alpha({ accessExp: NOW - DAY }));
    box.writeHome(
      '.codex-copy',
      alpha({ label: 'alphacopy', session: 'sess-unrelated', authTime: 1 })
    );
    const entries = plan();
    expect(entries.alpha.reason).toBe('shared_family');
  });

  it('skips a profile whose family (session id) lives on in a T3-style child home', async () => {
    box.addProfile('gmail', alpha({ accessExp: NOW - DAY }));
    // The copy was refreshed elsewhere: different refresh token and sign-in time, same session.
    box.writeHome(
      '.codex-t3/gmail',
      alpha({ refresh: 'rt-FAKE-alpha-rotated', label: 't3gmail', authTime: 1_780_999_999 })
    );
    expect(plan().gmail.reason).toBe('shared_family');
    const dry = await dryRunCodexProfileRenewal(box.options());
    const guards = dry.profiles[0].guards;
    expect(guards.sharesFamily).toBe(true);
    expect(guards.sharedWith).toEqual([
      {
        path: path.join(box.homeDir, '.codex-t3', 'gmail', 'auth.json'),
        kind: 'codex_home',
        matchedBy: ['session'],
      },
    ]);
  });

  it('skips the same account signed in at the same time (auth_time) without session ids', () => {
    box.addProfile('alpha', alpha({ accessExp: NOW - DAY }));
    box.writeHome(
      '.codex-t3/party',
      alpha({ refresh: 'rt-FAKE-alpha-t3', label: 't3', session: null })
    );
    expect(plan().alpha.reason).toBe('shared_family');
  });

  it('renews alongside an independent login of the same account (different session and sign-in)', () => {
    box.addProfile('alpha', alpha({ accessExp: NOW - DAY }));
    box.writeHome(
      '.codex-t3/lexx',
      alpha({
        refresh: 'rt-FAKE-alpha-t3',
        label: 't3b',
        session: 'sess-t3-own',
        authTime: 1_781_000_000,
      })
    );
    box.writeHome('.codex-t3/other', {
      account: CHARLIE,
      refresh: 'rt-FAKE-charlie',
      label: 'charlie',
      session: 'sess-alpha-family-not',
      authTime: 1_780_000_100,
    });
    expect(plan().alpha.decision).toBe('due');
  });

  it('finds a family copy through a running process CODEX_HOME outside the home folder', () => {
    box.addProfile('alpha', alpha({ accessExp: NOW - DAY }));
    const elsewhere = path.join(box.root, 'elsewhere-home');
    fs.mkdirSync(elsewhere);
    fs.writeFileSync(
      path.join(elsewhere, 'auth.json'),
      JSON.stringify({ tokens: { refresh_token: 'rt-FAKE-alpha-1' } })
    );
    box.processes = [codexProcess(elsewhere)];
    expect(plan().alpha.reason).toBe('shared_family');
  });

  it('skips a profile whose folder a running Codex uses as CODEX_HOME', () => {
    box.addProfile('alpha', alpha({ accessExp: NOW - DAY }));
    box.processes = [codexProcess(path.dirname(box.profileAuth('alpha')))];
    expect(plan().alpha.reason).toBe('in_use');
  });

  it('skips everything when another Codex login cannot be read', () => {
    box.addProfile('alpha', alpha({ accessExp: NOW - DAY }));
    box.writeHome('.codex-broken', '{not json');
    expect(plan().alpha.reason).toBe('family_unverifiable');
  });

  it('skips when a same-account copy carries no family markers', () => {
    box.addProfile('alpha', alpha({ accessExp: NOW - DAY }));
    box.writeHome(
      '.codex-old',
      alpha({ refresh: 'rt-FAKE-alpha-old', label: 'old', session: null, authTime: null })
    );
    expect(plan().alpha.reason).toBe('family_unverifiable');
  });

  it('skips a saved login without session id or sign-in time as unverifiable', () => {
    box.addProfile('alpha', alpha({ accessExp: NOW - DAY, session: null, authTime: null }));
    expect(plan().alpha.reason).toBe('unverifiable');
  });

  it('skips when the process scan fails, and relies on files when it is unavailable', () => {
    box.addProfile('alpha', alpha({ accessExp: NOW - DAY }));
    const failing = collectCodexRenewalSnapshot(
      box.options({
        scanProcesses: () => {
          throw new Error('scan failed');
        },
      })
    );
    expect(planCodexProfileRenewals(failing, NOW)[0].reason).toBe('process_scan_failed');
    const unavailable = collectCodexRenewalSnapshot(box.options({ scanProcesses: () => null }));
    expect(planCodexProfileRenewals(unavailable, NOW)[0].decision).toBe('due');
  });

  it('reports a missing login and refuses a linked auth file', () => {
    box.registry.createProfile('empty');
    box.addProfile('alpha', alpha());
    fs.mkdirSync(path.dirname(box.profileAuth('linked')), { recursive: true });
    fs.symlinkSync(box.profileAuth('alpha'), box.profileAuth('linked'));
    box.registry.createProfile('linked');
    const entries = plan();
    expect(entries.empty.reason).toBe('no_login');
    expect(entries.linked.reason).toBe('unsafe_file');
  });
});

describe('dry run', () => {
  it('reports guards, family paths and same-account profiles without secrets or writes', async () => {
    box.addProfile('alpha', alpha({ accessExp: NOW - DAY }));
    box.addProfile(
      'alpha-two',
      alpha({
        refresh: 'rt-FAKE-alpha-two',
        label: 'alphatwo',
        session: 'sess-two',
        authTime: 1_790_000_000,
        accessExp: NOW + 6 * DAY,
      })
    );
    box.writeLive({ account: BRAVO, refresh: 'rt-FAKE-bravo-live', label: 'bravolive' });
    box.writeHome('.codex-t3/gmail', alpha({ refresh: 'rt-FAKE-alpha-t3', label: 't3gm' }));
    const before = box.files();
    const dry = await dryRunCodexProfileRenewal(box.options());
    expect(box.files()).toEqual(before);
    expectNoSecrets(dry);
    const byName = Object.fromEntries(dry.profiles.map((profile) => [profile.name, profile]));
    expect(byName.alpha.reason).toBe('shared_family');
    expect(byName.alpha.sameAccountProfiles).toEqual(['alpha-two']);
    expect(byName.alpha.hasSessionId).toBe(true);
    expect(byName['alpha-two'].decision).toBe('fresh');
    expect(dry.sources.map((source) => source.kind).sort()).toEqual([
      'codex_home',
      'live',
      'profile',
      'profile',
    ]);
    expect(dry.processScan).toBe('ok');
  });
});
