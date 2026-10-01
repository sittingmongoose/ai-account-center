import { describe, expect, it } from 'bun:test';
import { OpenCodeConsoleWalletService } from '../../../src/web-server/services/opencode-console-wallet-service';

const source = JSON.stringify({ version: 1, platform: 'mac', sshHost: 'fixture-mac' });
function payload(overrides: Record<string, unknown> = {}) {
  return JSON.stringify({
    id: 'plan-opencode-go-console-mac-012345abcdef',
    provider: 'opencode-go',
    platform: 'mac',
    status: 'ok',
    email: null,
    label: 'OpenCode console wallet',
    source: 'Authenticated OpenCode console workspace on Mac',
    plan: 'Go',
    fetchedAt: '2026-10-01T12:00:00Z',
    sampledAt: '2026-10-01T12:00:00Z',
    auth: 'PRIVATE-SENTINEL',
    capabilities: { codexProfile: 'evil' },
    windows: [
      {
        key: 'console-week',
        kind: 'rate_limit',
        usedPercent: 125.5,
        remainingPercent: 0,
        resetAt: '2026-10-08T12:00:00Z',
      },
      { key: 'zen-balance', kind: 'balance', remaining: -2.5, unit: 'USD', expiresAt: null },
    ],
    ...overrides,
  });
}

describe('optional identity-bound OpenCode console wallet', () => {
  it('omits accounts entirely when the opt-in source or browser capsule is unavailable', async () => {
    let calls = 0;
    const absent = new OpenCodeConsoleWalletService({
      readSource: async () => null,
      runSource: async () => {
        calls++;
        return payload();
      },
    });
    expect(await absent.get({ refresh: true })).toEqual([]);
    expect(calls).toBe(0);
    const missingCapsule = new OpenCodeConsoleWalletService({
      readSource: async () => source,
      runSource: async () => {
        throw new Error('PRIVATE-CREDENTIAL');
      },
    });
    expect(await missingCapsule.get()).toEqual([]);
  });

  it('preserves separate workspace identity, signed money, actual overage and authoritative dates', async () => {
    const service = new OpenCodeConsoleWalletService({
      readSource: async () => source,
      runSource: async () => payload(),
    });
    const [wallet] = await service.get();
    expect(wallet.id).toBe('plan-opencode-go-console-mac-012345abcdef');
    expect(wallet.email).toBeNull();
    expect(wallet.message).toContain('has not been linked');
    expect(wallet.capabilities).toEqual({
      codexProfile: null,
      claudeProfileId: null,
      claudePlatforms: [],
    });
    expect(wallet.windows[0].usedPercent).toBe(125.5);
    expect(wallet.windows[0].remainingPercent).toBe(0);
    expect(wallet.windows[0].resetAt).toBe('2026-10-08T12:00:00.000Z');
    expect(wallet.windows[1].remaining).toBe(-2.5);
    expect(wallet.windows[1].expiresAt).toBeNull();
    expect(wallet.windows[1].resetAt).toBeNull();
    expect(JSON.stringify(wallet)).not.toContain('PRIVATE');
  });

  for (const invalid of [
    { id: 'opencode-go:usage' },
    { provider: 'codex' },
    { platform: 'windows' },
    { email: 'a@example.test' },
    { status: 'unavailable' },
    { source: 'Unverified workspace' },
    { fetchedAt: 'invalid' },
    { windows: [] },
    { windows: [{ key: 'zen-balance', kind: 'balance', remaining: 2, unit: 'credits' }] },
    { windows: [{ key: 'arbitrary-secret', kind: 'balance', remaining: 2, unit: 'USD' }] },
  ]) {
    it(`rejects an unverified or incomplete helper sample ${JSON.stringify(invalid)}`, async () => {
      const service = new OpenCodeConsoleWalletService({
        readSource: async () => source,
        runSource: async () => payload(invalid),
      });
      expect(await service.get()).toEqual([]);
    });
  }

  it('rejects source command fragments before starting a helper', async () => {
    let calls = 0;
    const service = new OpenCodeConsoleWalletService({
      readSource: async () =>
        JSON.stringify({ version: 1, platform: 'mac', sshHost: 'fixture;echo-secret' }),
      runSource: async () => {
        calls++;
        return payload();
      },
    });
    expect(await service.get()).toEqual([]);
    expect(calls).toBe(0);
  });

  it('coalesces requests, debounces refresh and backs off missing capsules without adding cards', async () => {
    let now = 0,
      calls = 0,
      online = true;
    const service = new OpenCodeConsoleWalletService({
      now: () => now,
      readSource: async () => source,
      runSource: async () => {
        calls++;
        if (!online) throw new Error('offline');
        return payload();
      },
    });
    await Promise.all([service.get({ refresh: true }), service.get({ refresh: true })]);
    expect(calls).toBe(1);
    now = 4_999;
    expect((await service.get({ refresh: true }))[0].status).toBe('cached');
    expect(calls).toBe(1);
    now = 5_000;
    online = false;
    expect(await service.get({ refresh: true })).toEqual([]);
    expect(calls).toBe(2);
    now = 34_999;
    expect(await service.get({ refresh: true })).toEqual([]);
    expect(calls).toBe(2);
    now = 35_000;
    online = true;
    expect((await service.get({ refresh: true }))[0].status).toBe('ok');
    expect(calls).toBe(3);
  });

  it('drops a cached workspace when its configured source changes', async () => {
    let host = 'first-mac';
    const service = new OpenCodeConsoleWalletService({
      readSource: async () => JSON.stringify({ version: 1, platform: 'mac', sshHost: host }),
      runSource: async () => {
        if (host === 'second-mac') throw new Error('missing capsule');
        return payload();
      },
    });
    expect(await service.get()).toHaveLength(1);
    host = 'second-mac';
    expect(await service.get()).toEqual([]);
  });
});
