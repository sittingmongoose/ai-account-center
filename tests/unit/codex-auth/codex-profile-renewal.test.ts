import { afterEach, beforeEach, describe, expect, it, spyOn } from 'bun:test';
import * as fs from 'fs';
import * as path from 'path';
import * as lockfile from 'proper-lockfile';
import {
  getCodexProfileRenewalStatus,
  renewCodexProfile,
  renewDueCodexProfiles,
} from '../../../src/codex-auth/codex-profile-renewal';
import {
  CODEX_REFRESH_CLIENT_ID,
  CODEX_REFRESH_URL,
} from '../../../src/codex-auth/codex-token-refresh';
import { acquireCodexActivationLock } from '../../../src/codex-auth/codex-activation-lock';
import {
  activateCodexProfile,
  CodexActivationError,
  type CodexActivationRuntime,
} from '../../../src/codex-auth/activate-codex-profile';
import {
  getCodexProfileQuotaRows,
  resetNativeQuotaState,
} from '../../../src/web-server/usage/native-quota-collector';
import type { CodexQuotaResult } from '../../../src/cliproxy/quota/quota-types';
import {
  ALPHA,
  BRAVO,
  DAY,
  NOW,
  RenewalSandbox,
  expectNoSecrets,
  fakeFetch,
  loginBuffer,
  makeTokens,
  refreshedBody,
  type FakeResponse,
  type LoginSpec,
} from './codex-renewal-fixtures';

let box: RenewalSandbox;

const alphaOld: LoginSpec = {
  account: ALPHA,
  refresh: 'rt-FAKE-alpha-old',
  label: 'alphaold',
  session: 'sess-alpha',
  authTime: 1_780_000_100,
  accessExp: NOW - DAY,
};
const alphaNew: LoginSpec = {
  ...alphaOld,
  refresh: 'rt-FAKE-alpha-rotated',
  label: 'alphanew',
  accessExp: NOW + 10 * DAY,
};
const bravo: LoginSpec = {
  account: BRAVO,
  refresh: 'rt-FAKE-bravo',
  label: 'bravo',
  accessExp: NOW + 8 * DAY,
};

function readJson(file: string): Record<string, any> {
  return JSON.parse(fs.readFileSync(file, 'utf8'));
}

function ok(spec = alphaNew, include?: { id: boolean; refresh: boolean }): FakeResponse {
  return { status: 200, body: refreshedBody(spec, include) };
}

beforeEach(() => {
  box = new RenewalSandbox();
  box.addProfile('alpha', alphaOld);
  box.addProfile('bravo', bravo);
  box.writeLive({
    ...bravo,
    refresh: 'rt-FAKE-bravo-live',
    label: 'bravolive',
    session: 'sess-bravo-live',
  });
});

afterEach(() => {
  box.cleanup();
});

describe('successful renewal', () => {
  it('rewrites only the due profile, preserving other keys, with rotated tokens and 0600', async () => {
    const before = box.files();
    const http = fakeFetch(() => ok());
    const cycle = await renewDueCodexProfiles(box.options({ fetch: http.fetch }));

    expect(http.calls).toHaveLength(1);
    expect(http.calls[0].url).toBe(CODEX_REFRESH_URL);
    expect(http.calls[0].body).toEqual({
      client_id: CODEX_REFRESH_CLIENT_ID,
      grant_type: 'refresh_token',
      refresh_token: 'rt-FAKE-alpha-old',
      scope: 'openid profile email',
    });
    expect(cycle.attempts.map((attempt) => [attempt.name, attempt.reason])).toEqual([
      ['alpha', 'renewed'],
    ]);
    expect(cycle.plans.find((plan) => plan.name === 'bravo')?.reason).toBe('live');

    const saved = readJson(box.profileAuth('alpha'));
    const fresh = makeTokens(alphaNew);
    expect(saved.tokens.access_token).toBe(fresh.access_token);
    expect(saved.tokens.id_token).toBe(fresh.id_token);
    expect(saved.tokens.refresh_token).toBe('rt-FAKE-alpha-rotated');
    expect(saved.tokens.account_id).toBe(ALPHA.accountId);
    expect(saved.OPENAI_API_KEY).toBeNull();
    expect(saved.auth_mode).toBe('chatgpt');
    expect(saved.unknown_extra).toEqual({ kept: true });
    expect(saved.last_refresh).toBe(new Date(NOW).toISOString());
    expect(fs.statSync(box.profileAuth('alpha')).mode & 0o777).toBe(0o600);

    const after = box.files();
    const changed = [...after.keys()].filter(
      (file) => !before.has(file) || !before.get(file)?.equals(after.get(file) as Buffer)
    );
    expect(changed.sort()).toEqual([box.profileAuth('alpha'), box.statusPath].sort());
    expect([...before.keys()].filter((file) => !after.has(file))).toEqual([]);
    expect(fs.statSync(box.statusPath).mode & 0o777).toBe(0o600);
    expect(fs.existsSync(path.join(box.codexHome, '.ccs-activation.lock'))).toBe(false);

    const status = getCodexProfileRenewalStatus(box.options());
    const alpha = status.profiles.find((profile) => profile.name === 'alpha');
    expect(alpha?.state).toBe('ok');
    expect(alpha?.lastRenewedAt).toBe(new Date(NOW).toISOString());
    expect(alpha?.accessExpiresAt).toBe(new Date(NOW + 10 * DAY).toISOString());
    expect(
      box.logs.find((line) => line.context.profile === 'alpha')?.context.familyMarkersKept
    ).toBe(true);
    expect(box.logs.map((line) => [line.context.profile, line.context.outcome])).toEqual([
      ['bravo', 'live'],
      ['alpha', 'renewed'],
    ]);
  });

  it('keeps the saved refresh and id tokens when the response omits them', async () => {
    const http = fakeFetch(() => ok(alphaNew, { id: false, refresh: false }));
    const attempt = await renewCodexProfile('alpha', box.options({ fetch: http.fetch }));
    expect(attempt.reason).toBe('renewed');
    const saved = readJson(box.profileAuth('alpha'));
    expect(saved.tokens.refresh_token).toBe('rt-FAKE-alpha-old');
    expect(saved.tokens.id_token).toBe(makeTokens(alphaOld).id_token);
    expect(saved.tokens.access_token).toBe(makeTokens(alphaNew).access_token);
  });

  it('lets the quota collector drop a cached reauth row once the file is renewed', async () => {
    resetNativeQuotaState();
    const quota = (token: string): CodexQuotaResult =>
      token === makeTokens(alphaOld).access_token
        ? { success: false, windows: [], planType: null, lastUpdated: NOW, needsReauth: true }
        : {
            success: true,
            windows: [],
            coreUsage: {
              fiveHour: { label: '5h', remainingPercent: 60, resetAfterSeconds: 60, resetAt: null },
              weekly: null,
            },
            planType: 'pro',
            lastUpdated: NOW,
          };
    const deps = {
      fetchCodexQuotaWithToken: async (token: string) => quota(token),
      defaultCodexProfile: () => null,
      now: () => NOW,
    };
    const [stale] = await getCodexProfileQuotaRows(['alpha'], deps);
    expect(stale.needsReauth).toBe(true);
    await renewCodexProfile('alpha', box.options({ fetch: fakeFetch(() => ok()).fetch }));
    const [fresh] = await getCodexProfileQuotaRows(['alpha'], deps);
    expect(fresh.needsReauth).toBeFalsy();
    expect(fresh.quotaStatus).toBe('ok');
    resetNativeQuotaState();
  });
});

describe('write guards', () => {
  it('does not overwrite a profile that changed during the request (compare-and-swap)', async () => {
    const replacement = loginBuffer({
      ...alphaOld,
      refresh: 'rt-FAKE-alpha-signin',
      label: 'signin',
      session: 'sess-signin',
    });
    const http = fakeFetch(() => {
      box.writeProfile('alpha', replacement);
      return ok();
    });
    const attempt = await renewCodexProfile('alpha', box.options({ fetch: http.fetch }));
    expect(attempt.reason).toBe('changed');
    expect(fs.readFileSync(box.profileAuth('alpha')).equals(replacement)).toBe(true);
  });

  it('writes nothing when the new id token is another account, then waits for Sign in again', async () => {
    const before = fs.readFileSync(box.profileAuth('alpha'));
    const http = fakeFetch(() =>
      ok({ ...alphaNew, account: { ...BRAVO, accountId: ALPHA.accountId } })
    );
    const attempt = await renewCodexProfile('alpha', box.options({ fetch: http.fetch }));
    expect(attempt.reason).toBe('identity_mismatch');
    expect(attempt.state).toBe('failed');
    expect(fs.readFileSync(box.profileAuth('alpha')).equals(before)).toBe(true);
    await renewDueCodexProfiles(box.options({ fetch: http.fetch }));
    expect(http.calls).toHaveLength(1);
  });

  it('writes nothing when the access token claims another workspace', async () => {
    const before = fs.readFileSync(box.profileAuth('alpha'));
    const http = fakeFetch(() => ok({ ...alphaNew, accessAccountId: 'acct-elsewhere' }));
    expect((await renewCodexProfile('alpha', box.options({ fetch: http.fetch }))).reason).toBe(
      'identity_mismatch'
    );
    expect(fs.readFileSync(box.profileAuth('alpha')).equals(before)).toBe(true);
  });

  it('never refreshes the live account even when called directly', async () => {
    box.writeLive({
      ...alphaOld,
      refresh: 'rt-FAKE-alpha-live',
      label: 'alphalive',
      session: 'sess-live2',
      authTime: 1_790_000_000,
    });
    const http = fakeFetch(() => ok());
    const attempt = await renewCodexProfile('alpha', box.options({ fetch: http.fetch }));
    expect(attempt.reason).toBe('live');
    expect(http.calls).toHaveLength(0);
  });
});

describe('failures', () => {
  for (const [status, body] of [
    [401, { error: { code: 'refresh_token_reused' } }],
    [401, { error: 'refresh_token_expired' }],
    [401, { error: { code: 'refresh_token_invalidated' } }],
    [400, { error: 'invalid_grant' }],
  ] as const) {
    it(`marks ${status} ${JSON.stringify(body)} failed and retries only after the file changes`, async () => {
      const http = fakeFetch(() => ({ status, body }));
      const first = await renewDueCodexProfiles(box.options({ fetch: http.fetch }));
      expect(first.attempts[0].reason).toBe('dead');
      expect(first.attempts[0].state).toBe('failed');
      box.clock += 30 * DAY;
      const second = await renewDueCodexProfiles(box.options({ fetch: http.fetch }));
      expect(second.attempts).toHaveLength(0);
      expect(second.plans.find((plan) => plan.name === 'alpha')?.decision).toBe('dead');
      expect(http.calls).toHaveLength(1);
      const status1 = getCodexProfileRenewalStatus(box.options());
      expect(status1.profiles.find((profile) => profile.name === 'alpha')?.state).toBe('failed');

      // Sign in again replaces the file: the recorded failure no longer applies.
      box.writeProfile('alpha', {
        ...alphaOld,
        refresh: 'rt-FAKE-alpha-signin',
        label: 'signin2',
        session: 'sess-signin2',
      });
      const third = await renewDueCodexProfiles(box.options({ fetch: http.fetch }));
      expect(third.attempts).toHaveLength(1);
      expect(http.calls[1].body.refresh_token).toBe('rt-FAKE-alpha-signin');
    });
  }

  it('backs off transient failures with jitter and retries after the backoff', async () => {
    const responses: FakeResponse[] = [
      { status: 503, body: 'upstream' },
      Object.assign(new Error('timed out'), { name: 'TimeoutError' }),
      { status: 429, body: {} },
    ];
    const http = fakeFetch(() => responses.shift() ?? ok());
    const first = await renewDueCodexProfiles(box.options({ fetch: http.fetch }));
    expect(first.attempts[0].reason).toBe('transient');
    expect(first.attempts[0].state).toBe('retrying');
    expect(Date.parse(first.attempts[0].nextAttemptAt ?? '')).toBe(NOW + 15 * 60_000);
    expect(first.retryAt).toBe(first.attempts[0].nextAttemptAt);

    box.clock += 10 * 60_000;
    const waiting = await renewDueCodexProfiles(box.options({ fetch: http.fetch }));
    expect(waiting.attempts).toHaveLength(0);
    expect(waiting.plans.find((plan) => plan.name === 'alpha')?.decision).toBe('backoff');

    box.clock += 6 * 60_000;
    const second = await renewDueCodexProfiles(box.options({ fetch: http.fetch }));
    expect(second.attempts[0].reason).toBe('transient');
    expect(Date.parse(second.attempts[0].nextAttemptAt ?? '') - box.clock).toBe(30 * 60_000);
    const record = readJson(box.statusPath).profiles.alpha;
    expect(record.consecutiveFailures).toBe(2);

    box.clock += 31 * 60_000;
    await renewDueCodexProfiles(box.options({ fetch: http.fetch }));
    box.clock += 61 * 60_000;
    const last = await renewDueCodexProfiles(box.options({ fetch: http.fetch }));
    expect(last.attempts[0].reason).toBe('renewed');
    expect(readJson(box.statusPath).profiles.alpha.consecutiveFailures).toBe(0);
  });

  it('treats an unparseable success body as invalid and retries', async () => {
    const http = fakeFetch(() => ({ status: 200, body: '{"access_token": ' }));
    const attempt = await renewCodexProfile('alpha', box.options({ fetch: http.fetch }));
    expect(attempt.reason).toBe('invalid_response');
    expect(attempt.state).toBe('retrying');
  });
});

describe('activation lock', () => {
  it('skips without waiting while the lock is held and records a short retry', async () => {
    const release = await acquireCodexActivationLock(box.codexHome);
    const before = fs.readFileSync(box.profileAuth('alpha'));
    const http = fakeFetch(() => ok());
    const started = Date.now();
    const attempt = await renewCodexProfile('alpha', box.options({ fetch: http.fetch }));
    expect(Date.now() - started).toBeLessThan(1000);
    await release();
    expect(attempt.reason).toBe('activation_busy');
    expect(attempt.state).toBe('skipped');
    expect(Date.parse(attempt.nextAttemptAt ?? '')).toBe(NOW + 15 * 60_000);
    expect(http.calls).toHaveLength(0);
    expect(fs.readFileSync(box.profileAuth('alpha')).equals(before)).toBe(true);
  });

  it('skips while a real activation is running', async () => {
    let stopped!: () => void;
    let resume!: () => void;
    const inStop = new Promise<void>((done) => (stopped = done));
    const gate = new Promise<void>((done) => (resume = done));
    const runtime: CodexActivationRuntime = {
      async stop() {
        stopped();
        await gate;
      },
      async start() {},
    };
    const activation = activateCodexProfile('bravo', { codexHome: box.codexHome, runtime });
    await inStop;
    const http = fakeFetch(() => ok());
    const attempt = await renewCodexProfile('alpha', box.options({ fetch: http.fetch }));
    resume();
    await activation;
    expect(attempt.reason).toBe('activation_busy');
    expect(http.calls).toHaveLength(0);
  });

  it('makes an activation wait (or fail busy) while a renewal holds the lock, then installs the renewed login', async () => {
    let entered!: () => void;
    let respond!: () => void;
    const inRequest = new Promise<void>((done) => (entered = done));
    const gate = new Promise<void>((done) => (respond = done));
    const http = fakeFetch(async () => {
      entered();
      await gate;
      return ok();
    });
    const renewal = renewCodexProfile('alpha', box.options({ fetch: http.fetch }));
    await inRequest;

    // A contender that does not wait is refused at once.
    const realLock = lockfile.lock;
    const noWait = spyOn(lockfile, 'lock').mockImplementation(((file, options) =>
      realLock(file, { ...options, retries: 0 })) as typeof lockfile.lock);
    const runtime: CodexActivationRuntime = { async stop() {}, async start() {} };
    let busy: unknown;
    try {
      await activateCodexProfile('alpha', { codexHome: box.codexHome, runtime });
    } catch (error) {
      busy = error;
    } finally {
      noWait.mockRestore();
    }
    expect(busy).toBeInstanceOf(CodexActivationError);
    expect((busy as CodexActivationError).code).toBe('busy');

    // A normal activation waits for the renewal and then copies the renewed login.
    const activation = activateCodexProfile('alpha', { codexHome: box.codexHome, runtime });
    respond();
    expect((await renewal).reason).toBe('renewed');
    await activation;
    const live = readJson(path.join(box.codexHome, 'auth.json'));
    expect(live.tokens.refresh_token).toBe('rt-FAKE-alpha-rotated');
  });
});

describe('secrets', () => {
  it('never puts token text in logs, status, attempts, status DTOs or the dry run', async () => {
    box.addProfile('dead', {
      ...alphaOld,
      account: {
        ...ALPHA,
        email: 'dead@example.test',
        accountId: 'acct-dead',
        userId: 'user-dead',
      },
      refresh: 'rt-FAKE-dead',
      label: 'dead',
      session: 'sess-dead',
    });
    box.addProfile('flaky', {
      ...alphaOld,
      account: {
        ...ALPHA,
        email: 'flaky@example.test',
        accountId: 'acct-flaky',
        userId: 'user-flaky',
      },
      refresh: 'rt-FAKE-flaky',
      label: 'flaky',
      session: 'sess-flaky',
    });
    const http = fakeFetch(({ body }) => {
      if (body.refresh_token === 'rt-FAKE-dead') {
        return {
          status: 401,
          body: { error: { code: 'refresh_token_reused', message: 'rt-FAKE-dead was reused' } },
        };
      }
      if (body.refresh_token === 'rt-FAKE-flaky')
        return { status: 502, body: `bad gateway rt-FAKE-flaky` };
      return ok();
    });
    const cycle = await renewDueCodexProfiles(box.options({ fetch: http.fetch }));
    expect(cycle.attempts.map((attempt) => attempt.reason).sort()).toEqual([
      'dead',
      'renewed',
      'transient',
    ]);
    expectNoSecrets(cycle);
    expectNoSecrets(box.logs);
    expectNoSecrets(fs.readFileSync(box.statusPath, 'utf8'));
    expectNoSecrets(getCodexProfileRenewalStatus(box.options()));
    const { dryRunCodexProfileRenewal } = await import(
      '../../../src/codex-auth/codex-profile-renewal'
    );
    expectNoSecrets(await dryRunCodexProfileRenewal(box.options()));
    expect(() => expectNoSecrets(fs.readFileSync(box.profileAuth('alpha'), 'utf8'))).toThrow();
  });

  it('keeps the renewal off with CCS_CODEX_RENEWAL=0', async () => {
    const http = fakeFetch(() => ok());
    const options = box.options({ fetch: http.fetch, env: { CCS_CODEX_RENEWAL: '0' } });
    const cycle = await renewDueCodexProfiles(options);
    expect(cycle.enabled).toBe(false);
    expect((await renewCodexProfile('alpha', options)).reason).toBe('disabled');
    expect(http.calls).toHaveLength(0);
    const status = getCodexProfileRenewalStatus(options);
    expect(status.profiles.every((profile) => profile.reason === 'disabled')).toBe(true);
  });
});
