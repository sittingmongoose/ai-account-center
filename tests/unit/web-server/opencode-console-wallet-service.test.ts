import { describe, expect, it } from 'bun:test';
import fs from 'fs/promises';
import os from 'os';
import path from 'path';
import { OpenCodeConsoleWalletService } from '../../../src/web-server/services/opencode-console-wallet-service';
import type { DashboardAccount } from '../../../src/web-server/services/account-dashboard-types';
import {
  parseUsageReading,
  type UsageCandidate,
  type UsageFailoverTarget,
} from '../../../src/web-server/services/usage-failover-contract';
import { UsageFailoverService } from '../../../src/web-server/services/usage-failover-service';

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

  it('coalesces requests, debounces refresh and retains dated readings while capsules are unavailable', async () => {
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
    const [saved] = await service.get({ refresh: true });
    expect(saved.status).toBe('cached');
    expect(saved.sampledAt).toBe('2026-10-01T12:00:00.000Z');
    expect(saved.windows[1].remaining).toBe(-2.5);
    expect(calls).toBe(2);
    now = 34_999;
    expect((await service.get({ refresh: true }))[0].status).toBe('cached');
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

  it('passes a distinct workspace target to shared failover and preserves cached status', async () => {
    let target: UsageFailoverTarget | null = null;
    const service = new OpenCodeConsoleWalletService({
      readSource: async () => source,
      runSource: async () => payload(),
      failover: {
        recover: async (requested, primary) => {
          target = requested;
          return {
            ...primary,
            status: 'cached',
            source: 'Verified wallet on Nas1',
            message: 'Showing the last saved sample.',
          };
        },
        collect: async () => {
          throw new Error('unexpected collection');
        },
      },
    });
    const [wallet] = await service.get();
    expect(target!.provider).toBe('opencode-wallet');
    expect(target!.accountId).toBe(wallet.id);
    expect(target!.binding).toBe(source);
    expect(target!.primarySources).toEqual([
      { host: 'mac', platform: 'mac', sshHost: 'fixture-mac', selector: 'wallet' },
    ]);
    expect(wallet.status).toBe('cached');
    expect(wallet.source).toBe('Verified wallet on Nas1');
    expect(wallet.message).toContain('has not been linked');
    expect(wallet.capabilities).toEqual({
      codexProfile: null,
      claudeProfileId: null,
      claudePlatforms: [],
    });
  });

  it('restores a known wallet ID after restart and asks other hosts when the primary is offline', async () => {
    const ccsDir = await fs.mkdtemp(path.join(os.tmpdir(), 'aac-wallet-failover-'));
    try {
      let remembered: DashboardAccount | null = null;
      const first = new OpenCodeConsoleWalletService({
        ccsDir,
        readSource: async () => source,
        runSource: async () => payload(),
        failover: {
          recover: async (_, primary) => {
            remembered = primary;
            return primary;
          },
          collect: async () => {
            throw new Error('unexpected collection');
          },
        },
      });
      expect(await first.get()).toHaveLength(1);
      const pointer = JSON.parse(
        await fs.readFile(path.join(ccsDir, 'opencode-console-wallet-account.json'), 'utf8')
      );
      expect(pointer.accountId).toBe(remembered!.id);
      expect(pointer.binding).toMatch(/^[a-f0-9]{64}$/);
      expect(JSON.stringify(pointer)).not.toContain('fixture-mac');
      let fallbackCalls = 0;
      const restarted = new OpenCodeConsoleWalletService({
        ccsDir,
        readSource: async () => source,
        runSource: async () => {
          throw new Error('Mac is offline');
        },
        failover: {
          recover: async () => {
            throw new Error('unexpected recovery');
          },
          collect: async (target, template) => {
            fallbackCalls++;
            expect(target.accountId).toBe(remembered!.id);
            expect(template.windows).toEqual([]);
            return { ...remembered!, source: 'Verified wallet on Ubuntu', status: 'ok' };
          },
        },
      });
      const [wallet] = await restarted.get();
      expect(fallbackCalls).toBe(1);
      expect(wallet.id).toBe(remembered!.id);
      expect(wallet.source).toBe('Verified wallet on Ubuntu');
      expect(wallet.windows[1].remaining).toBe(-2.5);
      const changed = new OpenCodeConsoleWalletService({
        ccsDir,
        readSource: async () =>
          JSON.stringify({ version: 1, platform: 'mac', sshHost: 'different-mac' }),
        runSource: async () => {
          throw new Error('offline');
        },
        failover: {
          recover: async () => {
            throw new Error('unexpected recovery');
          },
          collect: async () => {
            fallbackCalls++;
            throw new Error('wrong source binding');
          },
        },
      });
      expect(await changed.get()).toEqual([]);
      expect(fallbackCalls).toBe(1);
    } finally {
      await fs.rm(ccsDir, { recursive: true, force: true });
    }
  });

  it('does not recreate a removed wallet from an in-flight source response', async () => {
    let configured: string | null = source;
    let resolveSource!: (value: string) => void;
    const delayed = new Promise<string>((resolve) => {
      resolveSource = resolve;
    });
    const service = new OpenCodeConsoleWalletService({
      readSource: async () => configured,
      runSource: async () => delayed,
    });
    const first = service.get();
    await Promise.resolve();
    await Promise.resolve();
    configured = null;
    expect(await service.get()).toEqual([]);
    resolveSource(payload());
    expect(await first).toEqual([]);
  });

  it('does not restore a wallet reading invalidated by the shared identity coordinator', async () => {
    let online = true;
    let now = 0;
    const service = new OpenCodeConsoleWalletService({
      now: () => now,
      readSource: async () => source,
      runSource: async () => {
        if (!online) throw new Error('primary unavailable');
        return payload();
      },
      failover: {
        recover: async (_, primary) => primary,
        collect: async (_, template) => template,
      },
    });
    const [first] = await service.get();
    online = false;
    now = 5_000;
    const [saved] = await service.get({ refresh: true });
    expect(saved.status).toBe('unavailable');
    expect(saved.id).toBe(first.id);
    expect(saved.sampledAt).toBeNull();
    expect(saved.windows).toEqual([]);
  });

  it('discovers the cold-start workspace without a legacy quota call and shares 429 across restart', async () => {
    const ccsDir = await fs.mkdtemp(path.join(os.tmpdir(), 'aac-wallet-managed-'));
    try {
      await fs.chmod(ccsDir, 0o700);
      let now = Date.parse('2026-10-01T12:00:00Z');
      let limited = true;
      let offline = false;
      let quotaCalls = 0;
      const candidate: UsageCandidate = {
        locator: 'a'.repeat(64),
        revision: 'b'.repeat(64),
        selectors: ['wallet'],
        identity: { kind: 'opencode-workspace', key: '012345abcdef' + 'c'.repeat(52) },
        email: null,
        plan: 'Go',
      };
      const safeSample = parseUsageReading({
        ...JSON.parse(payload()),
        windows: [
          {
            key: 'console-week',
            label: 'Weekly',
            kind: 'rate_limit',
            usedPercent: 125.5,
            remainingPercent: 0,
            resetAt: '2026-10-08T12:00:00Z',
          },
          {
            key: 'zen-balance',
            label: 'Zen balance',
            kind: 'balance',
            remaining: -2.5,
            unit: 'USD',
          },
        ],
      });
      expect(safeSample).not.toBeNull();
      const primary = { host: 'mac' as const, platform: 'mac' as const, sshHost: 'fixture-mac' };
      const peer = {
        host: 'windows' as const,
        platform: 'windows' as const,
        sshHost: 'fixture-windows',
      };
      const create = () =>
        new OpenCodeConsoleWalletService({
          ccsDir,
          now: () => now,
          readSource: async () => source,
          inventorySource: async () => {
            if (offline) throw new Error('offline');
            return [candidate];
          },
          failover: new UsageFailoverService({
            now: () => now,
            hosts: async () => [primary, peer],
            transport: async (host, request) => {
              if (offline) throw new Error('offline');
              if (request.action === 'inventory')
                return { version: 1, action: 'inventory', candidates: [candidate] };
              quotaCalls++;
              expect(host.host).toBe('mac');
              return limited
                ? {
                    version: 1,
                    action: 'collect',
                    candidate,
                    sample: null,
                    failureCode: 'rate_limited',
                    retryAfterSeconds: 180,
                  }
                : {
                    version: 1,
                    action: 'collect',
                    candidate,
                    sample: safeSample,
                    failureCode: null,
                  };
            },
          }),
        });
      const first = create();
      expect((await first.get())[0].status).toBe('unavailable');
      expect(quotaCalls).toBe(1);
      now += 31_000;
      await first.get({ refresh: true });
      await create().get({ refresh: true });
      expect(quotaCalls).toBe(1);
      now += 150_000;
      limited = false;
      const [fresh] = await create().get();
      expect(fresh.id).toBe('plan-opencode-go-console-mac-012345abcdef');
      expect(fresh.status).toBe('ok');
      expect(fresh.windows.find((window) => window.key === 'zen-balance')?.remaining).toBe(-2.5);
      expect(quotaCalls).toBe(2);
      offline = true;
      const [saved] = await create().get();
      expect(saved.status).toBe('cached');
      expect(saved.id).toBe(fresh.id);
      expect(saved.sampledAt).toBe(fresh.sampledAt);
      expect(quotaCalls).toBe(2);
    } finally {
      await fs.rm(ccsDir, { recursive: true, force: true });
    }
  });

  it('does not discover an unknown workspace on another host when the configured primary is absent', async () => {
    let collections = 0;
    const service = new OpenCodeConsoleWalletService({
      readSource: async () => source,
      inventorySource: async () => [],
      failover: {
        collect: async (_, row) => {
          collections++;
          return row;
        },
        recover: async (_, row) => {
          collections++;
          return row;
        },
      },
    });
    expect(await service.get()).toEqual([]);
    expect(collections).toBe(0);
  });

  it('rebinds the workspace pointer before a changed workspace fails or goes offline', async () => {
    const ccsDir = await fs.mkdtemp(path.join(os.tmpdir(), 'aac-wallet-rebind-'));
    try {
      await fs.chmod(ccsDir, 0o700);
      let now = 0;
      let key = '1'.repeat(64);
      let offline = false;
      const ids: string[] = [];
      const create = () =>
        new OpenCodeConsoleWalletService({
          ccsDir,
          now: () => now,
          readSource: async () => source,
          inventorySource: async () => {
            if (offline) throw new Error('offline');
            return [
              {
                locator: 'a'.repeat(64),
                revision: 'b'.repeat(64),
                selectors: ['wallet'],
                identity: { kind: 'opencode-workspace', key },
                email: null,
                plan: null,
              },
            ];
          },
          failover: {
            recover: async (_, row) => row,
            collect: async (target, row) => {
              ids.push(target.accountId);
              return key === '1'.repeat(64)
                ? {
                    ...row,
                    status: 'ok',
                    sampledAt: '2026-10-01T12:00:00Z',
                    fetchedAt: '2026-10-01T12:00:00Z',
                    windows: [
                      {
                        key: 'zen-balance',
                        label: 'Zen balance',
                        kind: 'balance',
                        remaining: 1,
                        unit: 'USD',
                        usedPercent: null,
                        remainingPercent: null,
                        used: null,
                        limit: null,
                        resetAt: null,
                        windowMinutes: null,
                      },
                    ],
                  }
                : row;
            },
          },
        });
      const service = create();
      expect((await service.get())[0].id).toBe('plan-opencode-go-console-mac-' + '1'.repeat(12));
      key = '2'.repeat(64);
      now = 5_000;
      const [changed] = await service.get({ refresh: true });
      expect(changed.id).toBe('plan-opencode-go-console-mac-' + '2'.repeat(12));
      expect(changed.windows).toEqual([]);
      offline = true;
      now = 35_000;
      expect((await service.get({ refresh: true }))[0].id).toBe(changed.id);
      expect((await create().get())[0].id).toBe(changed.id);
      expect(ids.slice(1)).toEqual([changed.id, changed.id, changed.id]);
    } finally {
      await fs.rm(ccsDir, { recursive: true, force: true });
    }
  });
});
