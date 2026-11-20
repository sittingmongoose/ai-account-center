/**
 * G10 saved-login renewal parity cases: the TS behaviour the Rust port must
 * match, as sandbox specs of FAKE credentials only (the same fake JWT algorithm
 * as tests/unit/codex-auth/codex-renewal-fixtures.ts). The checker
 * (renewal-parity.ts) expands each spec on both sides, runs the real TS
 * functions and the Rust runner, and compares the normalized answers byte for
 * byte. `--regenerate` writes the checked-in goldens
 * (crates/aac-renewal/tests/fixtures/renewal-parity.json).
 *
 * Every case mirrors a captured TS test; no provider calls, no real homes.
 */

export const NOW = Date.parse('2026-10-08T12:00:00.000Z');
export const DAY = 86_400_000;
const MIN = 60_000;
const HOUR = 60 * MIN;
export const CLAUDE_NOW = Date.parse('2026-11-03T12:00:00Z');
export const NOTE_NOW = Date.parse('2026-10-01T12:00:00Z');

const iso = (ms: number) => new Date(ms).toISOString();

/** A fake login spec (manifest spelling; the checker maps it to LoginSpec). */
export interface MLogin {
  account: string | { email: string; account_id: string; user_id: string };
  refresh: string;
  label: string;
  session?: string | null;
  auth_time?: number | null;
  access_exp?: number | null;
  last_refresh?: string;
  access_account_id?: string;
}

export type MLoginOrRaw = MLogin | { raw: string };

export interface MCase {
  name: string;
  kind: string;
  sandbox?: {
    profiles?: { name: string; login: MLoginOrRaw }[];
    registryOnly?: string[];
    live?: MLoginOrRaw | null;
    homes?: { rel: string; login: MLoginOrRaw }[];
    processes?: { home?: string | null; pid?: number }[];
    records?: unknown;
    cliproxy?: { folder: string; name: string; login: MLoginOrRaw }[];
    extraFiles?: { rel: string; raw: string }[];
    lock?: boolean;
  };
  inputs?: Record<string, unknown>;
}

const alpha = (overrides: Partial<MLogin> = {}): MLogin => ({
  account: 'alpha',
  refresh: 'rt-FAKE-alpha-1',
  label: 'alpha1',
  session: 'sess-alpha-family',
  auth_time: 1_780_000_100,
  ...overrides,
});

const alphaOld: MLogin = {
  account: 'alpha',
  refresh: 'rt-FAKE-alpha-old',
  label: 'alphaold',
  session: 'sess-alpha',
  auth_time: 1_780_000_100,
  access_exp: NOW - DAY,
};
const alphaNew: MLogin = {
  ...alphaOld,
  refresh: 'rt-FAKE-alpha-rotated',
  label: 'alphanew',
  access_exp: NOW + 10 * DAY,
};
const bravo: MLogin = {
  account: 'bravo',
  refresh: 'rt-FAKE-bravo',
  label: 'bravo',
  access_exp: NOW + 8 * DAY,
};
const bravoLive: MLogin = {
  ...bravo,
  refresh: 'rt-FAKE-bravo-live',
  label: 'bravolive',
  session: 'sess-bravo-live',
};
const signin = (label: string, session: string): MLogin => ({
  ...alphaOld,
  refresh: 'rt-FAKE-alpha-signin',
  label,
  session,
});

/** The renewal test's base sandbox: alpha due, bravo fresh and live. */
const renewalBase = () => ({
  profiles: [
    { name: 'alpha', login: alphaOld },
    { name: 'bravo', login: bravo },
  ],
  live: bravoLive,
});

const ok = (spec: MLogin = alphaNew, include?: { id?: boolean; refresh?: boolean }) => ({
  spec,
  ...(include?.id === false ? { include_id: false } : {}),
  ...(include?.refresh === false ? { include_refresh: false } : {}),
});

const deadBodies = [
  { status: 401, body: { error: { code: 'refresh_token_reused' } } },
  { status: 401, body: { error: 'refresh_token_expired' } },
  { status: 401, body: { error: { code: 'refresh_token_invalidated' } } },
  { status: 400, body: { error: 'invalid_grant' } },
];

function claudeFile(overrides: Record<string, unknown> = {}) {
  const at = (ms: number) => iso(ms).replace('.000Z', 'Z');
  return {
    version: 1,
    runner: 'mac',
    runnerVersion: '1.0.0',
    generatedAt: at(CLAUDE_NOW - 5 * MIN),
    staleAfterSeconds: 3600,
    renewWithinHours: 96,
    needsYou: [
      {
        email: 'A@Example.com',
        reason: 'captcha',
        how: 'Solve the CAPTCHA.',
        since: at(CLAUDE_NOW),
      },
    ],
    hosts: [
      { host: 'mac', readAt: at(CLAUDE_NOW), error: null },
      { host: 'windows', readAt: at(CLAUDE_NOW - HOUR), error: 'ssh_timeout' },
    ],
    signIns: [
      {
        host: 'mac',
        profile: 'gmail',
        email: 'a@example.com',
        state: 'ok',
        signInEndsAt: at(CLAUDE_NOW + 40 * 24 * HOUR),
        logoutAt: at(CLAUDE_NOW + 40 * 24 * HOUR + 3 * HOUR),
        renewFrom: at(CLAUDE_NOW + 36 * 24 * HOUR),
        autoRenew: true,
        lastRenewAt: at(CLAUDE_NOW - HOUR),
        lastResult: 'renewed',
        nextVerifyAt: at(CLAUDE_NOW + 8 * HOUR),
        retryAfter: null,
        reverts: 0,
        readAt: at(CLAUDE_NOW),
      },
      {
        host: 'windows',
        profile: 'gmail',
        email: 'a@example.com',
        state: 'ok',
        lastResult: 'end_not_moved',
      },
    ],
    ...overrides,
  };
}

const at = (ms: number) => iso(ms).replace('.000Z', 'Z');

export function renewalParityCases(): MCase[] {
  const cases: MCase[] = [];
  const add = (c: MCase) => cases.push(c);

  // ---- planner: due threshold (codex-renewal-planner.test.ts) ----
  add({
    name: 'plan-fresh-names-due-time',
    kind: 'plan',
    sandbox: { profiles: [{ name: 'alpha', login: alpha({ access_exp: NOW + 5 * DAY }) }] },
  });
  add({
    name: 'plan-due-with-four-days-left',
    kind: 'plan',
    sandbox: { profiles: [{ name: 'alpha', login: alpha({ access_exp: NOW + 4 * DAY - 1000 }) }] },
  });
  add({
    name: 'plan-due-when-already-expired',
    kind: 'plan',
    sandbox: { profiles: [{ name: 'alpha', login: alpha({ access_exp: NOW - 2 * DAY }) }] },
  });
  add({
    name: 'plan-fallback-last-refresh-age',
    kind: 'plan',
    sandbox: {
      profiles: [
        { name: 'alpha', login: alpha({ access_exp: null, last_refresh: iso(NOW - 5 * DAY) }) },
        {
          name: 'bravo',
          login: {
            account: 'bravo',
            refresh: 'rt-FAKE-bravo-1',
            label: 'bravo1',
            access_exp: null,
            last_refresh: iso(NOW - 7 * DAY),
          },
        },
      ],
    },
  });
  for (const [lead, suffix] of [
    [6 * DAY, '6d'],
    [DAY, '1d'],
    [30 * DAY, '30d'],
  ] as const) {
    add({
      name: `plan-manual-lead-${suffix}`,
      kind: 'plan',
      sandbox: { profiles: [{ name: 'alpha', login: alpha({ access_exp: NOW + 5 * DAY }) }] },
      inputs: { renewWithinMs: lead },
    });
  }
  add({
    name: 'plan-manual-lead-keeps-guards',
    kind: 'plan',
    sandbox: {
      profiles: [{ name: 'alpha', login: alpha({ access_exp: NOW + 5 * DAY }) }],
      live: alpha({ refresh: 'rt-FAKE-alpha-live', label: 'alphalive', session: 's2' }),
    },
    inputs: { renewWithinMs: 9 * DAY },
  });

  // ---- planner: guards ----
  add({
    name: 'plan-live-skips-every-saved-copy',
    kind: 'plan',
    sandbox: {
      profiles: [
        { name: 'alpha', login: alpha({ access_exp: NOW - DAY }) },
        {
          name: 'alpha-old',
          login: alpha({
            refresh: 'rt-FAKE-alpha-old',
            label: 'alphaold',
            session: 'sess-other',
            auth_time: 1_700_000_000,
            access_exp: NOW - DAY,
          }),
        },
        {
          name: 'bravo',
          login: {
            account: 'bravo',
            refresh: 'rt-FAKE-bravo',
            label: 'bravo',
            access_exp: NOW - DAY,
          },
        },
      ],
      live: alpha({
        refresh: 'rt-FAKE-alpha-live',
        label: 'alphalive',
        session: 'sess-live',
        auth_time: 1_790_000_000,
      }),
    },
  });
  add({
    name: 'plan-live-unreadable',
    kind: 'plan',
    sandbox: {
      profiles: [{ name: 'alpha', login: alpha({ access_exp: NOW - DAY }) }],
      live: { raw: '{"tokens": {"id_token": ' },
    },
  });
  add({
    name: 'plan-live-api-key-only',
    kind: 'plan',
    sandbox: {
      profiles: [{ name: 'alpha', login: alpha({ access_exp: NOW - DAY }) }],
      live: {
        raw: JSON.stringify({ OPENAI_API_KEY: 'sk-FAKE', auth_mode: 'apikey', tokens: null }),
      },
    },
  });
  add({
    name: 'plan-shared-refresh-token',
    kind: 'plan',
    sandbox: {
      profiles: [{ name: 'alpha', login: alpha({ access_exp: NOW - DAY }) }],
      homes: [
        {
          rel: '.codex-copy',
          login: alpha({ label: 'alphacopy', session: 'sess-unrelated', auth_time: 1 }),
        },
      ],
    },
  });
  add({
    name: 'dry-run-shared-session-child-home',
    kind: 'dryRun',
    sandbox: {
      profiles: [{ name: 'gmail', login: alpha({ access_exp: NOW - DAY }) }],
      homes: [
        {
          rel: '.codex-t3/gmail',
          login: alpha({
            refresh: 'rt-FAKE-alpha-rotated',
            label: 't3gmail',
            auth_time: 1_780_999_999,
          }),
        },
      ],
    },
  });
  add({
    name: 'plan-shared-auth-time',
    kind: 'plan',
    sandbox: {
      profiles: [{ name: 'alpha', login: alpha({ access_exp: NOW - DAY }) }],
      homes: [
        {
          rel: '.codex-t3/party',
          login: alpha({ refresh: 'rt-FAKE-alpha-t3', label: 't3', session: null }),
        },
      ],
    },
  });
  add({
    name: 'plan-independent-same-account',
    kind: 'plan',
    sandbox: {
      profiles: [{ name: 'alpha', login: alpha({ access_exp: NOW - DAY }) }],
      homes: [
        {
          rel: '.codex-t3/lexx',
          login: alpha({
            refresh: 'rt-FAKE-alpha-t3',
            label: 't3b',
            session: 'sess-t3-own',
            auth_time: 1_781_000_000,
          }),
        },
        {
          rel: '.codex-t3/other',
          login: {
            account: 'charlie',
            refresh: 'rt-FAKE-charlie',
            label: 'charlie',
            session: 'sess-alpha-family-not',
            auth_time: 1_780_000_100,
          },
        },
      ],
    },
  });
  add({
    name: 'plan-process-home-copy',
    kind: 'plan',
    sandbox: {
      profiles: [{ name: 'alpha', login: alpha({ access_exp: NOW - DAY }) }],
      processes: [{ home: 'elsewhere-home' }],
      extraFiles: [
        {
          rel: 'elsewhere-home/auth.json',
          raw: JSON.stringify({ tokens: { refresh_token: 'rt-FAKE-alpha-1' } }),
        },
      ],
    },
  });
  add({
    name: 'plan-in-use',
    kind: 'plan',
    sandbox: {
      profiles: [{ name: 'alpha', login: alpha({ access_exp: NOW - DAY }) }],
      processes: [{ home: '.ccs/codex-instances/alpha' }],
    },
  });
  add({
    name: 'plan-family-unverifiable-broken',
    kind: 'plan',
    sandbox: {
      profiles: [{ name: 'alpha', login: alpha({ access_exp: NOW - DAY }) }],
      homes: [{ rel: '.codex-broken', login: { raw: '{not json' } }],
    },
  });
  add({
    name: 'plan-family-unverifiable-markerless',
    kind: 'plan',
    sandbox: {
      profiles: [{ name: 'alpha', login: alpha({ access_exp: NOW - DAY }) }],
      homes: [
        {
          rel: '.codex-old',
          login: alpha({
            refresh: 'rt-FAKE-alpha-old',
            label: 'old',
            session: null,
            auth_time: null,
          }),
        },
      ],
    },
  });
  add({
    name: 'plan-unverifiable-self',
    kind: 'plan',
    sandbox: {
      profiles: [
        { name: 'alpha', login: alpha({ access_exp: NOW - DAY, session: null, auth_time: null }) },
      ],
    },
  });
  add({
    name: 'plan-scan-failed',
    kind: 'plan',
    sandbox: { profiles: [{ name: 'alpha', login: alpha({ access_exp: NOW - DAY }) }] },
    inputs: { scan: 'fail' },
  });
  add({
    name: 'plan-scan-unavailable',
    kind: 'plan',
    sandbox: { profiles: [{ name: 'alpha', login: alpha({ access_exp: NOW - DAY }) }] },
    inputs: { scan: 'unavailable' },
  });
  add({
    name: 'plan-missing-login',
    kind: 'plan',
    sandbox: {
      profiles: [{ name: 'alpha', login: alpha() }],
      registryOnly: ['empty'],
    },
  });
  add({
    name: 'dry-run-guards-and-same-account',
    kind: 'dryRun',
    sandbox: {
      profiles: [
        { name: 'alpha', login: alpha({ access_exp: NOW - DAY }) },
        {
          name: 'alpha-two',
          login: alpha({
            refresh: 'rt-FAKE-alpha-two',
            label: 'alphatwo',
            session: 'sess-two',
            auth_time: 1_790_000_000,
            access_exp: NOW + 6 * DAY,
          }),
        },
      ],
      live: { account: 'bravo', refresh: 'rt-FAKE-bravo-live', label: 'bravolive' },
      homes: [
        { rel: '.codex-t3/gmail', login: alpha({ refresh: 'rt-FAKE-alpha-t3', label: 't3gm' }) },
      ],
    },
  });
  add({
    name: 'plan-cliproxy-codex-source',
    kind: 'plan',
    sandbox: {
      profiles: [{ name: 'alpha', login: alpha({ access_exp: NOW - DAY }) }],
      cliproxy: [
        {
          folder: 'auth',
          name: 'codex-copy.json',
          login: alpha({ label: 'cliproxy', session: 'sess-clip', auth_time: 1 }),
        },
        {
          folder: 'auth-paused',
          name: 'other.json',
          login: {
            raw: JSON.stringify({ type: 'gemini', tokens: { refresh_token: 'rt-FAKE-alpha-1' } }),
          },
        },
      ],
    },
  });

  // ---- renewal: success and write guards (codex-profile-renewal.test.ts) ----
  add({
    name: 'seq-success-rewrites-only-due-profile',
    kind: 'sequence',
    sandbox: renewalBase(),
    inputs: { steps: [{}], fetch: { responses: [ok()] }, log: true },
  });
  add({
    name: 'attempt-omitted-id-and-refresh',
    kind: 'attempt',
    sandbox: renewalBase(),
    inputs: { name: 'alpha', fetch: { responses: [ok(alphaNew, { id: false, refresh: false })] } },
  });
  add({
    name: 'attempt-compare-and-swap-changed',
    kind: 'attempt',
    sandbox: renewalBase(),
    inputs: {
      name: 'alpha',
      fetch: { responses: [ok()] },
      actions: [{ writeProfile: { name: 'alpha', login: signin('signin', 'sess-signin') } }],
    },
  });
  add({
    name: 'attempt-identity-mismatch-id-token',
    kind: 'attempt',
    sandbox: renewalBase(),
    inputs: {
      name: 'alpha',
      fetch: {
        responses: [
          ok({
            ...alphaNew,
            account: {
              email: 'bravo@example.test',
              account_id: 'acct-alpha',
              user_id: 'user-bravo',
            },
          }),
        ],
      },
    },
  });
  add({
    name: 'attempt-identity-mismatch-workspace',
    kind: 'attempt',
    sandbox: renewalBase(),
    inputs: {
      name: 'alpha',
      fetch: { responses: [ok({ ...alphaNew, access_account_id: 'acct-elsewhere' })] },
    },
  });
  add({
    name: 'attempt-never-refreshes-live',
    kind: 'attempt',
    sandbox: {
      ...renewalBase(),
      live: alpha({
        refresh: 'rt-FAKE-alpha-live',
        label: 'alphalive',
        session: 'sess-live2',
        auth_time: 1_790_000_000,
        access_exp: NOW - DAY,
      }),
    },
    inputs: { name: 'alpha', fetch: { responses: [ok()] } },
  });

  // ---- renewal: failures ----
  deadBodies.forEach((response, index) => {
    add({
      name: `seq-dead-${index + 1}-retries-only-after-change`,
      kind: 'sequence',
      sandbox: renewalBase(),
      inputs: {
        steps: [
          {},
          { advanceMs: 30 * DAY },
          {
            actions: [
              { writeProfile: { name: 'alpha', login: signin('signin2', 'sess-signin2') } },
            ],
          },
        ],
        fetch: { responses: [response], repeatLast: true },
      },
    });
  });
  add({
    name: 'seq-transient-backoff-with-jitter',
    kind: 'sequence',
    sandbox: renewalBase(),
    inputs: {
      steps: [
        {},
        { advanceMs: 10 * MIN },
        { advanceMs: 6 * MIN },
        { advanceMs: 31 * MIN },
        { advanceMs: 61 * MIN },
      ],
      fetch: {
        responses: [
          { status: 503, body: 'upstream' },
          { error: 'timeout' },
          { status: 429, body: {} },
          ok(),
        ],
        repeatLast: true,
      },
    },
  });
  add({
    name: 'attempt-invalid-success-body',
    kind: 'attempt',
    sandbox: renewalBase(),
    inputs: { name: 'alpha', fetch: { responses: [{ status: 200, body: '{"access_token": ' }] } },
  });
  add({
    name: 'attempt-activation-busy',
    kind: 'attempt',
    sandbox: { ...renewalBase(), lock: true },
    inputs: { name: 'alpha', fetch: { responses: [ok()] } },
  });
  add({
    name: 'seq-secrets-never-leak',
    kind: 'sequence',
    sandbox: {
      profiles: [
        { name: 'alpha', login: alphaOld },
        {
          name: 'dead',
          login: {
            ...alphaOld,
            account: { email: 'dead@example.test', account_id: 'acct-dead', user_id: 'user-dead' },
            refresh: 'rt-FAKE-dead',
            label: 'dead',
            session: 'sess-dead',
          },
        },
        {
          name: 'flaky',
          login: {
            ...alphaOld,
            account: {
              email: 'flaky@example.test',
              account_id: 'acct-flaky',
              user_id: 'user-flaky',
            },
            refresh: 'rt-FAKE-flaky',
            label: 'flaky',
            session: 'sess-flaky',
          },
        },
      ],
      live: bravoLive,
    },
    inputs: {
      steps: [{}],
      fetch: {
        byToken: {
          'rt-FAKE-dead': {
            status: 401,
            body: { error: { code: 'refresh_token_reused', message: 'rt-FAKE-dead was reused' } },
          },
          'rt-FAKE-flaky': { status: 502, body: 'bad gateway rt-FAKE-flaky' },
          '*': ok(),
        },
      },
      log: true,
    },
  });
  add({
    name: 'seq-disabled',
    kind: 'sequence',
    sandbox: renewalBase(),
    inputs: { steps: [{}], fetch: { responses: [ok()] }, env: { CCS_CODEX_RENEWAL: '0' } },
  });
  add({
    name: 'attempt-disabled',
    kind: 'attempt',
    sandbox: renewalBase(),
    inputs: { name: 'alpha', fetch: { responses: [ok()] }, env: { CCS_CODEX_RENEWAL: '0' } },
  });
  add({
    name: 'status-disabled',
    kind: 'status',
    sandbox: renewalBase(),
    inputs: { env: { CCS_CODEX_RENEWAL: '0' } },
  });

  // ---- service timer (codex-profile-renewal-service.test.ts) ----
  add({
    name: 'service-disabled-schedules-nothing',
    kind: 'service',
    inputs: { env: { CCS_CODEX_RENEWAL: '0' }, steps: ['start', 'status', 'stop'] },
  });
  add({
    name: 'service-first-delay-and-stop',
    kind: 'service',
    sandbox: {
      profiles: [
        {
          name: 'alpha',
          login: { account: 'alpha', refresh: 'rt-FAKE-svc', label: 'svc', access_exp: NOW - DAY },
        },
      ],
    },
    inputs: { steps: ['start', 'status', 'stop', 'status'] },
  });
  add({
    name: 'service-failed-cycle-fixed-message',
    kind: 'service',
    inputs: { failCycle: true, steps: ['runCycle', 'status'] },
  });
  add({
    name: 'service-ok-cycle-last-cycle-at',
    kind: 'service',
    inputs: { steps: ['runCycle', 'status'] },
  });

  // ---- Claude renewal reader (claude-signin-renewal.test.ts) ----
  add({ name: 'claude-allowlist', kind: 'claude', inputs: { doc: claudeFile(), now: CLAUDE_NOW } });
  add({
    name: 'claude-stale-runner',
    kind: 'claude',
    inputs: { doc: claudeFile({ generatedAt: at(CLAUDE_NOW - 2 * HOUR) }), now: CLAUDE_NOW },
  });
  add({
    name: 'claude-secretish-and-bad-rows',
    kind: 'claude',
    inputs: {
      doc: claudeFile({
        needsYou: [
          { email: 'a@example.com', reason: 'captcha', how: 'open https://claude.ai/magic-link#x' },
          {
            email: 'b@example.com',
            reason: 'captcha',
            how: `type ${'A'.repeat(24)}#${'B'.repeat(24)}`,
          },
          { email: 'not-an-email', reason: 'captcha' },
        ],
        signIns: [
          { host: '../etc', profile: 'p', state: 'ok' },
          { host: 'mac', profile: 'p', state: 'weird' },
        ],
      }),
      now: CLAUDE_NOW,
    },
  });
  add({
    name: 'claude-browser-sessions',
    kind: 'claude',
    inputs: {
      doc: claudeFile({
        browserSessions: [
          {
            email: 'a@example.com',
            signedIn: true,
            sessionEndsAt: at(CLAUDE_NOW + 28 * 24 * HOUR),
            visitExtends: false,
            value: 'sk',
          },
        ],
      }),
      now: CLAUDE_NOW,
    },
  });
  add({
    name: 'claude-version-refused',
    kind: 'claude',
    inputs: { doc: claudeFile({ version: 2 }), now: CLAUDE_NOW },
  });
  add({ name: 'claude-broken-json', kind: 'claude', inputs: { text: '{', now: CLAUDE_NOW } });
  for (const word of ['no_node', 'node_launch_failed', 'sk-ant-fixture-not-for-the-dashboard']) {
    add({
      name: `claude-browser-error-${word.slice(0, 12)}`,
      kind: 'claude',
      inputs: { doc: claudeFile({ browserError: word }), now: CLAUDE_NOW },
    });
  }
  add({
    name: 'claude-reader-no-folder',
    kind: 'claudeReader',
    inputs: { folder: null, now: CLAUDE_NOW },
  });
  add({
    name: 'claude-reader-file',
    kind: 'claudeReader',
    inputs: { folder: '/share', text: JSON.stringify(claudeFile()), now: CLAUDE_NOW },
  });

  // ---- dashboard notes (account-dashboard-codex-renewal.test.ts) ----
  const note = (name: string, state: string, reason: string, extra: Record<string, unknown> = {}) =>
    add({
      name,
      kind: 'note',
      inputs: { state, reason, now: NOTE_NOW, ...extra },
    });
  note('note-failed-dead', 'failed', 'dead');
  note('note-failed-identity', 'failed', 'identity_mismatch');
  note('note-needs-reauth-due', 'due', 'due', { needsReauth: true });
  note('note-needs-reauth-renewing', 'renewing', 'renewing', { needsReauth: true });
  note('note-needs-reauth-fresh-quiet', 'ok', 'fresh', { needsReauth: true });
  note('note-retry-within-48h', 'retrying', 'transient', {
    accessExpiresAt: '2026-10-02T08:00:00Z',
  });
  note('note-retry-past-expiry', 'retrying', 'transient', {
    accessExpiresAt: '2026-09-30T00:00:00Z',
  });
  note('note-retry-far-from-expiry', 'retrying', 'transient', {
    accessExpiresAt: '2026-10-03T12:01:00Z',
  });
  for (const reason of [
    'shared_family',
    'family_unverifiable',
    'unverifiable',
    'in_use',
    'process_scan_failed',
    'live_unverifiable',
    'unsafe_file',
  ]) {
    note(`note-stuck-${reason}`, 'skipped', reason, { accessExpiresAt: '2026-10-04T12:00:00Z' });
  }
  note('note-stuck-far-quiet', 'skipped', 'shared_family', {
    accessExpiresAt: '2026-10-06T12:00:00Z',
  });
  for (const reason of ['activation_busy', 'changed', 'disabled']) {
    note(`note-self-clearing-${reason}`, 'skipped', reason, {
      accessExpiresAt: '2026-10-02T12:00:00Z',
    });
  }

  // ---- Antigravity live-login renewal (live-login-renewal.ts) ----
  const grant = (refresh: string) => ({ refresh });
  add({
    name: 'agy-grant-same',
    kind: 'sameGrant',
    inputs: { left: grant('rt-a'), right: grant('rt-a') },
  });
  add({
    name: 'agy-grant-different',
    kind: 'sameGrant',
    inputs: { left: grant('rt-a'), right: grant('rt-b') },
  });
  add({
    name: 'agy-grant-empty',
    kind: 'sameGrant',
    inputs: { left: { empty: true }, right: { empty: true } },
  });
  add({
    name: 'agy-grant-broken',
    kind: 'sameGrant',
    inputs: { left: { raw: '{not json' }, right: grant('rt-a') },
  });
  add({
    name: 'agy-grant-no-refresh',
    kind: 'sameGrant',
    inputs: { left: { raw: '{"token":{}}' }, right: { raw: '{"token":{}}' } },
  });
  add({
    name: 'agy-grant-oversized',
    kind: 'sameGrant',
    inputs: { left: { oversized: true }, right: { oversized: true } },
  });

  const identity = {
    email: 'a@example.com',
    subject: 'sub-1',
    verifiedAt: '2026-10-08T12:00:00.000Z',
    source: 'provider-userinfo',
  };
  const liveCase = (name: string, inputs: Record<string, unknown>) =>
    add({ name, kind: 'liveToRenew', inputs: { profile: identity, ...inputs } });
  liveCase('agy-live-no-file', { exists: false, credential: grant('rt-live') });
  liveCase('agy-live-unreadable', { exists: true, credential: null });
  liveCase('agy-live-saved-revision', {
    exists: true,
    credential: grant('rt-live'),
    matchSaved: true,
  });
  liveCase('agy-live-verified-identity', {
    exists: true,
    credential: grant('rt-live'),
    validate: identity,
  });
  liveCase('agy-live-other-account', {
    exists: true,
    credential: grant('rt-live'),
    validate: { ...identity, subject: 'sub-2' },
  });
  liveCase('agy-live-google-fails-same-grant', {
    exists: true,
    credential: grant('rt-live'),
    validate: 'error',
    saved: grant('rt-live'),
  });
  liveCase('agy-live-google-fails-other-grant', {
    exists: true,
    credential: grant('rt-live'),
    validate: 'error',
    saved: grant('rt-other'),
  });
  liveCase('agy-live-google-fails-no-saved', {
    exists: true,
    credential: grant('rt-live'),
    validate: 'error',
  });

  const renewCase = (name: string, inputs: Record<string, unknown>) =>
    add({ name, kind: 'renewLive', inputs: { newCredential: grant('rt-new'), ...inputs } });
  renewCase('agy-renew-installed', { installOk: true, liveAfter: grant('rt-old') });
  renewCase('agy-renew-exchange-finished', { installOk: false, liveAfter: grant('rt-new') });
  renewCase('agy-renew-write-failed', {
    installOk: false,
    liveAfter: grant('rt-old'),
    restoreOk: true,
  });
  renewCase('agy-renew-not-updated', {
    installOk: false,
    liveAfter: grant('rt-old'),
    restoreOk: false,
  });
  renewCase('agy-renew-save-error', { installOk: true, liveAfter: grant('rt-old'), saveOk: false });

  // ---- entries (codex-renewal-entries.ts) ----
  add({
    name: 'entries-from-status',
    kind: 'entries',
    sandbox: { profiles: [{ name: 'alpha', login: alpha({ access_exp: NOW - DAY }) }] },
  });
  add({ name: 'entries-failed-read-empty', kind: 'entries', inputs: { fail: true } });

  return cases;
}
