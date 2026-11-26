/**
 * FW2-B: the dashboard picks up a terminal-completed Antigravity sign-in. A
 * fake CLI writes a new credential through the real terminal flow; the real
 * runtime then serves the new login on both the refresh and cached reads.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { AntigravityAccountLifecycle } from '../../../src/antigravity/account-lifecycle';
import { AntigravityProfileRegistry } from '../../../src/antigravity/registry';
import type {
  SavedQuotaRequest,
  SavedQuotaSnapshot,
} from '../../../src/antigravity/quota-worker-transport';
import { AntigravityUsageService } from '../../../src/antigravity/usage-service';
import { runAntigravityTerminalSignIn } from '../../../src/antigravity/terminal-signin';
import type {
  NativeCredential,
  VerifiedIdentity,
} from '../../../src/antigravity/types';

const NOW = Date.parse('2026-10-02T12:00:00.000Z');
const roots: string[] = [];
afterEach(() => {
  while (roots.length) fs.rmSync(roots.pop() as string, { recursive: true, force: true });
});

function envelope(email: string, version: number): string {
  return JSON.stringify({
    auth_method: 'consumer',
    token: { access_token: `access:${email}:${version}`, refresh_token: `refresh:${email}` },
  });
}

function identityOf(value: NativeCredential, now: number): VerifiedIdentity {
  const email = (JSON.parse(value.bytes.toString('utf8')).token.access_token as string).split(
    ':'
  )[1];
  return {
    email,
    subject: `subject-${email}`,
    plan: 'Google AI Pro',
    verifiedAt: new Date(now).toISOString(),
    source: 'provider-userinfo',
  };
}

function quotaSnapshot(request: SavedQuotaRequest): SavedQuotaSnapshot {
  return {
    profileId: request.profileId,
    identityKey: request.identityKey,
    credentialRevision: request.credentialRevision,
    email: request.email,
    plan: 'Google AI Pro',
    status: 'fresh',
    fetchedAt: new Date(NOW).toISOString(),
    sampledAt: new Date(NOW).toISOString(),
    windows: [
      {
        key: 'w',
        label: 'W',
        remainingPercent: 80,
        resetAt: new Date(NOW + 3600_000).toISOString(),
      },
    ],
    source: 'native-consumer',
    identityVerified: true,
    identityValidation: 'verified',
    pools: [],
  };
}

describe('terminal sign-in pickup', () => {
  it('serves the new login on refresh and cached reads after the command saves', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-pickup-'));
    roots.push(root);
    const ccsDir = path.join(root, '.ccs');
    const home = path.join(root, 'home');
    fs.mkdirSync(ccsDir, { mode: 0o700 });
    fs.mkdirSync(home, { mode: 0o700 });

    const registry = new AntigravityProfileRegistry(ccsDir);
    const seed = {
      format: 'antigravity-consumer-json',
      bytes: Buffer.from(envelope('party@example.com', 1)),
    };
    await registry.withLock(async () =>
      registry.saveCredential('party', 'ubuntu', seed, identityOf(seed, NOW), NOW)
    );
    const revisionBefore = registry.readCredential('party', 'ubuntu').credentialRevision;

    const readProfiles = () =>
      registry.listProfiles().map((profile) => {
        const credential = registry.readCredential(profile.id, 'ubuntu');
        return {
          id: profile.id,
          email: profile.email,
          plan: profile.plan,
          identityKey: profile.identityKey,
          credentialRevision: credential.credentialRevision,
          identityVerified: true,
          available: true,
          selected: false,
          verifiedAt: profile.verifiedAt,
        };
      });
    const usageService = new AntigravityUsageService({
      readProfiles: async () => readProfiles(),
      collectQuota: async (profileId) => {
        const profile = registry.listProfiles().find((item) => item.id === profileId)!;
        const credential = registry.readCredential(profileId, 'ubuntu');
        const snapshot = quotaSnapshot({
          profileId: profile.id,
          identityKey: profile.identityKey,
          credentialRevision: credential.credentialRevision,
          email: profile.email,
          plan: profile.plan,
          credential,
        });
        return {
          profileId: snapshot.profileId,
          identityKey: snapshot.identityKey,
          credentialRevision: snapshot.credentialRevision,
          status: 'ok',
          email: snapshot.email,
          plan: snapshot.plan,
          fetchedAt: snapshot.fetchedAt,
          sampledAt: snapshot.sampledAt,
          windows: snapshot.windows.map((window) => ({
            key: window.key,
            label: window.label,
            usedPercent: 100 - (window.remainingPercent ?? 0),
            remainingPercent: window.remainingPercent,
            resetAt: window.resetAt,
            windowMinutes: null,
            used: null,
            limit: null,
            unit: null,
            kind: 'rate_limit' as const,
          })),
        };
      },
      now: () => NOW,
    });
    try {
      expect((await usageService.getAccounts({ refresh: true }))[0].status).toBe('ok');

      // The terminal command runs with a fake CLI that signs in as v2.
      let polls = 0;
      let mask = '';
      let child: { exited: boolean; exit: (code: number | null) => void } | null = null;
      const code = await runAntigravityTerminalSignIn('party', {
        ccsDir,
        realHome: home,
        env: {},
        lifecycle: new AntigravityAccountLifecycle({
          ccsDir: () => ccsDir,
          home: () => home,
          now: () => NOW,
          validateCredential: async (value) => identityOf(value, NOW),
          readNativeCredential: async () => {
            throw new Error('no live login in this fixture');
          },
        }),
        io: {
          isInteractive: () => true,
          write: () => undefined,
          saveTerminal: () => null,
          restoreTerminal: () => undefined,
        },
        preflight: () => ({ ok: true, nativeBinary: '/fixture/agy', apparmorLeaf: null }),
        guardSignals: false,
        now: () => NOW,
        wait: async () => {
          polls += 1;
          await Promise.resolve();
          if (child && !child.exited && polls === 2) {
            fs.writeFileSync(
              path.join(mask, 'antigravity-cli', 'antigravity-oauth-token'),
              envelope('party@example.com', 2),
              { mode: 0o600 }
            );
          }
        },
        spawnSandbox: (_file, args) => {
          mask = args[args.indexOf('--bind') + 1];
          const listeners: Array<(code: number | null) => void> = [];
          const state = {
            exited: false,
            exit: (exitCode: number | null) => {
              if (state.exited) return;
              state.exited = true;
              for (const listener of listeners) listener(exitCode);
            },
          };
          child = state;
          return {
            onExit: (listener: (code: number | null) => void) => {
              listeners.push(listener);
            },
            kill: () => {
              state.exit(null);
            },
          };
        },
      });
      expect(code).toBe(0);

      // The import is visible: a new revision, and the dashboard serves it.
      const revisionAfter = registry.readCredential('party', 'ubuntu').credentialRevision;
      expect(revisionAfter).not.toBe(revisionBefore);
      const refreshed = await usageService.getAccounts({ refresh: true });
      expect(refreshed[0].status).toBe('ok');
      expect(refreshed[0].windows.length).toBe(1);
      const cached = usageService.cachedAccounts(readProfiles());
      expect(cached[0].status).toBe('ok');
      expect(cached[0].email).toBe('party@example.com');
    } finally {
      // The runtime holds no timers to stop.
    }
  });
});
