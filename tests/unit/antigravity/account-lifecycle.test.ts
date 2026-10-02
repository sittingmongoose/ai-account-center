/**
 * Antigravity Add / Sign in again import and snapshot Remove over the real
 * private registry in a temporary CCS folder. The provider identity check and
 * the live native login are fakes; no native store or provider is touched.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  ANTIGRAVITY_NATIVE_TOKEN,
  AntigravityAccountLifecycle,
  AntigravityLifecycleError,
  antigravitySignInCommand,
} from '../../../src/antigravity/account-lifecycle';
import {
  AntigravityProfileRegistry,
  credentialFingerprint,
} from '../../../src/antigravity/registry';
import type { NativeCredential, VerifiedIdentity } from '../../../src/antigravity/types';

const NOW = Date.parse('2026-10-02T12:00:00.000Z');
let root: string;
let ccsDir: string;
let home: string;
let native: NativeCredential | null;
let nativeFails: boolean;
let validations: number;

function credential(email: string, version = 1): NativeCredential {
  return {
    format: 'antigravity-consumer-json',
    bytes: Buffer.from(
      JSON.stringify({
        auth_method: 'consumer',
        token: { access_token: `access:${email}:${version}`, refresh_token: `refresh:${email}` },
      })
    ),
  };
}

function identityOf(value: NativeCredential, verifiedAt = NOW): VerifiedIdentity {
  const token = JSON.parse(value.bytes.toString('utf8')).token.access_token as string;
  const email = token.split(':')[1];
  return {
    email,
    subject: `subject-${email}`,
    plan: null,
    verifiedAt: new Date(verifiedAt).toISOString(),
    source: 'provider-userinfo',
  };
}

function lifecycle(validate?: (value: NativeCredential) => Promise<VerifiedIdentity>) {
  return new AntigravityAccountLifecycle({
    ccsDir: () => ccsDir,
    home: () => home,
    now: () => NOW,
    validateCredential:
      validate ??
      (async (value) => {
        validations += 1;
        return identityOf(value);
      }),
    readNativeCredential: async () => {
      if (nativeFails || !native) throw new Error('native store unavailable');
      return native;
    },
  });
}

function setNative(value: NativeCredential | null): void {
  native = value;
  const file = path.join(home, ANTIGRAVITY_NATIVE_TOKEN);
  if (value) {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, 'fixture placeholder; the fake reader answers', { mode: 0o600 });
  } else fs.rmSync(file, { force: true });
}

async function save(id: string, email = `${id}@example.com`, version = 1): Promise<void> {
  const registry = new AntigravityProfileRegistry(ccsDir);
  const value = credential(email, version);
  await registry.withLock(async () =>
    registry.saveCredential(id, 'ubuntu', value, identityOf(value), NOW)
  );
}

function credentialFiles(id: string): string[] {
  const directory = path.join(ccsDir, 'antigravity-instances', id, 'ubuntu');
  return fs.existsSync(directory) ? fs.readdirSync(directory).sort() : [];
}

async function code(promise: Promise<unknown>): Promise<string> {
  try {
    await promise;
    return 'resolved';
  } catch (error) {
    return error instanceof AntigravityLifecycleError ? error.code : `other:${String(error)}`;
  }
}

beforeEach(() => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-lifecycle-'));
  ccsDir = path.join(root, '.ccs');
  home = path.join(root, 'home');
  fs.mkdirSync(ccsDir, { mode: 0o700 });
  fs.mkdirSync(home, { mode: 0o700 });
  native = null;
  nativeFails = false;
  validations = 0;
});

afterEach(() => {
  fs.rmSync(root, { recursive: true, force: true });
});

describe('Antigravity profile listing', () => {
  it('reads nothing and creates nothing while no registry exists', () => {
    const agy = lifecycle();
    expect(agy.listProfiles()).toEqual([]);
    expect(agy.profileCount()).toBe(0);
    expect(agy.activationRunning()).toBe(false);
    expect(fs.readdirSync(ccsDir)).toEqual([]);
  });

  it('builds the fixed terminal command for valid names only', () => {
    expect(antigravitySignInCommand('party')).toBe('ai-account-center antigravity signin party');
    for (const bad of ['', 'Party', '1x', 'a b', 'a;rm', `a${'b'.repeat(48)}`]) {
      expect(() => antigravitySignInCommand(bad)).toThrow();
    }
  });
});

describe('Antigravity snapshot Remove', () => {
  it('deletes only the registry snapshot and its own credential files', async () => {
    await save('gmail');
    await save('party');
    await save('party', 'party@example.com', 2);
    const foreign = path.join(ccsDir, 'antigravity-instances', 'party', 'ubuntu', 'notes.txt');
    fs.writeFileSync(foreign, 'not ours', { mode: 0o600 });
    setNative(credential('gmail@example.com'));
    const agy = lifecycle();
    expect(await agy.removeRefusal('party', { signinRunning: false, fresh: true })).toBeNull();
    const result = await agy.remove('party');
    expect(result.leftInPlace).toBe(1);
    expect(agy.listProfiles().map((profile) => profile.id)).toEqual(['gmail']);
    expect(credentialFiles('party')).toEqual(['notes.txt']);
    expect(credentialFiles('gmail').length).toBe(1);
    expect(await code(agy.remove('party'))).toBe('unknown_account');
  });

  it('refuses the live native login by its saved bytes, at both checks, with no provider call', async () => {
    await save('gmail');
    await save('party');
    setNative(credential('gmail@example.com'));
    const agy = lifecycle();
    expect(await agy.removeRefusal('gmail', { signinRunning: false, fresh: true })).toBe(
      'account_active'
    );
    expect(await code(agy.remove('gmail'))).toBe('account_active');
    expect(validations).toBe(0);
    expect(agy.listProfiles().length).toBe(2);
  });

  it('refuses the live native login by verified identity after a token refresh', async () => {
    await save('gmail');
    await save('party');
    setNative(credential('gmail@example.com', 7));
    const agy = lifecycle();
    expect(await code(agy.remove('gmail'))).toBe('account_active');
    expect(validations).toBe(1);
    expect(credentialFiles('gmail').length).toBe(1);
  });

  it('refuses when the live login cannot be checked, and allows when there is none', async () => {
    await save('gmail');
    await save('party');
    setNative(credential('gmail@example.com'));
    nativeFails = true;
    const agy = lifecycle();
    await expect(
      agy.removeRefusal('party', { signinRunning: false, fresh: true })
    ).rejects.toThrow();
    expect(await code(agy.remove('party'))).toBe('remove_failed');
    setNative(null);
    expect(await agy.removeRefusal('party', { signinRunning: false, fresh: true })).toBeNull();
    expect(await code(agy.remove('party'))).toBe('resolved');
  });

  it('refuses the runtime-verified active profile, a held lock, a pending switch and a running sign-in', async () => {
    await save('gmail');
    await save('party');
    const registry = new AntigravityProfileRegistry(ccsDir);
    await registry.withLock(async () =>
      registry.completeTransaction('party', new Date(NOW).toISOString())
    );
    const agy = lifecycle();
    expect(await agy.removeRefusal('party', { signinRunning: false, fresh: false })).toBe(
      'account_active'
    );
    expect(await code(agy.remove('party'))).toBe('account_active');
    expect(await agy.removeRefusal('gmail', { signinRunning: true, fresh: false })).toBe(
      'signin_running'
    );
    expect(
      await agy.removeRefusal('gmail', { signinRunning: false, fresh: false, liveHint: true })
    ).toBe('account_active');
    const lock = path.join(ccsDir, 'antigravity-profiles', '.transaction-lock');
    fs.mkdirSync(lock, { mode: 0o700 });
    fs.writeFileSync(
      path.join(lock, 'holder.json'),
      JSON.stringify({ pid: process.pid, startedAt: new Date().toISOString() }),
      { mode: 0o600 }
    );
    expect(await agy.removeRefusal('gmail', { signinRunning: false, fresh: false })).toBe(
      'activation_running'
    );
    expect(await code(agy.remove('gmail'))).toBe('activation_running');
    fs.rmSync(lock, { recursive: true });
    await registry.withLock(async () => registry.beginTransaction('gmail', 'party', NOW));
    expect(await agy.removeRefusal('gmail', { signinRunning: false, fresh: false })).toBe(
      'activation_running'
    );
    expect(await code(agy.remove('gmail'))).toBe('activation_running');
    expect(agy.listProfiles().length).toBe(2);
  });

  it('binds the confirmation fingerprint to the saved credential and the profile list', async () => {
    await save('gmail');
    await save('party');
    const agy = lifecycle();
    const before = agy.removeFingerprint('party');
    expect(agy.removeFingerprint('party')).toBe(before);
    await save('party', 'party@example.com', 2);
    expect(agy.removeFingerprint('party')).not.toBe(before);
    const changed = agy.removeFingerprint('party');
    await save('work');
    expect(agy.removeFingerprint('party')).not.toBe(changed);
  });
});

describe('Antigravity sign-in import', () => {
  it('adds a new profile with the verified identity and refuses a name or account already saved', async () => {
    await save('gmail');
    const agy = lifecycle();
    const result = await agy.importSignIn({
      profileId: 'party',
      mode: 'add',
      credential: credential('party@example.com'),
    });
    expect(result).toEqual({ profileId: 'party', email: 'party@example.com', plan: null });
    expect(
      agy
        .listProfiles()
        .map((profile) => profile.id)
        .sort()
    ).toEqual(['gmail', 'party']);
    expect(
      await code(
        agy.importSignIn({
          profileId: 'party',
          mode: 'add',
          credential: credential('x@example.com'),
        })
      )
    ).toBe('id_in_use');
    expect(
      await code(
        agy.importSignIn({
          profileId: 'second',
          mode: 'add',
          credential: credential('gmail@example.com', 3),
        })
      )
    ).toBe('duplicate_identity');
    expect(agy.profileCount()).toBe(2);
  });

  it('signs in again only to the same Google account and never the live login', async () => {
    await save('gmail');
    await save('party');
    setNative(credential('gmail@example.com'));
    const agy = lifecycle();
    const before = new AntigravityProfileRegistry(ccsDir).readCredential('party', 'ubuntu');
    expect(
      await code(
        agy.importSignIn({
          profileId: 'party',
          mode: 'signin-again',
          credential: credential('other@example.com'),
        })
      )
    ).toBe('identity_mismatch');
    expect(
      await code(
        agy.importSignIn({
          profileId: 'gmail',
          mode: 'signin-again',
          credential: credential('gmail@example.com', 5),
        })
      )
    ).toBe('account_active');
    expect(
      new AntigravityProfileRegistry(ccsDir).readCredential('party', 'ubuntu').credentialRevision
    ).toBe(before.credentialRevision);
    const fresh = credential('party@example.com', 9);
    await agy.importSignIn({ profileId: 'party', mode: 'signin-again', credential: fresh });
    const after = new AntigravityProfileRegistry(ccsDir).readCredential('party', 'ubuntu');
    expect(after.credentialRevision).toBe(credentialFingerprint(fresh));
    expect(
      await code(agy.importSignIn({ profileId: 'nobody', mode: 'signin-again', credential: fresh }))
    ).toBe('unknown_account');
  });

  it('refuses a stale identity proof, a failed check and the 17th profile', async () => {
    const stale = lifecycle(async (value) => identityOf(value, NOW - 10 * 60_000));
    expect(
      await code(
        stale.importSignIn({
          profileId: 'party',
          mode: 'add',
          credential: credential('p@example.com'),
        })
      )
    ).toBe('write_failed');
    const broken = lifecycle(async () => {
      throw new Error('provider unavailable');
    });
    expect(
      await code(
        broken.importSignIn({
          profileId: 'party',
          mode: 'add',
          credential: credential('p@example.com'),
        })
      )
    ).toBe('write_failed');
    for (let index = 0; index < 16; index += 1) await save(`p${index}`);
    expect(
      await code(
        lifecycle().importSignIn({
          profileId: 'extra',
          mode: 'add',
          credential: credential('extra@example.com'),
        })
      )
    ).toBe('too_many_accounts');
  });
});
