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
import { defaultAntigravityAutoSwitchState } from '../../../src/antigravity/auto-switch/settings';
import { AntigravityProfileRegistry } from '../../../src/antigravity/registry';
import {
  createAntigravityRuntime,
  type SavedQuotaRequest,
  type SavedQuotaSnapshot,
} from '../../../src/antigravity/runtime-composition';
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

    let storedState = defaultAntigravityAutoSwitchState();
    const runtime = createAntigravityRuntime({
      registry,
      driver: {
        hostId: 'ubuntu',
        canProveRuntimeIdentity: async () => false,
        readCurrentCredential: async () => {
          throw new Error('no live login in this fixture');
        },
        validateCredential: async (value) => identityOf(value, NOW),
        inspectProcesses: async () => {
          throw new Error('unused');
        },
        stopProcesses: async () => {
          throw new Error('unused');
        },
        installCredential: async () => {
          throw new Error('unused');
        },
        readStoredIdentity: async () => {
          throw new Error('unused');
        },
        restartProcesses: async () => {
          throw new Error('unused');
        },
        proveRuntimeIdentity: async () => {
          throw new Error('unused');
        },
        rollbackCredential: async () => {
          throw new Error('unused');
        },
        stopOwnedRestarts: async () => {
          throw new Error('unused');
        },
      },
      store: {
        read: () => structuredClone(storedState),
        write: (value) => {
          storedState = structuredClone(value);
        },
      },
      now: () => NOW,
      setTimer: (callback, ms) => setTimeout(callback, ms),
      clearTimer: (timer) => clearTimeout(timer as never),
      collectQuota: async (request) => quotaSnapshot(request),
      observeHost: async () => ({
        hostId: 'ubuntu',
        available: true,
        complete: true,
        busy: false,
        manualActivationInProgress: false,
        sampledAt: new Date(NOW).toISOString(),
      }),
    });
    try {
      expect((await runtime.getAccounts({ refresh: true }))[0].status).toBe('ok');

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
      const refreshed = await runtime.getAccounts({ refresh: true });
      expect(refreshed[0].status).toBe('ok');
      expect(refreshed[0].windows.length).toBe(1);
      const cached = runtime.cachedAccounts();
      expect(cached[0].status).toBe('ok');
      expect(cached[0].email).toBe('party@example.com');
    } finally {
      runtime.stop();
    }
  });
});
