import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs/promises';
import os from 'os';
import path from 'path';
import { UsageFailoverService } from '../../../src/web-server/services/usage-failover-service';
import type { DashboardAccount } from '../../../src/web-server/services/account-dashboard-types';
import type {
  UsageCandidate,
  UsageCollection,
  UsageFailoverTarget,
  UsageHelperRequest,
  UsageHelperResponse,
  UsageHost,
  UsageReading,
} from '../../../src/web-server/services/usage-failover-contract';

const BASE_TIME = Date.parse('2026-10-14T12:00:00.000Z');
const MAC: UsageHost = { host: 'mac', platform: 'mac', sshHost: 'fixture-mac' };
const WINDOWS: UsageHost = { host: 'windows', platform: 'windows', sshHost: 'fixture-windows' };
const UBUNTU: UsageHost = { host: 'ubuntu', platform: 'ubuntu', sshHost: 'fixture-ubuntu' };
const scopes: string[] = [];

afterEach(async () => {
  await Promise.all(
    scopes.splice(0).map((scope) => fs.rm(scope, { recursive: true, force: true }))
  );
});

function candidate(overrides: Partial<UsageCandidate> = {}): UsageCandidate {
  return {
    locator: '1'.repeat(64),
    selectors: ['discover'],
    identity: { kind: 'muse-team', key: 'a'.repeat(64) },
    revision: '2'.repeat(64),
    email: 'same@example.test',
    plan: 'Pro',
    ...overrides,
  };
}

function reading(at = BASE_TIME, overrides: Partial<UsageReading> = {}): UsageReading {
  return {
    status: 'ok',
    email: 'same@example.test',
    plan: 'Pro',
    fetchedAt: new Date(at).toISOString(),
    sampledAt: new Date(at).toISOString(),
    windows: [
      {
        key: 'weekly',
        label: 'Weekly',
        usedPercent: 0,
        remainingPercent: 100,
        used: 0,
        limit: 100,
        resetAt: '2026-10-18T12:00:00.000Z',
        windowMinutes: 10080,
        unit: 'credits',
      },
    ],
    ...overrides,
  };
}

function template(): DashboardAccount {
  return {
    id: 'muse:usage',
    provider: 'muse',
    providerLabel: 'Muse Code',
    label: 'Personal Muse',
    email: null,
    plan: null,
    platform: 'mac',
    source: 'Account on Mac',
    status: 'unavailable',
    message: null,
    fetchedAt: null,
    sampledAt: null,
    windows: [],
    isActive: false,
    capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
  };
}

async function harness() {
  const ccsDir = await fs.mkdtemp(path.join(os.tmpdir(), 'aac-failover-service-'));
  scopes.push(ccsDir);
  let now = BASE_TIME;
  const calls: Array<{ host: string; request: UsageHelperRequest }> = [];
  const inventories = new Map<string, UsageCandidate[] | Error>([
    ['mac', [candidate()]],
    ['windows', [candidate({ locator: '3'.repeat(64), revision: '4'.repeat(64) })]],
    ['ubuntu', []],
  ]);
  const collections = new Map<string, UsageCollection | Error>([
    ['mac', { sample: reading(), failureCode: null }],
    ['windows', { sample: reading(), failureCode: null }],
  ]);
  const transport = async (
    host: UsageHost,
    request: UsageHelperRequest
  ): Promise<UsageHelperResponse> => {
    calls.push({ host: host.host, request });
    if (request.action === 'inventory') {
      const value = inventories.get(host.host) ?? [];
      if (value instanceof Error) throw value;
      return { version: 1, action: 'inventory', candidates: structuredClone(value) };
    }
    const value = collections.get(host.host) ?? { sample: null, failureCode: 'unavailable' };
    if (value instanceof Error) throw value;
    return {
      version: 1,
      action: 'collect',
      candidate: structuredClone(request.candidate),
      ...structuredClone(value),
    };
  };
  const target: UsageFailoverTarget = {
    ccsDir,
    accountId: 'muse:usage',
    provider: 'muse',
    binding: 'fixture-source-and-credential',
    primarySources: [{ ...MAC, selector: 'discover' }],
  };
  const deps = { transport, hosts: async () => [MAC, UBUNTU, WINDOWS], now: () => now };
  return {
    service: new UsageFailoverService(deps),
    deps,
    target,
    calls,
    inventories,
    collections,
    advance: (milliseconds = 31_000) => (now += milliseconds),
    now: () => now,
    collected: () => calls.filter((call) => call.request.action === 'collect'),
  };
}

describe('identity-bound usage source failover', () => {
  it('falls back to the verified account and never calls a peer with the same email but another identity', async () => {
    const h = await harness();
    h.collections.set('mac', new Error('offline'));
    h.inventories.set('ubuntu', [
      candidate({ identity: { kind: 'muse-team', key: 'b'.repeat(64) } }),
    ]);
    const row = await h.service.collect(h.target, template());
    expect(row).toMatchObject({ id: 'muse:usage', platform: 'windows', status: 'ok' });
    expect(row.source).toContain('Windows');
    expect(row.source).toContain('fallback');
    expect(row.windows[0].usedPercent).toBe(0);
    expect(h.collected().map((call) => call.host)).toEqual(['mac', 'windows']);
  });

  it('can recover from a completely offline primary using a previously verified account anchor', async () => {
    const h = await harness();
    await h.service.collect(h.target, template());
    h.advance();
    h.inventories.set('mac', new Error('machine offline'));
    h.collections.set('windows', { sample: reading(h.now()), failureCode: null });
    const row = await h.service.collect(h.target, template());
    expect(row.platform).toBe('windows');
    expect(row.sampledAt).toBe(new Date(h.now()).toISOString());
  });

  it('does not bootstrap an account from a peer when the configured source has never established identity', async () => {
    const h = await harness();
    h.inventories.set('mac', new Error('machine offline'));
    const row = await h.service.collect(h.target, template());
    expect(row.windows).toHaveLength(0);
    expect(h.collected()).toHaveLength(0);
  });

  it('allows a trusted explicit account identity to find peers when its primary is offline', async () => {
    const h = await harness();
    h.inventories.set('mac', new Error('machine offline'));
    h.target.expectedIdentity = candidate().identity;
    h.target.expectedEmail = 'same@example.test';
    const row = await h.service.collect(h.target, template());
    expect(row.platform).toBe('windows');
    expect(row.windows).toHaveLength(1);
  });

  it('rejects live sample identity conflicts even when inventory claimed the expected account', async () => {
    const h = await harness();
    h.target.expectedEmail = 'same@example.test';
    h.collections.set('mac', {
      sample: reading(BASE_TIME, { email: 'wrong@example.test' }),
      failureCode: null,
    });
    h.inventories.set('windows', []);
    const row = await h.service.collect(h.target, template());
    expect(row.windows).toHaveLength(0);
  });

  it('rejects a changed revision in the helper response before considering its quota', async () => {
    const h = await harness();
    const service = new UsageFailoverService({
      ...h.deps,
      transport: async (host, request) => {
        const result = await h.deps.transport(host, request);
        return result.action === 'collect' && result.candidate
          ? { ...result, candidate: { ...result.candidate, revision: 'f'.repeat(64) } }
          : result;
      },
    });
    expect((await service.collect(h.target, template())).windows).toHaveLength(0);
  });

  it('retains the last observation with its original time across a restart and offline refresh', async () => {
    const h = await harness();
    const first = await h.service.collect(h.target, template());
    h.advance(5 * 60_000);
    h.inventories.set('mac', new Error('offline'));
    h.inventories.set('windows', new Error('offline'));
    const restarted = new UsageFailoverService(h.deps);
    const cached = await restarted.collect(h.target, template());
    expect(cached.status).toBe('cached');
    expect(cached.sampledAt).toBe(first.sampledAt);
    expect(cached.fetchedAt).toBe(first.fetchedAt);
    expect(cached.windows).toEqual(first.windows);
  });

  it('isolates persisted observations when the configured source binding changes', async () => {
    const h = await harness();
    await h.service.collect(h.target, template());
    h.advance();
    h.inventories.set('mac', new Error('offline'));
    const changed = { ...h.target, binding: 'another-source-credential' };
    const restarted = new UsageFailoverService(h.deps);
    expect((await restarted.collect(changed, template())).windows).toHaveLength(0);
  });

  it('never reuses another primary login after the default credential switches account', async () => {
    const h = await harness();
    await h.service.collect(h.target, template());
    h.advance();
    h.inventories.set('mac', [candidate({ identity: { kind: 'muse-team', key: 'b'.repeat(64) } })]);
    h.collections.set('mac', { sample: null, failureCode: 'network_error' });
    const before = h.collected().length;
    const row = await h.service.collect(h.target, template());
    expect(row.windows).toHaveLength(0);
    expect(
      h
        .collected()
        .slice(before)
        .some((call) => call.host === 'windows')
    ).toBe(false);
  });

  it('retains dated quota when credentials renew for the same verified account', async () => {
    const h = await harness();
    await h.service.collect(h.target, template());
    h.advance();
    h.inventories.set('mac', [candidate({ revision: 'e'.repeat(64) })]);
    h.collections.set('mac', { sample: null, failureCode: 'network_error' });
    h.inventories.set('windows', []);
    const row = await h.service.collect(h.target, template());
    expect(row.status).toBe('cached');
    expect(row.sampledAt).toBe(new Date(BASE_TIME).toISOString());
    expect(row.windows).toHaveLength(1);
  });

  it('drops browser extras on Claude token renewal while retaining dated core quota', async () => {
    const h = await harness();
    const target = { ...h.target, provider: 'claude' as const, accountId: 'claude:personal' };
    const claude = { ...template(), provider: 'claude' as const, id: target.accountId };
    const sample = reading(BASE_TIME, { optionalExtrasContext: 'c'.repeat(64) });
    sample.windows.push({ ...sample.windows[0], key: 'prepaid_balance', remaining: 5 });
    h.collections.set('mac', { sample, failureCode: null });
    await h.service.collect(target, claude);
    h.advance();
    h.inventories.set('mac', [candidate({ revision: 'e'.repeat(64) })]);
    h.collections.set('mac', { sample: null, failureCode: 'network_error' });
    h.inventories.set('windows', []);
    const row = await h.service.collect(target, claude);
    expect(row.windows.map((window) => window.key)).toEqual(['weekly']);
    expect(row.status).toBe('cached');
    expect(row.sampledAt).toBe(new Date(BASE_TIME).toISOString());
  });

  it('keeps cached quota accompanying a 429 and blocks every peer through restart', async () => {
    const h = await harness();
    h.collections.set('mac', {
      sample: reading(BASE_TIME, { status: 'cached' }),
      failureCode: 'rate_limited',
      retryAfterSeconds: 120,
    });
    const row = await h.service.collect(h.target, template());
    expect(row.status).toBe('cached');
    expect(row.sampledAt).toBe(new Date(BASE_TIME).toISOString());
    expect(h.collected().map((call) => call.host)).toEqual(['mac']);
    const count = h.calls.length;
    h.advance(119_000);
    const restored = await new UsageFailoverService(h.deps).collect(h.target, template());
    expect(restored.status).toBe('cached');
    expect(h.calls).toHaveLength(count);
  });

  it('keeps the original account cooldown when in-flight rotation rejects its quota', async () => {
    const h = await harness();
    h.collections.set('mac', {
      sample: null,
      failureCode: 'source_changed',
      rateLimited: true,
      retryAfterSeconds: 1800,
    });
    const row = await h.service.collect(h.target, template());
    expect(row.windows).toHaveLength(0);
    expect(h.collected().map((call) => call.host)).toEqual(['mac']);
    h.advance(31_000);
    h.inventories.set('mac', [candidate({ revision: 'e'.repeat(64) })]);
    h.collections.set('mac', { sample: reading(h.now()), failureCode: null });
    await new UsageFailoverService(h.deps).collect(h.target, template());
    expect(h.collected()).toHaveLength(1);
    h.advance(1_768_000);
    await new UsageFailoverService(h.deps).collect(h.target, template());
    expect(h.collected()).toHaveLength(1);
    h.inventories.set('mac', [candidate({ identity: { kind: 'muse-team', key: 'b'.repeat(64) } })]);
    const other = { ...h.target, binding: 'different-verified-account' };
    expect((await new UsageFailoverService(h.deps).collect(other, template())).status).toBe('ok');
    expect(h.collected()).toHaveLength(2);
  });

  for (const failureCode of ['network_error', 'rate_limited'])
    it(`never replaces newer peer quota with an old native cache during ${failureCode}`, async () => {
      const h = await harness();
      const target = { ...h.target, provider: 'codex' as const, accountId: 'codex:fixture' };
      const identity = { kind: 'codex-principal-v1', key: 'a'.repeat(64) };
      h.inventories.set('mac', [candidate({ identity })]);
      h.inventories.set('windows', [candidate({ identity, locator: '3'.repeat(64) })]);
      const oldNative: DashboardAccount = {
        ...template(),
        ...reading(BASE_TIME - 2 * 86400_000),
        id: target.accountId,
        provider: 'codex',
        status: 'cached',
      };
      const first = await h.service.recover(target, oldNative);
      expect(first.sampledAt).toBe(new Date(BASE_TIME).toISOString());
      expect(first.source).toContain('fallback');
      h.advance();
      h.collections.set('windows', { sample: null, failureCode, retryAfterSeconds: 1800 });
      const next = await h.service.recover(target, oldNative);
      for (const row of [
        next,
        h.service.peek(target, oldNative),
        new UsageFailoverService(h.deps).peek(target, oldNative),
      ]) {
        expect(row.status).toBe('cached');
        expect(row.sampledAt).toBe(first.sampledAt);
        expect(row.source).toBe(first.source);
      }
    });

  it('respects a healthy collector refresh budget without trying another host', async () => {
    const h = await harness();
    h.collections.set('mac', {
      sample: reading(BASE_TIME, { status: 'cached' }),
      failureCode: null,
      refreshAfterSeconds: 270,
    });
    expect((await h.service.collect(h.target, template())).status).toBe('cached');
    expect(h.collected().map((call) => call.host)).toEqual(['mac']);
    const count = h.calls.length;
    h.advance(269_000);
    await new UsageFailoverService(h.deps).collect(h.target, template());
    expect(h.calls).toHaveLength(count);
  });

  it('clears changed-plan quota while preserving a simultaneous provider cooldown', async () => {
    const h = await harness();
    await h.service.collect(h.target, template());
    h.advance();
    h.collections.set('mac', {
      sample: null,
      failureCode: 'rate_limited',
      retryAfterSeconds: 120,
      observedPlan: 'Max',
      observedPlanAt: new Date(h.now()).toISOString(),
    });
    const row = await h.service.collect(h.target, template());
    expect(row.windows).toHaveLength(0);
    expect(row.message).toContain('rate limited');
    const count = h.collected().length;
    h.advance(119_000);
    await new UsageFailoverService(h.deps).collect(h.target, template());
    expect(h.collected()).toHaveLength(count);
  });

  it('rejects cached Muse quota for an obsolete plan after restart but accepts a verified new plan', async () => {
    const h = await harness();
    await h.service.collect(h.target, template());
    h.advance();
    h.collections.set('mac', {
      sample: null,
      failureCode: 'network_error',
      observedPlan: 'Max',
      observedPlanAt: new Date(h.now()).toISOString(),
    });
    h.collections.set('windows', {
      sample: reading(BASE_TIME, { status: 'cached' }),
      failureCode: null,
    });
    expect((await h.service.collect(h.target, template())).windows).toHaveLength(0);
    h.advance();
    h.inventories.set('mac', new Error('offline'));
    const restarted = new UsageFailoverService(h.deps);
    expect((await restarted.collect(h.target, template())).windows).toHaveLength(0);
    h.advance();
    h.collections.set('windows', { sample: reading(h.now(), { plan: 'Max' }), failureCode: null });
    const row = await restarted.collect(h.target, template());
    expect(row.status).toBe('ok');
    expect(row.plan).toBe('Max');
    expect(row).not.toHaveProperty('observedPlan');
  });

  it('retains the dated Muse quota when a failed refresh verifies the same plan', async () => {
    const h = await harness();
    await h.service.collect(h.target, template());
    h.advance();
    h.collections.set('mac', {
      sample: null,
      failureCode: 'network_error',
      observedPlan: 'Pro',
      observedPlanAt: new Date(h.now()).toISOString(),
    });
    h.inventories.set('windows', []);
    const row = await h.service.collect(h.target, template());
    expect(row.status).toBe('cached');
    expect(row.sampledAt).toBe(new Date(BASE_TIME).toISOString());
    expect(row).not.toHaveProperty('observedPlan');
  });

  it('automatically replaces Cursor legacy and X-plan meters using the verified current plan', async () => {
    const h = await harness();
    const target: UsageFailoverTarget = {
      ...h.target,
      provider: 'cursor',
      accountId: 'cursor:usage',
    };
    const rowTemplate: DashboardAccount = { ...template(), provider: 'cursor', id: 'cursor:usage' };
    // Local desktop membership can lag the server-side subscription change.
    h.inventories.set('mac', [candidate({ plan: 'ultra' })]);
    for (const [plan, key, windowMinutes] of [
      ['ultra', 'apiPercentUsed', null],
      ['premium', 'grok-shared-weekly', 10080],
      ['unified_ultra', 'grok-shared-weekly', 10080],
      ['pro', 'apiPercentUsed', null],
    ] as const) {
      h.advance();
      h.collections.set('mac', {
        sample: reading(h.now(), {
          plan,
          windows: [{ ...reading().windows[0], key, windowMinutes }],
        }),
        failureCode: null,
        observedPlan: plan,
        observedPlanAt: new Date(h.now()).toISOString(),
      });
      const row = await h.service.collect(target, rowTemplate);
      expect(row.plan).toBe(plan);
      expect(row.windows.map((window) => window.key)).toEqual([key]);
      expect(row).not.toHaveProperty('observedPlanAt');
    }
  });

  it('keeps Cursor plan changes across restart without restoring obsolete legacy quota', async () => {
    const h = await harness();
    const target: UsageFailoverTarget = {
      ...h.target,
      provider: 'cursor',
      accountId: 'cursor:usage',
    };
    const rowTemplate: DashboardAccount = { ...template(), provider: 'cursor', id: 'cursor:usage' };
    await h.service.collect(target, rowTemplate);
    h.advance();
    h.collections.set('mac', {
      sample: null,
      failureCode: 'rate_limited',
      retryAfterSeconds: 1800,
      observedPlan: 'premium',
      observedPlanAt: new Date(h.now()).toISOString(),
    });
    const changed = await h.service.collect(target, rowTemplate);
    expect(changed.plan).toBe('premium');
    expect(changed.windows).toHaveLength(0);
    const count = h.collected().length;
    h.advance(1799_000);
    const restarted = new UsageFailoverService(h.deps);
    const held = await restarted.collect(target, rowTemplate);
    expect(held.plan).toBe('premium');
    expect(held.windows).toHaveLength(0);
    expect(h.collected()).toHaveLength(count);
    h.advance(1001);
    h.inventories.set('mac', new Error('offline'));
    h.collections.set('windows', {
      sample: reading(BASE_TIME, { status: 'cached' }),
      failureCode: null,
      observedPlan: 'Pro',
      observedPlanAt: new Date(BASE_TIME).toISOString(),
    });
    const stale = await restarted.collect(target, rowTemplate);
    expect(stale.plan).toBe('premium');
    expect(stale.windows).toHaveLength(0);
    h.advance();
    h.collections.set('windows', {
      sample: reading(h.now(), {
        plan: 'premium',
        windows: [{ ...reading().windows[0], key: 'grok-shared-weekly' }],
      }),
      failureCode: null,
    });
    const current = await restarted.collect(target, rowTemplate);
    expect(current.plan).toBe('premium');
    expect(current.windows[0].key).toBe('grok-shared-weekly');
  });

  it('still tries a matching peer when cached quota accompanies a transient failure', async () => {
    const h = await harness();
    h.collections.set('mac', {
      sample: reading(BASE_TIME, { status: 'cached' }),
      failureCode: 'network_error',
    });
    const row = await h.service.collect(h.target, template());
    expect(row.status).toBe('ok');
    expect(row.platform).toBe('windows');
    expect(h.collected().map((call) => call.host)).toEqual(['mac', 'windows']);
  });

  it('keeps newer peer quota when an older verified Muse plan returns after restart', async () => {
    const h = await harness();
    await h.service.collect(h.target, template());
    h.advance();
    h.collections.set('mac', new Error('offline'));
    h.collections.set('windows', { sample: reading(h.now(), { plan: 'Max' }), failureCode: null });
    const latest = await h.service.collect(h.target, template());
    h.advance();
    h.collections.set('mac', {
      sample: null,
      failureCode: 'network_error',
      observedPlan: 'Pro',
      observedPlanAt: new Date(BASE_TIME).toISOString(),
    });
    h.collections.set('windows', new Error('offline'));
    const restarted = new UsageFailoverService(h.deps);
    const row = await restarted.collect(h.target, template());
    expect(row.status).toBe('cached');
    expect(row.plan).toBe('Max');
    expect(row.sampledAt).toBe(latest.sampledAt);
    expect(row.windows).toEqual(latest.windows);
    expect(row).not.toHaveProperty('observedPlanAt');
    h.advance();
    h.collections.set('mac', { sample: reading(BASE_TIME), failureCode: null });
    expect((await restarted.collect(h.target, template())).plan).toBe('Max');
  });

  it('does not invalidate Muse quota from unproven or future plan metadata', async () => {
    for (const observedPlanAt of [undefined, 'invalid', '2099-01-01T00:00:00Z']) {
      const h = await harness();
      await h.service.collect(h.target, template());
      h.advance();
      h.collections.set('mac', {
        sample: null,
        failureCode: 'network_error',
        observedPlan: 'Max',
        observedPlanAt,
      });
      h.collections.set('windows', new Error('offline'));
      const row = await h.service.collect(h.target, template());
      expect(row.status).toBe('cached');
      expect(row.plan).toBe('Pro');
    }
  });

  it('accepts a newer exact-identity peer plan while preserving its provider cooldown', async () => {
    const h = await harness();
    await h.service.collect(h.target, template());
    h.advance();
    h.inventories.set('mac', new Error('offline'));
    h.collections.set('windows', {
      sample: null,
      failureCode: 'rate_limited',
      retryAfterSeconds: 1800,
      observedPlan: 'Ultra',
      observedPlanAt: new Date(h.now()).toISOString(),
    });
    const row = await h.service.collect(h.target, template());
    expect(row.windows).toHaveLength(0);
    const count = h.collected().length;
    h.advance(1799_000);
    const restarted = new UsageFailoverService(h.deps);
    expect((await restarted.collect(h.target, template())).windows).toHaveLength(0);
    expect(h.collected()).toHaveLength(count);
    h.advance(1001);
    h.collections.set('windows', {
      sample: reading(h.now(), { plan: 'Ultra' }),
      failureCode: null,
    });
    expect((await restarted.collect(h.target, template())).plan).toBe('Ultra');
  });

  it('ignores an exact-identity peer plan older than the saved quota', async () => {
    const h = await harness();
    h.advance();
    h.collections.set('mac', { sample: reading(h.now(), { plan: 'Ultra' }), failureCode: null });
    const latest = await h.service.collect(h.target, template());
    h.advance();
    h.inventories.set('mac', new Error('offline'));
    h.collections.set('windows', {
      sample: null,
      failureCode: 'network_error',
      observedPlan: 'Pro',
      observedPlanAt: new Date(BASE_TIME).toISOString(),
    });
    const row = await new UsageFailoverService(h.deps).collect(h.target, template());
    expect(row.plan).toBe('Ultra');
    expect(row.sampledAt).toBe(latest.sampledAt);
    expect(row.windows).toEqual(latest.windows);
  });

  it('does not hide a primary renewal message behind fallback or retained quota', async () => {
    const h = await harness();
    await h.service.collect(h.target, template());
    h.advance();
    const renewal: DashboardAccount = {
      ...template(),
      status: 'needs_sign_in',
      message: 'Renew the configured login.',
    };
    const row = await h.service.recover(h.target, renewal);
    expect(row.status).toBe('needs_sign_in');
    expect(row.message).toBe(renewal.message);
    expect(row.windows).toHaveLength(1);
    expect(h.service.peek(h.target, renewal).status).toBe('needs_sign_in');
  });

  it('maps actionable primary browser failures even when no quota has ever succeeded', async () => {
    const h = await harness();
    h.collections.set('mac', { sample: null, failureCode: 'choose_team' });
    h.inventories.set('windows', []);
    const row = await h.service.collect(h.target, template());
    expect(row.status).toBe('needs_sign_in');
    expect(row.message).toContain('Choose a team');
  });

  it('invalidates the original identity and stops peers when the primary changes during collection', async () => {
    const h = await harness();
    await h.service.collect(h.target, template());
    h.advance();
    h.collections.set('mac', { sample: null, failureCode: 'source_changed' });
    const count = h.collected().length;
    const row = await h.service.collect(h.target, template());
    expect(row.windows).toHaveLength(0);
    expect(
      h
        .collected()
        .slice(count)
        .map((call) => call.host)
    ).toEqual(['mac']);
  });

  it('preserves native Antigravity activation eligibility but cannot grant it from fallback', async () => {
    const h = await harness();
    const target = { ...h.target, provider: 'antigravity' as const };
    const native: DashboardAccount = {
      ...template(),
      ...reading(),
      provider: 'antigravity',
      status: 'cached',
      capabilities: { ...template().capabilities, antigravityCanActivate: true },
    };
    h.inventories.set('windows', []);
    expect((await h.service.recover(target, native)).capabilities.antigravityCanActivate).toBe(
      true
    );
    h.advance();
    h.inventories.set('windows', [candidate({ locator: '3'.repeat(64) })]);
    const peer = await h.service.recover(target, { ...native, status: 'unavailable', windows: [] });
    expect(peer.capabilities.antigravityCanActivate).toBe(false);
  });

  it('forwards private Claude context to its host and isolates inventories between profiles', async () => {
    const h = await harness();
    const target = (id: string): UsageFailoverTarget => ({
      ...h.target,
      provider: 'claude',
      accountId: `claude:${id}`,
      primarySources: [
        {
          ...MAC,
          selector: 'discover',
          claudeProfile: { id, email: 'same@example.test', profilePath: `C:\\Private\\${id}` },
        },
      ],
    });
    await h.service.collect(target('one'), template());
    await h.service.collect(target('two'), template());
    const inventories = h.calls.filter((call) => call.request.action === 'inventory');
    expect(inventories).toHaveLength(2);
    expect(inventories.map((call) => call.request.claudeProfile?.id)).toEqual(['one', 'two']);
    expect(h.collected().map((call) => call.request.claudeProfile?.id)).toEqual(['one', 'two']);
  });

  it('does not revive a removed primary credential from another host', async () => {
    const h = await harness();
    await h.service.collect(h.target, template());
    h.advance();
    h.inventories.set('mac', []);
    const before = h.collected().length;
    expect((await h.service.collect(h.target, template())).windows).toHaveLength(0);
    expect(h.collected()).toHaveLength(before);
  });

  it('rebinds a changed discover slot instead of retaining the removed account on a peer', async () => {
    const h = await harness();
    await h.service.collect(h.target, template());
    h.advance();
    h.inventories.set('mac', [
      candidate({
        locator: '7'.repeat(64),
        identity: { kind: 'muse-team', key: 'b'.repeat(64) },
        email: 'replacement@example.test',
      }),
    ]);
    h.collections.set('mac', { sample: null, failureCode: 'network_error' });
    h.calls.splice(0);
    const row = await h.service.collect(h.target, template());
    expect(row.status).toBe('unavailable');
    expect(row.windows).toEqual([]);
    expect(h.collected().map((call) => call.host)).toEqual(['mac']);
  });

  for (const failureCode of [
    'needs_sign_in',
    'account_mismatch',
    'identity_unavailable',
    'source_changed',
  ]) {
    it(`clears rejected primary observations after ${failureCode}, including after restart`, async () => {
      const h = await harness();
      await h.service.collect(h.target, template());
      h.advance();
      h.collections.set('mac', { sample: null, failureCode });
      h.inventories.set('windows', []);
      expect((await h.service.collect(h.target, template())).windows).toHaveLength(0);
      h.advance();
      h.inventories.set('mac', new Error('offline'));
      const restarted = new UsageFailoverService(h.deps);
      expect((await restarted.collect(h.target, template())).windows).toHaveLength(0);
    });
  }

  it('stops all provider calls on 429 and keeps the same cooldown after restart', async () => {
    const h = await harness();
    h.collections.set('mac', { sample: null, failureCode: 'rate_limited', retryAfterSeconds: 900 });
    await h.service.collect(h.target, template());
    expect(h.collected().map((call) => call.host)).toEqual(['mac']);
    h.advance(60_000);
    const restarted = new UsageFailoverService(h.deps);
    await restarted.collect(h.target, template());
    expect(h.collected().map((call) => call.host)).toEqual(['mac']);
    h.advance(901_000);
    h.collections.set('mac', { sample: reading(h.now()), failureCode: null });
    expect((await restarted.collect(h.target, template())).status).toBe('ok');
    expect(h.collected().map((call) => call.host)).toEqual(['mac', 'mac']);
  });

  it('shares a provider cooldown with another account row bound to the same credential', async () => {
    const h = await harness();
    h.collections.set('mac', { sample: null, failureCode: 'rate_limited', retryAfterSeconds: 900 });
    await h.service.collect(h.target, template());
    await h.service.collect(
      { ...h.target, accountId: 'muse:acct:1234abcd', binding: 'same-account-other-row' },
      template()
    );
    expect(h.collected()).toHaveLength(1);
  });

  it('keeps a shared identity cooldown after restart when a different row asks for the same account', async () => {
    const h = await harness();
    h.collections.set('mac', {
      sample: null,
      failureCode: 'rate_limited',
      retryAfterSeconds: 900,
    });
    await h.service.collect(h.target, template());
    h.advance(60_000);
    const secondRow = {
      ...h.target,
      accountId: 'muse:acct:1234abcd',
      binding: 'same-account-different-row-after-restart',
    };
    const restarted = new UsageFailoverService(h.deps);
    await restarted.collect(secondRow, template());
    expect(h.collected()).toHaveLength(1);
    h.advance(901_000);
    h.collections.set('mac', { sample: reading(h.now()), failureCode: null });
    expect((await restarted.collect(secondRow, template())).status).toBe('ok');
    expect(h.collected()).toHaveLength(2);
  });

  it('stops queued and later peer requests when another row receives an identity-wide cooldown', async () => {
    const h = await harness();
    const targets = Array.from({ length: 4 }, (_, index) => ({
      ...h.target,
      accountId: `muse:concurrent-${index}`,
    }));
    const active: Array<{
      candidate: UsageCandidate;
      resolve: (response: UsageHelperResponse) => void;
    }> = [];
    let ready!: () => void;
    const saturated = new Promise<void>((resolve) => {
      ready = resolve;
    });
    const calls: UsageHelperRequest[] = [];
    const transport = async (
      host: UsageHost,
      request: UsageHelperRequest
    ): Promise<UsageHelperResponse> => {
      calls.push(request);
      if (request.action === 'inventory') return h.deps.transport(host, request);
      return new Promise<UsageHelperResponse>((resolve) => {
        active.push({ candidate: request.candidate, resolve });
        if (active.length === 3) ready();
      });
    };
    const deps = { ...h.deps, transport };
    const service = new UsageFailoverService(deps);
    const pending = targets.map((target) => service.collect(target, template()));
    await saturated;
    // All four refreshes now passed the initial check; three hold transport
    // slots and the fourth waits for a slot before it can start collecting.
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    active[0].resolve({
      version: 1,
      action: 'collect',
      candidate: active[0].candidate,
      sample: null,
      failureCode: 'rate_limited',
      retryAfterSeconds: 120,
    });
    await pending[0];
    for (const request of active.slice(1))
      request.resolve({
        version: 1,
        action: 'collect',
        candidate: request.candidate,
        sample: null,
        failureCode: 'network_error',
      });
    await Promise.all(pending);
    expect(calls.filter((request) => request.action === 'collect')).toHaveLength(3);
    const totalCalls = calls.length;
    h.advance(119_000);
    await new UsageFailoverService(deps).collect(targets[3], template());
    expect(calls).toHaveLength(totalCalls);
  });

  it('coalesces simultaneous reads of one logical account', async () => {
    const h = await harness();
    const rows = await Promise.all(
      Array.from({ length: 12 }, () => h.service.collect(h.target, template()))
    );
    expect(h.collected()).toHaveLength(1);
    expect(h.calls.filter((call) => call.request.action === 'inventory')).toHaveLength(1);
    expect(rows.every((row) => row.windows[0].usedPercent === 0)).toBe(true);
  });

  it('preserves the account id and launcher capabilities while quota comes from another machine', async () => {
    const h = await harness();
    h.collections.set('mac', { sample: null, failureCode: 'network_error' });
    const claude: DashboardAccount = {
      ...template(),
      id: 'claude:personal',
      provider: 'claude',
      capabilities: { codexProfile: null, claudeProfileId: 'personal', claudePlatforms: ['mac'] },
    };
    const target = { ...h.target, accountId: claude.id, provider: 'claude' as const };
    const row = await h.service.collect(target, claude);
    expect(row.id).toBe(claude.id);
    expect(row.platform).toBe('mac');
    expect(row.source).toContain('Windows');
    expect(row.capabilities).toEqual(claude.capabilities);
  });

  it('selects the expected account when multiple inventory entries share a selector', async () => {
    const h = await harness();
    h.target.expectedEmail = 'same@example.test';
    h.target.expectedIdentity = candidate().identity;
    h.inventories.set('mac', [
      candidate({
        email: 'wrong@example.test',
        identity: { kind: 'muse-team', key: 'b'.repeat(64) },
      }),
      candidate(),
    ]);
    expect((await h.service.collect(h.target, template())).status).toBe('ok');
    expect(h.collected()).toHaveLength(1);
    expect(h.collected()[0].request).toMatchObject({ candidate: { email: 'same@example.test' } });
  });

  it('allows a strongly bound candidate without inventory email, then verifies live email', async () => {
    const h = await harness();
    h.target.expectedEmail = 'same@example.test';
    h.target.expectedIdentity = candidate().identity;
    h.inventories.set('mac', [candidate({ email: null })]);
    expect((await h.service.collect(h.target, template())).status).toBe('ok');
    h.advance();
    h.collections.set('mac', {
      sample: reading(h.now(), { email: 'wrong@example.test' }),
      failureCode: null,
    });
    h.inventories.set('windows', []);
    const row = await h.service.collect(h.target, template());
    expect(row.status).toBe('cached');
    expect(row.email).toBe('same@example.test');
    expect(row.sampledAt).toBe(new Date(BASE_TIME).toISOString());
  });

  it('refuses a wallet workspace that does not match the canonical account id', async () => {
    const h = await harness();
    h.target.provider = 'opencode-wallet';
    h.target.accountId = 'plan-opencode-go-console-mac-aaaaaaaaaaaa';
    h.inventories.set('mac', [
      candidate({ identity: { kind: 'opencode-workspace', key: 'b'.repeat(64) } }),
    ]);
    expect((await h.service.collect(h.target, template())).status).toBe('unavailable');
    expect(h.collected()).toHaveLength(0);
  });

  it('does not use a mismatching wallet peer even when an explicit identity was supplied', async () => {
    const h = await harness();
    h.target.provider = 'opencode-wallet';
    h.target.accountId = 'plan-opencode-go-console-mac-aaaaaaaaaaaa';
    h.target.expectedIdentity = { kind: 'opencode-workspace', key: 'b'.repeat(64) };
    h.inventories.set('mac', new Error('offline'));
    h.inventories.set('windows', [candidate({ identity: h.target.expectedIdentity })]);
    expect((await h.service.collect(h.target, template())).status).toBe('unavailable');
    expect(h.collected()).toHaveLength(0);
  });

  it('keeps primary Claude sign-in failures visible through fallback and cached output', async () => {
    const h = await harness();
    const target = {
      ...h.target,
      provider: 'claude' as const,
      accountId: 'claude:personal',
      primarySources: [
        { ...WINDOWS, selector: 'discover' },
        { ...MAC, selector: 'discover' },
      ],
    };
    const claude = { ...template(), provider: 'claude' as const, id: target.accountId };
    h.collections.set('windows', { sample: null, failureCode: 'needs_sign_in' });
    const row = await h.service.collect(target, claude);
    expect(row.status).toBe('ok');
    expect(row.signInNeeded).toEqual(['windows']);
    expect(h.service.peek(target, claude).signInNeeded).toEqual(['windows']);
    expect(new UsageFailoverService(h.deps).peek(target, claude).signInNeeded).toEqual(['windows']);
    h.advance();
    h.collections.set('windows', { sample: reading(h.now()), failureCode: null });
    expect((await h.service.collect(target, claude)).signInNeeded).toBeUndefined();
  });

  it('shows Claude sign-in failures even when every usage source is unavailable', async () => {
    const h = await harness();
    const target = { ...h.target, provider: 'claude' as const, accountId: 'claude:personal' };
    h.collections.set('mac', { sample: null, failureCode: 'needs_sign_in' });
    h.inventories.set('windows', []);
    const row = await h.service.collect(target, {
      ...template(),
      provider: 'claude',
      id: target.accountId,
    });
    expect(row.status).toBe('needs_sign_in');
    expect(row.signInNeeded).toEqual(['mac']);
  });

  it('preserves signed-out Claude inventory slots even though they have no credential candidate', async () => {
    const h = await harness();
    const target = {
      ...h.target,
      provider: 'claude' as const,
      accountId: 'claude:personal',
      expectedIdentity: candidate().identity,
      primarySources: [
        { ...WINDOWS, selector: 'claude-desktop-profile:personal' },
        { ...MAC, selector: 'discover' },
      ],
    };
    const transport = async (
      host: UsageHost,
      request: UsageHelperRequest
    ): Promise<UsageHelperResponse> => {
      if (host.host === 'windows' && request.action === 'inventory')
        return {
          version: 1,
          action: 'inventory',
          candidates: [],
          signInNeededSelectors: ['claude-desktop-profile:personal'],
        };
      return h.deps.transport(host, request);
    };
    const row = await new UsageFailoverService({ ...h.deps, transport }).collect(target, {
      ...template(),
      provider: 'claude',
      id: target.accountId,
    });
    expect(row.status).toBe('ok');
    expect(row.signInNeeded).toEqual(['windows']);
  });

  it('does not apply another Claude profile inventory sign-in notice to this account', async () => {
    const h = await harness();
    const target = {
      ...h.target,
      provider: 'claude' as const,
      accountId: 'claude:personal',
      expectedIdentity: candidate().identity,
      primarySources: [
        { ...WINDOWS, selector: 'claude-desktop-profile:personal' },
        { ...MAC, selector: 'discover' },
      ],
    };
    const transport = async (
      host: UsageHost,
      request: UsageHelperRequest
    ): Promise<UsageHelperResponse> => {
      if (host.host === 'windows' && request.action === 'inventory')
        return {
          version: 1,
          action: 'inventory',
          candidates: [],
          signInNeededSelectors: ['claude-desktop-profile:other'],
        };
      return h.deps.transport(host, request);
    };
    const row = await new UsageFailoverService({ ...h.deps, transport }).collect(target, {
      ...template(),
      provider: 'claude',
      id: target.accountId,
    });
    expect(row.status).toBe('ok');
    expect(row.signInNeeded).toBeUndefined();
  });

  it('retains temporarily unavailable Claude extras and strips their private metadata from output', async () => {
    const h = await harness();
    const target = { ...h.target, provider: 'claude' as const, accountId: 'claude:personal' };
    const claude = { ...template(), provider: 'claude' as const, id: target.accountId };
    const first = reading(BASE_TIME, {
      optionalExtrasContext: 'c'.repeat(64),
      optionalExtras: { resetCredits: 'ok', prepaidBalance: 'ok' },
    });
    first.windows.push({
      ...first.windows[0],
      key: 'prepaid_balance',
      remaining: -3,
      usedPercent: null,
      remainingPercent: null,
    });
    h.collections.set('mac', { sample: first, failureCode: null });
    const fresh = await h.service.collect(target, claude);
    expect(fresh).not.toHaveProperty('optionalExtras');
    expect(fresh).not.toHaveProperty('optionalExtrasContext');
    h.advance();
    h.collections.set('mac', {
      sample: reading(h.now(), {
        optionalExtrasContext: 'c'.repeat(64),
        optionalExtras: { resetCredits: 'ok', prepaidBalance: 'unavailable' },
      }),
      failureCode: null,
    });
    const row = await new UsageFailoverService(h.deps).collect(target, claude);
    expect(row.status).toBe('ok');
    expect(row.windows.find((item) => item.key === 'prepaid_balance')).toMatchObject({
      remaining: -3,
      status: 'cached',
      sampledAt: new Date(BASE_TIME).toISOString(),
    });
    expect(row).not.toHaveProperty('optionalExtras');
    expect(row).not.toHaveProperty('optionalExtrasContext');
    h.advance(24 * 60 * 60 * 1000);
    const restarted = new UsageFailoverService(h.deps);
    for (const projected of [restarted.peek(target, claude), restarted.peek(target, row)]) {
      expect(projected.windows.some((item) => item.key === 'prepaid_balance')).toBe(false);
      expect(projected.windows[0].key).toBe('weekly');
      expect(projected.sampledAt).toBe(row.sampledAt);
    }
  });
});
