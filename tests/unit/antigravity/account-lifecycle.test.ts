/**
 * Antigravity Add / Sign in again import and snapshot Remove over the real
 * private registry in a temporary CCS folder. The provider identity check and
 * the live native login are fakes; no native store or provider is touched.
 */
import { afterEach, beforeEach, describe, expect, it, spyOn } from 'bun:test';
import { spawn } from 'child_process';
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
import {
  antigravitySignInRunning,
  claimAntigravitySignInMarker,
} from '../../../src/antigravity/signin-marker';
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

  it('refuses the runtime-verified active profile, a held lock and a running sign-in', async () => {
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
    expect(result).toEqual({
      profileId: 'party',
      email: 'party@example.com',
      plan: null,
      liveRenewed: false,
    });
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

  it('signs in again only to the same Google account', async () => {
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

/**
 * The live native login as a real file in the injected home (0600), read back
 * from disk, and an installer that honours the guarded store's contract: it
 * replaces the file only while it still has the expected fingerprint.
 */
interface LiveStore {
  file: string;
  installs: Array<{ fingerprint: string; expected: string }>;
  /** Called instead of installing; may throw to fail the install. */
  failInstall: ((next: NativeCredential) => void) | null;
  bytes(): Buffer | null;
  agy(
    validate?: (value: NativeCredential) => Promise<VerifiedIdentity>
  ): AntigravityAccountLifecycle;
}

function liveStore(initial: NativeCredential | null): LiveStore {
  const file = path.join(home, ANTIGRAVITY_NATIVE_TOKEN);
  const write = (value: NativeCredential) => {
    fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
    const temporary = `${file}.fixture`;
    fs.writeFileSync(temporary, value.bytes, { mode: 0o600 });
    fs.renameSync(temporary, file);
  };
  if (initial) write(initial);
  const store: LiveStore = {
    file,
    installs: [],
    failInstall: null,
    bytes: () => (fs.existsSync(file) ? fs.readFileSync(file) : null),
    agy: (validate) =>
      new AntigravityAccountLifecycle({
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
          if (nativeFails) throw new Error('native store unavailable');
          return { format: 'antigravity-consumer-json', bytes: fs.readFileSync(file) };
        },
        installNativeCredential: async (next, expected) => {
          store.installs.push({ fingerprint: credentialFingerprint(next), expected });
          if (store.failInstall) {
            store.failInstall(next);
            return;
          }
          const current = credentialFingerprint({
            format: 'antigravity-consumer-json',
            bytes: fs.readFileSync(file),
          });
          if (current !== expected) throw new Error('another process changed the live login');
          write(next);
        },
      }),
  };
  return store;
}

/** A credential of `email` whose Google grant (refresh token) is `grant`. */
function grantCredential(email: string, version: number, grant: string): NativeCredential {
  return {
    format: 'antigravity-consumer-json',
    bytes: Buffer.from(
      JSON.stringify({
        auth_method: 'consumer',
        token: { access_token: `access:${email}:${version}`, refresh_token: grant },
      })
    ),
  };
}

function savedRevision(id: string): string {
  return new AntigravityProfileRegistry(ccsDir).readCredential(id, 'ubuntu').credentialRevision;
}

describe('Antigravity live sign-in again (switching is gone)', () => {
  it('the live login signs in again: the profile and the live native login both get the new credential', async () => {
    await save('gmail');
    await save('party');
    const live = liveStore(credential('gmail@example.com'));
    const agy = live.agy();
    const before = live.bytes();
    const fresh = credential('gmail@example.com', 5);
    const result = await agy.importSignIn({
      profileId: 'gmail',
      mode: 'signin-again',
      credential: fresh,
    });
    expect(result).toEqual({
      profileId: 'gmail',
      email: 'gmail@example.com',
      plan: null,
      liveRenewed: true,
    });
    expect(savedRevision('gmail')).toBe(credentialFingerprint(fresh));
    expect(live.bytes()?.equals(fresh.bytes)).toBe(true);
    expect((fs.statSync(live.file).mode & 0o777).toString(8)).toBe('600');
    // The install was bound to the live login as it was read under the lock.
    expect(live.installs).toEqual([
      {
        fingerprint: credentialFingerprint(fresh),
        expected: credentialFingerprint({ format: 'antigravity-consumer-json', bytes: before! }),
      },
    ]);
    // Matched by the saved bytes: no provider call for the live login.
    expect(validations).toBe(1);
    // The other profile is untouched.
    expect(savedRevision('party')).toBe(credentialFingerprint(credential('party@example.com')));
    // Remove still refuses the live login, at the listing and at the commit.
    expect(await agy.removeRefusal('gmail', { signinRunning: false, fresh: true })).toBe(
      'account_active'
    );
    expect(await code(agy.remove('gmail'))).toBe('account_active');
    expect(
      agy
        .listProfiles()
        .map((profile) => profile.id)
        .sort()
    ).toEqual(['gmail', 'party']);
  });

  it('a live login matched by its verified identity after a token refresh is renewed too', async () => {
    await save('gmail');
    const live = liveStore(credential('gmail@example.com', 7));
    const fresh = credential('gmail@example.com', 8);
    await live.agy().importSignIn({ profileId: 'gmail', mode: 'signin-again', credential: fresh });
    expect(savedRevision('gmail')).toBe(credentialFingerprint(fresh));
    expect(live.bytes()?.equals(fresh.bytes)).toBe(true);
    expect(validations).toBe(2);
  });

  it('a live login Google cannot verify is renewed only when it holds the saved Google grant', async () => {
    await save('gmail');
    // Expired or revoked: Google answers for the new sign-in only.
    const dead = (value: NativeCredential) => value.bytes.toString('utf8').includes(':7"');
    const validate = async (value: NativeCredential) => {
      if (dead(value)) throw new Error('needs sign-in');
      return identityOf(value);
    };
    const same = liveStore(grantCredential('gmail@example.com', 7, 'refresh:gmail@example.com'));
    const fresh = credential('gmail@example.com', 8);
    await same.agy(validate).importSignIn({
      profileId: 'gmail',
      mode: 'signin-again',
      credential: fresh,
    });
    expect(same.bytes()?.equals(fresh.bytes)).toBe(true);
    expect(savedRevision('gmail')).toBe(credentialFingerprint(fresh));

    // Another grant that Google cannot verify is not known to be this account: left alone.
    const other = liveStore(grantCredential('gmail@example.com', 7, 'refresh:someone-else'));
    const unknownBefore = other.bytes();
    const again = credential('gmail@example.com', 9);
    await other.agy(validate).importSignIn({
      profileId: 'gmail',
      mode: 'signin-again',
      credential: again,
    });
    expect(savedRevision('gmail')).toBe(credentialFingerprint(again));
    expect(other.bytes()?.equals(unknownBefore!)).toBe(true);
    expect(other.installs).toEqual([]);
  });

  it('a different identity is refused for the live login and neither store changes', async () => {
    await save('gmail');
    await save('party');
    const live = liveStore(credential('gmail@example.com'));
    const before = live.bytes();
    const saved = savedRevision('gmail');
    for (const email of ['party@example.com', 'other@example.com']) {
      expect(
        await code(
          live.agy().importSignIn({
            profileId: 'gmail',
            mode: 'signin-again',
            credential: credential(email, 4),
          })
        )
      ).toBe('identity_mismatch');
    }
    // Same email, another Google person (subject): still refused.
    const person = live.agy(async (value) => ({ ...identityOf(value), subject: 'another-person' }));
    expect(
      await code(
        person.importSignIn({
          profileId: 'gmail',
          mode: 'signin-again',
          credential: credential('gmail@example.com', 4),
        })
      )
    ).toBe('identity_mismatch');
    expect(savedRevision('gmail')).toBe(saved);
    expect(live.bytes()?.equals(before!)).toBe(true);
    expect(live.installs).toEqual([]);
    // The same Google account then commits both stores.
    const same = credential('gmail@example.com', 4);
    await live.agy().importSignIn({ profileId: 'gmail', mode: 'signin-again', credential: same });
    expect(savedRevision('gmail')).toBe(credentialFingerprint(same));
    expect(live.bytes()?.equals(same.bytes)).toBe(true);
  });

  it('a live login of another account, an unreadable one, or none is never written', async () => {
    await save('gmail');
    await save('party');
    // The registry still names gmail from an older switch; the live login is party's now.
    const registry = new AntigravityProfileRegistry(ccsDir);
    await registry.withLock(async () =>
      registry.completeTransaction('gmail', new Date(NOW).toISOString())
    );
    const live = liveStore(credential('party@example.com'));
    const before = live.bytes();
    const first = credential('gmail@example.com', 3);
    await live.agy().importSignIn({ profileId: 'gmail', mode: 'signin-again', credential: first });
    expect(savedRevision('gmail')).toBe(credentialFingerprint(first));
    expect(live.bytes()?.equals(before!)).toBe(true);

    nativeFails = true;
    const second = credential('gmail@example.com', 4);
    await live.agy().importSignIn({ profileId: 'gmail', mode: 'signin-again', credential: second });
    expect(savedRevision('gmail')).toBe(credentialFingerprint(second));
    expect(live.bytes()?.equals(before!)).toBe(true);
    nativeFails = false;

    fs.rmSync(live.file);
    const third = credential('gmail@example.com', 5);
    await live.agy().importSignIn({ profileId: 'gmail', mode: 'signin-again', credential: third });
    expect(savedRevision('gmail')).toBe(credentialFingerprint(third));
    expect(fs.existsSync(live.file)).toBe(false);
    expect(live.installs).toEqual([]);
  });

  it('a failed live install puts the previous profile credential back: write_failed, nothing changed', async () => {
    await save('gmail');
    const live = liveStore(credential('gmail@example.com'));
    const before = live.bytes();
    const saved = savedRevision('gmail');
    live.failInstall = () => {
      throw new Error('the guarded store refused');
    };
    const agy = live.agy();
    expect(
      await code(
        agy.importSignIn({
          profileId: 'gmail',
          mode: 'signin-again',
          credential: credential('gmail@example.com', 6),
        })
      )
    ).toBe('write_failed');
    expect(savedRevision('gmail')).toBe(saved);
    expect(live.bytes()?.equals(before!)).toBe(true);
    expect(live.installs.length).toBe(1);
    // The live login is still this profile's, so Remove still refuses it.
    expect(await code(agy.remove('gmail'))).toBe('account_active');

    // The live login changed between the read and the install (the CLI refreshed it while
    // Google checked it): the exchange refuses, and the profile goes back.
    const refreshed = liveStore(credential('gmail@example.com', 7));
    const raced = refreshed.agy(async (value) => {
      if (value.bytes.toString('utf8').includes(':7"')) {
        fs.writeFileSync(`${refreshed.file}.race`, credential('gmail@example.com', 11).bytes, {
          mode: 0o600,
        });
        fs.renameSync(`${refreshed.file}.race`, refreshed.file);
      }
      return identityOf(value);
    });
    expect(
      await code(
        raced.importSignIn({
          profileId: 'gmail',
          mode: 'signin-again',
          credential: credential('gmail@example.com', 6),
        })
      )
    ).toBe('write_failed');
    expect(savedRevision('gmail')).toBe(saved);
    expect(refreshed.bytes()?.equals(credential('gmail@example.com', 11).bytes)).toBe(true);
    expect(refreshed.installs.length).toBe(1);
  });

  it('an install that failed after it finished counts: both stores keep the new credential', async () => {
    await save('gmail');
    const live = liveStore(credential('gmail@example.com'));
    live.failInstall = (next) => {
      fs.writeFileSync(`${live.file}.late`, next.bytes, { mode: 0o600 });
      fs.renameSync(`${live.file}.late`, live.file);
      throw new Error('the worker timed out after the exchange');
    };
    const fresh = credential('gmail@example.com', 6);
    await live.agy().importSignIn({ profileId: 'gmail', mode: 'signin-again', credential: fresh });
    expect(savedRevision('gmail')).toBe(credentialFingerprint(fresh));
    expect(live.bytes()?.equals(fresh.bytes)).toBe(true);
  });

  it('reports live_login_not_updated only when the profile cannot be put back either', async () => {
    await save('gmail');
    const live = liveStore(credential('gmail@example.com'));
    const before = live.bytes();
    const previous = savedRevision('gmail');
    live.failInstall = () => {
      // The previous generation disappears, so the profile cannot be put back.
      fs.rmSync(
        path.join(ccsDir, 'antigravity-instances', 'gmail', 'ubuntu', `credential-${previous}.bin`)
      );
      throw new Error('the guarded store refused');
    };
    const fresh = credential('gmail@example.com', 6);
    expect(
      await code(
        live.agy().importSignIn({ profileId: 'gmail', mode: 'signin-again', credential: fresh })
      )
    ).toBe('live_login_not_updated');
    // The profile has the new credential, the live login the old one: the same Google account.
    expect(savedRevision('gmail')).toBe(credentialFingerprint(fresh));
    expect(live.bytes()?.equals(before!)).toBe(true);
  });

  it('a sign-in stopped while the checks ran writes neither store', async () => {
    await save('gmail');
    const live = liveStore(credential('gmail@example.com'));
    const before = live.bytes();
    const saved = savedRevision('gmail');
    expect(
      await code(
        live.agy().importSignIn({
          profileId: 'gmail',
          mode: 'signin-again',
          credential: credential('gmail@example.com', 6),
          stopped: () => true,
        })
      )
    ).toBe('write_failed');
    expect(savedRevision('gmail')).toBe(saved);
    expect(live.bytes()?.equals(before!)).toBe(true);
    expect(live.installs).toEqual([]);
  });
});

/** A disposable reaped child pid. */
async function deadPid(): Promise<number> {
  const child = spawn(process.execPath, ['--version'], { stdio: 'ignore' });
  const pid = child.pid;
  await new Promise<void>((resolve, reject) => {
    child.once('exit', () => resolve());
    child.once('error', reject);
  });
  if (pid === undefined) throw new Error('fixture could not spawn a disposable process');
  return pid;
}

function plantDeadLock(pid: number): string {
  const lock = path.join(ccsDir, 'antigravity-profiles', '.transaction-lock');
  fs.mkdirSync(lock, { mode: 0o700 });
  fs.writeFileSync(
    path.join(lock, 'holder.json'),
    JSON.stringify({ pid, startedAt: new Date().toISOString() }),
    { mode: 0o600 }
  );
  return lock;
}

describe('Antigravity lifecycle review fixes', () => {
  it('an abandoned lock never blocks Remove: the removal takes it over', async () => {
    await save('gmail');
    await save('party');
    const lock = plantDeadLock(await deadPid());
    const agy = lifecycle();
    expect(agy.activationRunning()).toBe(false);
    expect(await agy.removeRefusal('party', { signinRunning: false, fresh: true })).toBeNull();
    const warn = spyOn(console, 'warn').mockImplementation(() => {});
    let result: string;
    try {
      result = await code(agy.remove('party'));
    } finally {
      warn.mockRestore();
    }
    expect(result).toBe('resolved');
    expect(fs.existsSync(lock)).toBe(false);
    expect(agy.listProfiles().map((profile) => profile.id)).toEqual(['gmail']);
  });

  it('removes a profile whose saved snapshot is unreadable, and still refuses the live account', async () => {
    await save('gmail');
    await save('party');
    for (const id of ['gmail', 'party'])
      for (const name of credentialFiles(id))
        fs.rmSync(path.join(ccsDir, 'antigravity-instances', id, 'ubuntu', name));
    const agy = lifecycle();
    // The live login is gmail's account with refreshed bytes: matched by identity.
    setNative(credential('gmail@example.com', 7));
    expect(await agy.isLiveNativeProfile('gmail')).toBe(true);
    expect(await agy.removeRefusal('gmail', { signinRunning: false, fresh: true })).toBe(
      'account_active'
    );
    expect(await code(agy.remove('gmail'))).toBe('account_active');
    expect(await agy.isLiveNativeProfile('party')).toBe(false);
    expect(await code(agy.remove('party'))).toBe('resolved');
    expect(agy.listProfiles().map((profile) => profile.id)).toEqual(['gmail']);
  });

  it('refuses Remove while a terminal sign-in for the profile runs', async () => {
    await save('gmail');
    await save('party');
    const agy = lifecycle();
    const marker = claimAntigravitySignInMarker(ccsDir, 'party');
    expect(marker).not.toBeNull();
    expect(claimAntigravitySignInMarker(ccsDir, 'party')).toBeNull();
    expect(agy.signInRunning('party')).toBe(true);
    expect(agy.signInRunning('gmail')).toBe(false);
    expect(await agy.removeRefusal('party', { signinRunning: false, fresh: false })).toBe(
      'signin_running'
    );
    expect(await code(agy.remove('party'))).toBe('signin_running');
    marker!.release();
    expect(agy.signInRunning('party')).toBe(false);
    expect(await agy.removeRefusal('party', { signinRunning: false, fresh: false })).toBeNull();
    expect(fs.readdirSync(path.join(ccsDir, 'antigravity-signin'))).toEqual([]);
  });

  it('a marker left by a sign-in that died does not count and is replaced', async () => {
    const pid = await deadPid();
    const directory = path.join(ccsDir, 'antigravity-signin');
    fs.mkdirSync(directory, { mode: 0o700 });
    fs.writeFileSync(
      path.join(directory, 'party.running'),
      JSON.stringify({ pid, startedAt: new Date().toISOString() }),
      { mode: 0o600 }
    );
    expect(antigravitySignInRunning(ccsDir, 'party')).toBe(false);
    const marker = claimAntigravitySignInMarker(ccsDir, 'party');
    expect(marker).not.toBeNull();
    expect(antigravitySignInRunning(ccsDir, 'party')).toBe(true);
    expect(JSON.parse(fs.readFileSync(path.join(directory, 'party.running'), 'utf8')).pid).toBe(
      process.pid
    );
    marker!.release();
    expect(fs.readdirSync(directory)).toEqual([]);
  });

  it('sweeps credential files no saved profile names, and a new profile keeps no leftovers', async () => {
    await save('gmail');
    const ghost = path.join(ccsDir, 'antigravity-instances', 'ghost', 'ubuntu');
    fs.mkdirSync(ghost, { recursive: true, mode: 0o700 });
    fs.chmodSync(path.dirname(ghost), 0o700);
    const leftover = `credential-${'a'.repeat(64)}.bin`;
    fs.writeFileSync(path.join(ghost, leftover), 'old', { mode: 0o600 });
    // A new Add of the same name does not keep the leftover as its previous copy.
    await save('ghost');
    expect(credentialFiles('ghost')).toEqual([
      `credential-${credentialFingerprint(credential('ghost@example.com'))}.bin`,
    ]);
    const agy = lifecycle();
    expect(await code(agy.remove('ghost'))).toBe('resolved');
    fs.mkdirSync(ghost, { recursive: true, mode: 0o700 });
    fs.chmodSync(path.dirname(ghost), 0o700);
    fs.writeFileSync(path.join(ghost, leftover), 'old', { mode: 0o600 });
    expect(await agy.sweepOrphans()).toEqual({ removed: 1, leftInPlace: 0 });
    expect(fs.existsSync(path.dirname(ghost))).toBe(false);
    expect(credentialFiles('gmail').length).toBe(1);
  });

  it('reading the registry never creates the credential folder', async () => {
    fs.mkdirSync(path.join(ccsDir, 'antigravity-profiles'), { mode: 0o700 });
    const agy = lifecycle();
    expect(agy.listProfiles()).toEqual([]);
    expect(agy.activationRunning()).toBe(false);
    expect(agy.removeFingerprint('party')).toMatch(/^[a-f0-9]{64}$/);
    expect(await agy.isLiveNativeProfile('party')).toBe(false);
    expect(fs.readdirSync(ccsDir)).toEqual(['antigravity-profiles']);
  });

  it('clears a legacy stuck transaction on write without blocking, while a held lock still blocks', async () => {
    await save('gmail');
    await save('party');
    const profilesDir = path.join(ccsDir, 'antigravity-profiles');
    const latest = fs
      .readdirSync(profilesDir)
      .filter((name) => /^registry-\d+\.json$/.test(name))
      .sort()
      .at(-1)!;
    const raw = JSON.parse(fs.readFileSync(path.join(profilesDir, latest), 'utf8'));
    raw.transaction = {
      id: '0123456789abcdef0123456789abcdef',
      state: 'recovery-required',
      targetId: 'party',
      previousId: 'gmail',
      startedAt: new Date(NOW).toISOString(),
    };
    fs.writeFileSync(path.join(profilesDir, latest), JSON.stringify(raw), { mode: 0o600 });
    const registry = new AntigravityProfileRegistry(ccsDir);
    expect(registry.readState().transaction?.state).toBe('recovery-required');

    const agy = lifecycle();
    expect(agy.activationRunning()).toBe(false);
    expect(await code(agy.remove('party'))).toBe('resolved');
    expect(registry.readState().transaction).toBeNull();
    expect(agy.listProfiles().map((profile) => profile.id)).toEqual(['gmail']);

    // An active .transaction-lock held by a live process still reports busy.
    await registry.withLock(async () => {
      expect(agy.activationRunning()).toBe(true);
    });
    expect(agy.activationRunning()).toBe(false);
  });
});
