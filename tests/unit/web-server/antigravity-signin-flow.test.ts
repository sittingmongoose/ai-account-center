/**
 * The supervised Antigravity sign-in job spec (antigravityJobFlow): the argv it
 * hands the runner, the marker and staging it claims, the import + descriptor
 * refresh on completion, and the guarantee that a cancelled job never commits.
 * Hermetic: preflight and descriptor refresh are injected, and the registry,
 * staging and token live in a temporary CCS dir. No bubblewrap, no network.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { antigravityJobFlow } from '../../../src/web-server/services/account-lifecycle-antigravity';
import {
  ANTIGRAVITY_FLOW_CACHE_MS,
  antigravitySignInFlow,
  resetAntigravitySignInFlowCache,
} from '../../../src/web-server/services/account-lifecycle-runtime';
import { AntigravityAccountLifecycle } from '../../../src/antigravity/account-lifecycle';
import type {
  NativeCredential,
  VerifiedIdentity,
} from '../../../src/antigravity/types';
import { SignInJobStopped } from '../../../src/web-server/services/signin-jobs';
import type { SignInPreflight } from '../../../src/antigravity/signin-sandbox';
import { claimAntigravitySignInMarker } from '../../../src/antigravity/signin-marker';

let root: string;
let ccsDir: string;
let home: string;

beforeEach(() => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-flow-'));
  ccsDir = path.join(root, '.ccs');
  home = path.join(root, 'home');
  fs.mkdirSync(ccsDir, { recursive: true, mode: 0o700 });
  fs.chmodSync(ccsDir, 0o700);
  fs.mkdirSync(home, { recursive: true });
});

afterEach(() => {
  fs.rmSync(root, { recursive: true, force: true });
});

const OK_PREFLIGHT: SignInPreflight = {
  ok: true,
  nativeBinary: '/home/u/.local/bin/agy',
  apparmorLeaf: null,
  runtimeDir: null,
};

function identityOf(value: NativeCredential): VerifiedIdentity {
  const email = JSON.parse(value.bytes.toString('utf8')).token.access_token.split(':')[1];
  return {
    email: `${email}@example.com`,
    subject: `subject-${email}`,
    plan: 'Google AI Pro',
    verifiedAt: new Date().toISOString(),
    source: 'provider-userinfo',
  };
}

function lifecycle(): AntigravityAccountLifecycle {
  return new AntigravityAccountLifecycle({
    ccsDir: () => ccsDir,
    home: () => home,
    validateCredential: async (value) => identityOf(value),
    readNativeCredential: async () => {
      throw new Error('no native login');
    },
  });
}

/** Write a complete consumer credential where the driver would leave it. */
function writeToken(tokenPath: string, email = 'party'): void {
  const fd = fs.openSync(
    tokenPath,
    fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_TRUNC,
    0o600
  );
  fs.writeFileSync(
    fd,
    JSON.stringify({
      auth_method: 'consumer',
      token: { access_token: `access:${email}:1`, refresh_token: `refresh:${email}` },
    })
  );
  fs.closeSync(fd);
}

describe('antigravityJobFlow', () => {
  it('hands the runner the fixed driver argv and claims the marker and staging', async () => {
    const flow = antigravityJobFlow(lifecycle(), ccsDir, home, 'party', 'add', {
      preflight: () => OK_PREFLIGHT,
      refreshDescriptor: async () => ({ status: 'no-installation' }),
    });
    const command = await flow.prepare('job_0000000000000001');
    expect(command.file).toBe('/usr/bin/python3');
    expect(command.pty).toBe(false);
    expect(command.args[0]).toBe('-c');
    // args[1] is the driver program; then token, origin, '--', bwrap, sandbox argv.
    expect(command.args[2]).toContain('antigravity-oauth-token');
    expect(command.args[3]).toBe('https://accounts.google.com');
    expect(command.args[4]).toBe('--');
    expect(command.args[5]).toBe('/usr/bin/bwrap');
    expect(command.args).toContain('--unshare-user');
    expect(command.args[command.args.length - 1]).toBe('/home/u/.local/bin/agy');
    expect(command.env.HOME).toBe(home);
    // The per-profile marker is held while the job runs.
    expect(fs.existsSync(path.join(ccsDir, 'antigravity-signin', 'party.running'))).toBe(true);
    await flow.cleanup('job_0000000000000001');
    expect(fs.existsSync(path.join(ccsDir, 'antigravity-signin', 'party.running'))).toBe(false);
  });

  it('fails prepare when the preflight cannot run', async () => {
    const flow = antigravityJobFlow(lifecycle(), ccsDir, home, 'party', 'add', {
      preflight: () => ({ ok: false, reason: 'tool_missing', detail: 'no CLI' }),
    });
    await expect(flow.prepare('job_0000000000000002')).rejects.toMatchObject({
      code: 'tool_missing',
    });
  });

  it('refuses to start while a terminal sign-in holds the profile marker', async () => {
    const held = claimAntigravitySignInMarker(ccsDir, 'party');
    expect(held).not.toBe(null);
    const flow = antigravityJobFlow(lifecycle(), ccsDir, home, 'party', 'add', {
      preflight: () => OK_PREFLIGHT,
    });
    await expect(flow.prepare('job_0000000000000006')).rejects.toMatchObject({
      code: 'write_failed',
    });
    // The marker is claimed before any staging, so a refused start leaves none behind.
    const signinDir = path.join(ccsDir, 'antigravity-signin');
    expect(fs.readdirSync(signinDir).filter((name) => name.startsWith('.staging-'))).toEqual([]);
    held?.release();
  });

  it('imports the credential, refreshes the descriptor and returns the account', async () => {
    const agy = lifecycle();
    const refreshed: Array<{ ccsDir: string; home: string }> = [];
    const flow = antigravityJobFlow(agy, ccsDir, home, 'party', 'add', {
      preflight: () => OK_PREFLIGHT,
      refreshDescriptor: async (request) => {
        refreshed.push(request);
        return { status: 'no-installation' };
      },
    });
    const command = await flow.prepare('job_0000000000000003');
    writeToken(command.args[2]);
    const result = await flow.complete('job_0000000000000003', { stopped: () => false });
    expect(result).toEqual({
      accountId: 'antigravity:profile:party',
      email: 'party@example.com',
      plan: 'Google AI Pro',
    });
    expect(agy.hasProfile('party')).toBe(true);
    expect(refreshed).toEqual([{ ccsDir, home }]);
  });

  it('never commits when the job was cancelled during the provider check', async () => {
    const agy = lifecycle();
    const flow = antigravityJobFlow(agy, ccsDir, home, 'party', 'add', {
      preflight: () => OK_PREFLIGHT,
      refreshDescriptor: async () => ({ status: 'no-installation' }),
    });
    const command = await flow.prepare('job_0000000000000004');
    writeToken(command.args[2]);
    await expect(
      flow.complete('job_0000000000000004', { stopped: () => true })
    ).rejects.toBeInstanceOf(SignInJobStopped);
    expect(agy.hasProfile('party')).toBe(false);
  });

  it('maps a duplicate identity to the duplicate_identity job error', async () => {
    const agy = lifecycle();
    // Seed a profile with the same Google identity the new sign-in will verify to.
    await agy.importSignIn({
      profileId: 'first',
      mode: 'add',
      credential: {
        format: 'antigravity-consumer-json',
        bytes: Buffer.from(
          JSON.stringify({
            auth_method: 'consumer',
            token: { access_token: 'access:party:1', refresh_token: 'refresh:party' },
          })
        ),
      },
    });
    const flow = antigravityJobFlow(agy, ccsDir, home, 'second', 'add', {
      preflight: () => OK_PREFLIGHT,
      refreshDescriptor: async () => ({ status: 'no-installation' }),
    });
    const command = await flow.prepare('job_0000000000000005');
    writeToken(command.args[2], 'party');
    await expect(flow.complete('job_0000000000000005', { stopped: () => false })).rejects.toMatchObject({
      code: 'duplicate_identity',
    });
  });
});

describe('antigravitySignInFlow gate', () => {
  const OK: SignInPreflight = {
    ok: true,
    nativeBinary: '/x/agy',
    apparmorLeaf: null,
    runtimeDir: null,
  };

  it('answers null when the preflight passes and maps a failure to its reason', () => {
    resetAntigravitySignInFlowCache();
    expect(antigravitySignInFlow('/h', 1, () => OK)).toBe(null);
    resetAntigravitySignInFlowCache();
    expect(
      antigravitySignInFlow('/h', 1, () => ({ ok: false, reason: 'tool_missing', detail: 'x' }))
    ).toBe('tool_missing');
    resetAntigravitySignInFlowCache();
    expect(
      antigravitySignInFlow('/h', 1, () => ({ ok: false, reason: 'preflight_failed', detail: 'x' }))
    ).toBe('preflight_failed');
  });

  it('caches the preflight per home for the TTL, so it is not probed on every read', () => {
    resetAntigravitySignInFlowCache();
    let calls = 0;
    const counting = (): SignInPreflight => {
      calls += 1;
      return OK;
    };
    expect(antigravitySignInFlow('/home/a', 1_000, counting)).toBe(null);
    expect(calls).toBe(1);
    // Same home within the TTL: served from the cache, no new probe.
    expect(antigravitySignInFlow('/home/a', 1_000 + ANTIGRAVITY_FLOW_CACHE_MS - 1, counting)).toBe(
      null
    );
    expect(calls).toBe(1);
    // A different home is not the cached one: it probes again.
    expect(antigravitySignInFlow('/home/b', 1_000, counting)).toBe(null);
    expect(calls).toBe(2);
    // Past the TTL the cached home probes again.
    expect(antigravitySignInFlow('/home/b', 1_000 + ANTIGRAVITY_FLOW_CACHE_MS, counting)).toBe(null);
    expect(calls).toBe(3);
    // An explicit reset drops the cache.
    resetAntigravitySignInFlowCache();
    expect(antigravitySignInFlow('/home/b', 1_000, counting)).toBe(null);
    expect(calls).toBe(4);
  });
});
