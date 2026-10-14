import { describe, expect, it } from 'bun:test';
import fs from 'fs/promises';
import os from 'os';
import path from 'path';
import { credentialFingerprint, identityKey } from '../../../src/antigravity/registry';
import type { DashboardAccount } from '../../../src/web-server/services/account-dashboard-types';
import {
  peekAntigravityUsage,
  recoverAntigravityUsage,
  type AntigravityUsageBinding,
  type AntigravityUsageFailoverDeps,
} from '../../../src/web-server/services/usage-failover-antigravity';
import type { UsageFailoverTarget } from '../../../src/web-server/services/usage-failover-contract';

const STAMP = '2026-10-01T12:00:00.000Z';
function account(overrides: Partial<DashboardAccount> = {}): DashboardAccount {
  return {
    id: 'antigravity:profile:alice',
    provider: 'antigravity',
    providerLabel: 'Antigravity',
    label: 'alice@example.test',
    email: 'alice@example.test',
    plan: 'Pro',
    platform: 'ubuntu',
    source: 'Antigravity saved login on Ubuntu',
    status: 'unavailable',
    message: null,
    fetchedAt: null,
    sampledAt: null,
    isActive: true,
    windows: [],
    capabilities: {
      codexProfile: null,
      claudeProfileId: null,
      claudePlatforms: [],
      antigravityProfileId: 'alice',
      antigravityHostIds: ['ubuntu'],
      antigravityCanActivate: true,
    },
    ...overrides,
  };
}
function reading(
  primary: DashboardAccount,
  overrides: Partial<DashboardAccount> = {}
): DashboardAccount {
  return {
    ...primary,
    status: 'ok',
    fetchedAt: STAMP,
    sampledAt: STAMP,
    source: 'Verified quota on Mac (fallback)',
    platform: 'mac',
    capabilities: { ...primary.capabilities, antigravityCanActivate: false },
    windows: [
      {
        key: 'model',
        label: 'Model quota',
        usedPercent: 25,
        remainingPercent: 75,
        resetAt: null,
        windowMinutes: null,
        used: null,
        limit: null,
        unit: null,
        kind: 'rate_limit',
      },
    ],
    ...overrides,
  };
}
function binding(): AntigravityUsageBinding {
  return {
    email: 'alice@example.test',
    identityKey: '1'.repeat(64),
    credentialRevision: '2'.repeat(64),
  };
}

describe('registered Antigravity display failover', () => {
  it('binds the default reader to the real saved fixture identity and credential revision', async () => {
    const scope = await fs.mkdtemp(path.join(os.tmpdir(), 'aac-agy-failover-'));
    try {
      const credential = { format: 'fixture-json', bytes: Buffer.from('PRIVATE-SAVED-CREDENTIAL') };
      const identity = {
        email: 'alice@example.test',
        subject: '123456789',
        plan: 'Pro',
        verifiedAt: STAMP,
        source: 'provider-userinfo' as const,
      };
      const profiles = path.join(scope, 'antigravity-profiles');
      const instances = path.join(scope, 'antigravity-instances', 'alice', 'ubuntu');
      await fs.mkdir(profiles, { mode: 0o700 });
      await fs.mkdir(instances, { recursive: true, mode: 0o700 });
      await fs.writeFile(
        path.join(instances, `credential-${credentialFingerprint(credential)}.bin`),
        credential.bytes,
        { mode: 0o600 }
      );
      await fs.writeFile(
        path.join(profiles, 'registry-000000000001.json'),
        JSON.stringify({
          version: 1,
          revision: 1,
          active: null,
          transaction: null,
          profiles: [
            {
              id: 'alice',
              email: identity.email,
              plan: identity.plan,
              identityKey: identityKey(identity),
              credential: {
                format: credential.format,
                fingerprint: credentialFingerprint(credential),
                identity,
                savedAt: STAMP,
              },
            },
          ],
        }),
        { mode: 0o600 }
      );
      let target: UsageFailoverTarget | null = null;
      const deps: AntigravityUsageFailoverDeps = {
        failover: {
          recover: async (requested, primary) => {
            target = requested;
            return reading(primary);
          },
          peek: (_, primary) => primary,
        },
      };
      const [result] = await recoverAntigravityUsage([account()], scope, deps);
      expect(target!.expectedIdentity).toEqual({
        kind: 'antigravity-subject',
        key: identityKey(identity),
      });
      expect(target!.binding).toContain(credentialFingerprint(credential));
      expect(target!.primarySources[0].selector).toBe('antigravity-profile:alice');
      expect(target!.primarySources[0].sshHost).toBeNull();
      expect(JSON.stringify(target)).not.toContain('PRIVATE-SAVED-CREDENTIAL');
      expect(result.platform).toBe('ubuntu');
      expect(result.isActive).toBe(true);
      expect(result.source).toContain('Mac (fallback)');
      expect(result.capabilities.antigravityCanActivate).toBe(false);
      expect(result.capabilities.antigravityHostIds).toEqual(['ubuntu']);
      expect(result.windows[0].usedPercent).toBe(25);
      expect(JSON.stringify(result)).not.toContain(identityKey(identity));
      expect(JSON.stringify(result)).not.toContain(credentialFingerprint(credential));
    } finally {
      await fs.rm(scope, { recursive: true, force: true });
    }
  });

  it('keeps an unchanged independently verified native reading and activation capability', async () => {
    const primary = reading(account(), {
      source: 'Antigravity saved login on Ubuntu',
      platform: 'ubuntu',
      capabilities: account().capabilities,
    });
    const [result] = await recoverAntigravityUsage([primary], 'fixture', {
      readBinding: binding,
      failover: { recover: async (_, row) => row, peek: (_, row) => row },
    });
    expect(result).toBe(primary);
    expect(result.capabilities.antigravityCanActivate).toBe(true);
  });

  it('invalidates pending usage when the saved revision changes', async () => {
    let current = binding();
    const [result] = await recoverAntigravityUsage([account()], 'fixture', {
      readBinding: () => current,
      failover: {
        recover: async (_, primary) => {
          current = { ...current, credentialRevision: '3'.repeat(64) };
          return reading(primary);
        },
        peek: (_, primary) => primary,
      },
    });
    expect(result.windows).toEqual([]);
    expect(result.status).toBe('unavailable');
    expect(result.sampledAt).toBeNull();
    expect(result.capabilities.antigravityCanActivate).toBe(false);
  });

  it('preserves independently proven activation for a cloned cached native projection only', async () => {
    const primary = reading(account(), {
      status: 'cached',
      source: 'Antigravity saved login on Ubuntu',
      platform: 'ubuntu',
      capabilities: account().capabilities,
    });
    const deps: AntigravityUsageFailoverDeps = {
      readBinding: binding,
      failover: {
        recover: async (_, row) => structuredClone(row),
        peek: (_, row) => structuredClone(row),
      },
    };
    const [result] = await recoverAntigravityUsage([primary], 'fixture', deps);
    expect(result).not.toBe(primary);
    expect(result.capabilities.antigravityCanActivate).toBe(true);
    expect(peekAntigravityUsage(primary, 'fixture', deps).capabilities.antigravityCanActivate).toBe(
      true
    );
    const unproven = {
      ...primary,
      capabilities: { ...primary.capabilities, antigravityCanActivate: false },
    };
    expect(
      peekAntigravityUsage(unproven, 'fixture', deps).capabilities.antigravityCanActivate
    ).toBe(false);
    const fallback = {
      ...deps,
      failover: {
        recover: async (_: UsageFailoverTarget, row: DashboardAccount) =>
          reading(row, { platform: 'ubuntu', source: 'Verified quota on Ubuntu (fallback)' }),
        peek: (_: UsageFailoverTarget, row: DashboardAccount) => row,
      },
    };
    expect(
      (await recoverAntigravityUsage([primary], 'fixture', fallback))[0].capabilities
        .antigravityCanActivate
    ).toBe(false);
  });

  it('does not publish a reading after a saved profile is removed', async () => {
    let current: AntigravityUsageBinding | null = binding();
    const [result] = await recoverAntigravityUsage([account()], 'fixture', {
      readBinding: () => current,
      failover: {
        recover: async (_, primary) => {
          current = null;
          return reading(primary);
        },
        peek: (_, primary) => primary,
      },
    });
    expect(result.windows).toEqual([]);
    expect(result.capabilities.antigravityCanActivate).toBe(false);
  });

  it('never uses failover to bypass an explicit native identity-unbound failure', async () => {
    let calls = 0;
    const primary = account({ status: 'error', statusReason: 'identity_unbound' });
    const deps: AntigravityUsageFailoverDeps = {
      readBinding: () => {
        calls++;
        return binding();
      },
      failover: {
        recover: async (_, row) => {
          calls++;
          return reading(row);
        },
        peek: (_, row) => {
          calls++;
          return reading(row);
        },
      },
    };
    expect((await recoverAntigravityUsage([primary], 'fixture', deps))[0]).toBe(primary);
    expect(peekAntigravityUsage(primary, 'fixture', deps)).toBe(primary);
    expect(calls).toBe(0);
  });

  it('rechecks the current binding on cache projection and preserves the real sample time', () => {
    let current = binding();
    const targets: UsageFailoverTarget[] = [];
    const deps: AntigravityUsageFailoverDeps = {
      readBinding: () => current,
      failover: {
        recover: async () => {
          throw new Error('peek must not query a provider');
        },
        peek: (target, primary) => {
          targets.push(target);
          return reading(primary, { status: 'cached' });
        },
      },
    };
    const first = peekAntigravityUsage(account(), 'fixture', deps);
    current = { ...current, credentialRevision: '3'.repeat(64) };
    const second = peekAntigravityUsage(account(), 'fixture', deps);
    expect(first.status).toBe('cached');
    expect(second.sampledAt).toBe(STAMP);
    expect(targets[0].binding).not.toBe(targets[1].binding);
    expect(first.capabilities.antigravityCanActivate).toBe(false);
    expect(second.capabilities.antigravityCanActivate).toBe(false);
  });

  it('rejects mismatched result attribution and unavailable saved credential metadata', async () => {
    const [wrong] = await recoverAntigravityUsage([account()], 'fixture', {
      readBinding: binding,
      failover: {
        recover: async (_, row) => reading(row, { email: 'bob@example.test' }),
        peek: (_, row) => row,
      },
    });
    expect(wrong.windows).toEqual([]);
    expect(wrong.statusReason).toBe('identity_unbound');
    const existing = reading(account());
    const invalid = peekAntigravityUsage(existing, 'fixture', { readBinding: () => null });
    expect(invalid.windows).toEqual([]);
    expect(invalid.status).toBe('unavailable');
    expect(invalid.capabilities.antigravityCanActivate).toBe(false);
  });
});
