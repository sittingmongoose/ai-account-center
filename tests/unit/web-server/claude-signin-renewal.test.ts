import { describe, expect, it } from 'bun:test';
import {
  parseClaudeSignInRenewal,
  readClaudeSignInRenewal,
} from '../../../src/web-server/services/claude-signin-renewal';
import { mergeRenewal } from '../../../src/web-server/services/signin-health-service';

const NOW = Date.parse('2026-11-03T12:00:00Z');
const H = 3600 * 1000;
const at = (ms: number) => new Date(ms).toISOString().replace(/\.\d{3}Z$/, 'Z');

function file(overrides: Record<string, unknown> = {}) {
  return {
    version: 1,
    runner: 'mac',
    runnerVersion: '1.0.0',
    generatedAt: at(NOW - 5 * 60 * 1000),
    staleAfterSeconds: 3600,
    renewWithinHours: 96,
    needsYou: [
      { email: 'A@Example.com', reason: 'captcha', how: 'Solve the CAPTCHA.', since: at(NOW) },
    ],
    hosts: [
      { host: 'mac', readAt: at(NOW), error: null },
      { host: 'windows', readAt: at(NOW - H), error: 'ssh_timeout' },
    ],
    signIns: [
      {
        host: 'mac',
        profile: 'gmail',
        email: 'a@example.com',
        state: 'ok',
        signInEndsAt: at(NOW + 40 * 24 * H),
        logoutAt: at(NOW + 40 * 24 * H + 3 * H),
        renewFrom: at(NOW + 36 * 24 * H),
        autoRenew: true,
        lastRenewAt: at(NOW - H),
        lastResult: 'renewed',
        nextVerifyAt: at(NOW + 8 * H),
        retryAfter: null,
        reverts: 0,
        readAt: at(NOW),
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

describe('claude sign-in renewal file', () => {
  it('keeps the allowlisted fields and lowercases emails', () => {
    const parsed = parseClaudeSignInRenewal(JSON.stringify(file()), NOW);
    expect(parsed?.stale).toBe(false);
    expect(parsed?.needsYou).toEqual([
      { email: 'a@example.com', reason: 'captcha', how: 'Solve the CAPTCHA.', since: at(NOW) },
    ]);
    expect(parsed?.signIns[0].renewFrom).toBe(at(NOW + 36 * 24 * H));
    expect(parsed?.signIns[1].signInEndsAt).toBeNull();
  });

  it('marks a runner that stopped writing as stale', () => {
    const parsed = parseClaudeSignInRenewal(
      JSON.stringify(file({ generatedAt: at(NOW - 2 * H) })),
      NOW
    );
    expect(parsed?.stale).toBe(true);
  });

  it('never carries a link, code or token, and drops malformed rows', () => {
    const parsed = parseClaudeSignInRenewal(
      JSON.stringify(
        file({
          needsYou: [
            {
              email: 'a@example.com',
              reason: 'captcha',
              how: 'open https://claude.ai/magic-link#x',
            },
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
        })
      ),
      NOW
    );
    expect(parsed?.needsYou.map((n) => n.how)).toEqual(['', '']);
    expect(parsed?.signIns).toEqual([]);
    expect(JSON.stringify(parsed)).not.toContain('magic-link');
  });

  it('keeps browser session ends, never cookie values', () => {
    const parsed = parseClaudeSignInRenewal(
      JSON.stringify(
        file({
          browserSessions: [
            { email: 'a@example.com', signedIn: true, sessionEndsAt: at(NOW + 28 * 24 * H), visitExtends: false, value: 'sk' },
          ],
        })
      ),
      NOW
    );
    expect(parsed?.browserSessions).toEqual([
      { email: 'a@example.com', signedIn: true, sessionEndsAt: at(NOW + 28 * 24 * H), checkedAt: null, visitExtends: false },
    ]);
  });

  it('refuses other versions and broken JSON', () => {
    expect(parseClaudeSignInRenewal(JSON.stringify(file({ version: 2 })), NOW)).toBeNull();
    expect(parseClaudeSignInRenewal('{', NOW)).toBeNull();
  });

  it('reads nothing when no snapshot folder is set', async () => {
    expect(await readClaudeSignInRenewal({ folder: async () => null })).toBeNull();
    const read = await readClaudeSignInRenewal({
      folder: async () => '/share',
      readFile: async (name) => {
        expect(name).toBe('/share/claude-signins.json');
        return JSON.stringify(file());
      },
      now: () => NOW,
    });
    expect(read?.runner).toBe('mac');
  });
});

describe('merging renewal into sign-in health', () => {
  const renewal = parseClaudeSignInRenewal(JSON.stringify(file()), NOW);

  it('uses the runner rows for a computer it read, keeping other providers', () => {
    const merged = mergeRenewal(
      {
        mac: [
          {
            name: 'keychain',
            provider: 'claude',
            platform: 'mac',
            machine: 'mac',
            state: 'unavailable',
            fixCommand: '',
          },
          {
            name: 'codex',
            provider: 'codex',
            platform: 'mac',
            machine: 'mac',
            state: 'unsupported',
            fixCommand: '',
          },
        ],
      },
      renewal,
      NOW
    );
    expect(merged.mac.map((r) => `${r.provider}:${r.name}:${r.state}`)).toEqual([
      'claude:gmail:ok',
      'codex:codex:unsupported',
    ]);
    expect(merged.mac[0]).toMatchObject({
      email: 'a@example.com',
      autoRenew: true,
      lastResult: 'renewed',
    });
  });

  it('only adds renewal fields where the runner could not read the computer', () => {
    const own = {
      name: 'gmail',
      provider: 'claude',
      platform: 'windows',
      machine: 'windows',
      state: 'ok' as const,
      fixCommand: 'x',
    };
    const merged = mergeRenewal({ windows: [own] }, renewal, NOW);
    expect(merged.windows[0]).toMatchObject({ fixCommand: 'x', lastResult: 'end_not_moved' });
  });

  it('ignores a stale runner', () => {
    const stale = parseClaudeSignInRenewal(
      JSON.stringify(file({ generatedAt: at(NOW - 3 * H) })),
      NOW
    );
    const machines = { mac: [] };
    expect(mergeRenewal(machines, stale, NOW)).toBe(machines);
  });
});
