import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as lockfile from 'proper-lockfile';
import {
  activateCodexProfile,
  CodexActivationError,
} from '../../../src/codex-auth/activate-codex-profile';
import type { CodexActivationRuntime } from '../../../src/codex-auth/activate-codex-profile';
import { CodexProfileRegistry } from '../../../src/codex-auth/codex-profile-registry';
import { decodeAccountIdentity } from '../../../src/codex-auth/codex-account-identity';
import { resolveCodexProfileDir } from '../../../src/codex-auth/codex-profile-paths';
import {
  getCodexAuthProfilesSummary,
  invalidateCodexAuthProfilesCache,
} from '../../../src/codex-auth/codex-auth-dashboard-service';
import { runCodexAuth } from '../../../src/codex-auth/codex-auth-router';
import { CodexActivationRuntimeError } from '../../../src/codex-auth/codex-activation-runtime';
import type {
  CodexActivationConfirmation,
  CodexActivationStopPlan,
} from '../../../src/codex-auth/codex-activation-confirmation';

const oldCcsHome = process.env.CCS_HOME;
const oldCodexHome = process.env.CODEX_HOME;
const oldProfile = process.env.CCS_CODEX_PROFILE;
let temporary: string;
let codexHome: string;
let registry: CodexProfileRegistry;
const fixtureDir = path.join(__dirname, 'fixtures/activation');

function fixture(name: string): Buffer {
  return fs.readFileSync(path.join(fixtureDir, `${name}.auth.json`));
}

function auth(name: string): string {
  return path.join(resolveCodexProfileDir(name), 'auth.json');
}

function writeProfile(name: string, content: Buffer): void {
  fs.mkdirSync(resolveCodexProfileDir(name), { recursive: true });
  fs.writeFileSync(auth(name), content, { mode: 0o600 });
  registry.createProfile(name);
}

function runtime(events: string[]): CodexActivationRuntime {
  return {
    async stop() {
      events.push('stop');
    },
    async start() {
      events.push('start');
    },
  };
}

function deferred(): { promise: Promise<void>; resolve: () => void } {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

/** A zero-retry contender for the activation lock: refused at once while it is held. */
async function activationLockHeld(): Promise<boolean> {
  try {
    const release = await lockfile.lock(codexHome, {
      realpath: false,
      lockfilePath: path.join(codexHome, '.ccs-activation.lock'),
      stale: 120_000,
      retries: 0,
    });
    await release();
    return false;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ELOCKED') return true;
    throw error;
  }
}

/**
 * The shared registry, with one caller's hasProfile reads reported in order. Activation
 * and removal both read it once just before waiting for the lock and once under it.
 */
function observedRegistry(onHasProfile: (call: number) => void): CodexProfileRegistry {
  let calls = 0;
  return new Proxy(registry, {
    get(target, property) {
      if (property === 'hasProfile') {
        return (name: string) => {
          onHasProfile(++calls);
          return target.hasProfile(name);
        };
      }
      const value: unknown = Reflect.get(target, property, target);
      return typeof value === 'function' ? value.bind(target) : value;
    },
  });
}

beforeEach(() => {
  temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-activate-'));
  process.env.CCS_HOME = temporary;
  delete process.env.CODEX_HOME;
  delete process.env.CCS_CODEX_PROFILE;
  codexHome = path.join(temporary, 'shared-codex');
  fs.mkdirSync(codexHome, { recursive: true });
  registry = new CodexProfileRegistry();
  writeProfile('gmail', fixture('gmail'));
  writeProfile('platyr', fixture('platyr'));
  fs.writeFileSync(path.join(codexHome, 'auth.json'), fixture('gmail'), { mode: 0o600 });
  invalidateCodexAuthProfilesCache();
});

afterEach(() => {
  if (oldCcsHome === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = oldCcsHome;
  if (oldCodexHome === undefined) delete process.env.CODEX_HOME;
  else process.env.CODEX_HOME = oldCodexHome;
  if (oldProfile === undefined) delete process.env.CCS_CODEX_PROFILE;
  else process.env.CCS_CODEX_PROFILE = oldProfile;
  invalidateCodexAuthProfilesCache();
  fs.rmSync(temporary, { recursive: true, force: true });
});

describe('activateCodexProfile', () => {
  it('stops writers, saves final live tokens by identity, installs atomically and starts once', async () => {
    const events: string[] = [];
    // A stale launch default must not cause the live login to be saved to platyr.
    registry.setDefault('platyr');
    fs.writeFileSync(path.join(codexHome, 'config.toml'), 'preserved');
    fs.mkdirSync(path.join(codexHome, 'sessions'));
    fs.writeFileSync(path.join(codexHome, 'sessions', 'keep.jsonl'), 'preserved');
    const refreshed = Buffer.from(
      fixture('gmail').toString().replace('fake-access-gmail', 'fake-refreshed-gmail')
    );
    const stub: CodexActivationRuntime = {
      async stop() {
        events.push('stop');
        // Simulate a shutdown refresh that caused the old two-run switch.
        fs.writeFileSync(path.join(codexHome, 'auth.json'), refreshed);
      },
      async start() {
        expect(fs.readFileSync(auth('gmail'))).toEqual(refreshed);
        expect(decodeAccountIdentity(path.join(codexHome, 'auth.json')).email).toBe(
          'platyr@example.test'
        );
        events.push('start');
      },
    };
    const result = await activateCodexProfile('platyr', { codexHome, registry, runtime: stub });
    expect(events).toEqual(['stop', 'start']);
    expect(result).toEqual({
      name: 'platyr',
      email: 'platyr@example.test',
      plan: 'pro',
      codexHome,
      previousEmail: 'gmail@example.test',
    });
    expect(fs.statSync(path.join(codexHome, 'auth.json')).mode & 0o777).toBe(0o600);
    expect(fs.readFileSync(auth('platyr'))).toEqual(fixture('platyr'));
    expect(fs.readFileSync(path.join(codexHome, 'config.toml'), 'utf8')).toBe('preserved');
    expect(fs.readFileSync(path.join(codexHome, 'sessions', 'keep.jsonl'), 'utf8')).toBe(
      'preserved'
    );
    expect(fs.readdirSync(codexHome).some((name) => name.includes('.tmp.'))).toBe(false);
    expect(registry.getProfile('platyr').last_used).toBeTruthy();
  });

  it('rereads a same-identity target after saving the refreshed live login', async () => {
    const refreshed = Buffer.from(
      fixture('gmail').toString().replace('fake-access-gmail', 'fake-refreshed-gmail')
    );
    fs.writeFileSync(path.join(codexHome, 'auth.json'), refreshed);
    const events: string[] = [];
    await activateCodexProfile('gmail', { codexHome, registry, runtime: runtime(events) });
    expect(fs.readFileSync(auth('gmail'))).toEqual(refreshed);
    expect(fs.readFileSync(path.join(codexHome, 'auth.json'))).toEqual(refreshed);
    expect(events).toEqual(['stop', 'start']);
  });

  it('restores the original auth and restarts after a startup failure', async () => {
    const events: string[] = [];
    let starts = 0;
    const stub = runtime(events);
    stub.start = async () => {
      events.push('start');
      if (++starts === 1) throw new Error('fixture startup failed');
    };
    await expect(
      activateCodexProfile('platyr', { codexHome, registry, runtime: stub })
    ).rejects.toThrow('Could not stop or restart Codex safely.');
    expect(fs.readFileSync(path.join(codexHome, 'auth.json'))).toEqual(fixture('gmail'));
    expect(events).toEqual(['stop', 'start', 'stop', 'start']);
    expect(fs.existsSync(path.join(codexHome, '.ccs-activation.lock'))).toBe(false);
  });

  it('rolls back if a restarted writer overwrites the requested account', async () => {
    let starts = 0;
    const events: string[] = [];
    const stub = runtime(events);
    stub.start = async () => {
      events.push('start');
      if (++starts === 1) fs.writeFileSync(path.join(codexHome, 'auth.json'), fixture('gmail'));
    };
    await expect(
      activateCodexProfile('platyr', { codexHome, registry, runtime: stub })
    ).rejects.toThrow('did not keep the requested account');
    expect(fs.readFileSync(path.join(codexHome, 'auth.json'))).toEqual(fixture('gmail'));
    expect(events).toEqual(['stop', 'start', 'stop', 'start']);
  });

  it('does not install anything when runtime stop refuses active work', async () => {
    const events: string[] = [];
    const stub = runtime(events);
    stub.stop = async () => {
      events.push('stop');
      throw new CodexActivationError('busy', 'Codex work is still active.');
    };
    await expect(
      activateCodexProfile('platyr', { codexHome, registry, runtime: stub })
    ).rejects.toThrow('Codex work is still active.');
    expect(fs.readFileSync(path.join(codexHome, 'auth.json'))).toEqual(fixture('gmail'));
    expect(events).toEqual(['stop']);
  });

  it('serializes simultaneous activations with a lock spanning stop through verification', async () => {
    const events: string[] = [];
    const lockHeldByFirst: boolean[] = [];
    const firstStopping = deferred();
    const finishFirstStop = deferred();
    const secondWaiting = deferred();
    const first: CodexActivationRuntime = {
      async stop() {
        events.push('first-stop');
        lockHeldByFirst.push(await activationLockHeld());
        firstStopping.resolve();
        await finishFirstStop.promise;
      },
      async start() {
        events.push('first-start');
        // Restart and verification still run under the same lock.
        lockHeldByFirst.push(await activationLockHeld());
      },
    };
    const second: CodexActivationRuntime = {
      async stop() {
        events.push('second-stop');
      },
      async start() {
        events.push('second-start');
      },
    };
    const firstActivation = activateCodexProfile('platyr', { codexHome, registry, runtime: first });
    // Stop runs only under the lock, so the first activation holds it from here on.
    await firstStopping.promise;
    const secondActivation = activateCodexProfile('gmail', {
      codexHome,
      registry: observedRegistry((call) => {
        if (call === 1) secondWaiting.resolve();
        else events.push('second-locked');
      }),
      runtime: second,
    });
    // The second activation has read its profile and asked for the lock.
    await secondWaiting.promise;
    finishFirstStop.resolve();
    await Promise.all([firstActivation, secondActivation]);
    expect(lockHeldByFirst).toEqual([true, true]);
    expect(events).toEqual([
      'first-stop',
      'first-start',
      'second-locked',
      'second-stop',
      'second-start',
    ]);
    expect(decodeAccountIdentity(path.join(codexHome, 'auth.json')).email).toBe(
      'gmail@example.test'
    );
  });

  it('rejects a per-profile CODEX_HOME before stopping anything', async () => {
    process.env.CODEX_HOME = resolveCodexProfileDir('platyr');
    const events: string[] = [];
    await expect(
      activateCodexProfile('platyr', { codexHome, registry, runtime: runtime(events) })
    ).rejects.toThrow('Unset the per-profile CODEX_HOME');
    expect(events).toEqual([]);
  });

  it('refuses unsaved live identities and brings writers back without changing auth', async () => {
    // The payload itself needs a different decoded email.
    const header = Buffer.from('{"alg":"none"}').toString('base64url');
    const payload = Buffer.from(
      JSON.stringify({
        email: 'unknown@example.test',
        'https://api.openai.com/auth': { chatgpt_account_id: 'fixture-unknown' },
      })
    ).toString('base64url');
    const live = Buffer.from(
      JSON.stringify({ tokens: { id_token: `${header}.${payload}.fakesig` } })
    );
    fs.writeFileSync(path.join(codexHome, 'auth.json'), live);
    const events: string[] = [];
    await expect(
      activateCodexProfile('platyr', { codexHome, registry, runtime: runtime(events) })
    ).rejects.toThrow('no saved profile');
    expect(fs.readFileSync(path.join(codexHome, 'auth.json'))).toEqual(live);
    expect(events).toEqual(['stop', 'start']);
  });

  it('does not reveal token material from malformed target JSON', async () => {
    fs.writeFileSync(auth('platyr'), '{"tokens":{"id_token":"secret-fixture-fragment');
    let error: unknown;
    try {
      await activateCodexProfile('platyr', { codexHome, registry, runtime: runtime([]) });
    } catch (caught) {
      error = caught;
    }
    expect(error).toBeInstanceOf(CodexActivationError);
    expect(String(error)).not.toContain('secret-fixture-fragment');
    expect(String(error)).toContain('not valid JSON');
  });

  it('rejects an identity-only target before stopping writers', async () => {
    const incomplete = JSON.parse(fixture('platyr').toString());
    delete incomplete.tokens.access_token;
    delete incomplete.tokens.refresh_token;
    fs.writeFileSync(auth('platyr'), JSON.stringify(incomplete));
    const events: string[] = [];
    await expect(
      activateCodexProfile('platyr', { codexHome, registry, runtime: runtime(events) })
    ).rejects.toThrow('needs access and refresh tokens');
    expect(events).toEqual([]);
    expect(fs.readFileSync(path.join(codexHome, 'auth.json'))).toEqual(fixture('gmail'));
  });
});

describe('confirmed activation transactions', () => {
  const plan: CodexActivationStopPlan = {
    identities: [{ pid: 42, ppid: 1, startTime: 'fixture-start', fingerprint: 'safe-digest' }],
    roots: [42],
    processes: [{ pid: 42, label: 'Codex CLI', role: 'cli' }],
  };

  async function issue(): Promise<CodexActivationConfirmation> {
    try {
      await activateCodexProfile('platyr', {
        codexHome,
        registry,
        runtime: {
          stop: async () => {
            throw new CodexActivationRuntimeError('active CLI', 'busy', plan);
          },
          start: async () => {
            throw new Error('Must not restart before consent');
          },
        },
      });
    } catch (error) {
      expect(error).toBeInstanceOf(CodexActivationError);
      const offer = (error as CodexActivationError).details?.confirmation;
      expect(offer).toBeDefined();
      return offer!;
    }
    throw new Error('Expected an offer');
  }

  it('releases activation lock after the warning, changes no auth on cancellation, and performs a one-shot confirmed swap', async () => {
    const offer = await issue();
    expect(fs.existsSync(path.join(codexHome, '.ccs-activation.lock'))).toBe(false);
    expect(fs.readFileSync(path.join(codexHome, 'auth.json'))).toEqual(fixture('gmail'));
    const events: string[] = [];
    const stub = runtime(events);
    stub.stop = async (approval) => {
      expect(approval).toEqual(plan);
      events.push('approved-stop');
    };
    const result = await activateCodexProfile('platyr', {
      codexHome,
      registry,
      runtime: stub,
      confirmationToken: offer.token,
    });
    expect(result.email).toBe('platyr@example.test');
    expect(events).toEqual(['approved-stop', 'start']);
    await expect(
      activateCodexProfile('platyr', {
        codexHome,
        registry,
        runtime: runtime([]),
        confirmationToken: offer.token,
      })
    ).rejects.toMatchObject({ code: 'confirmation_stale' });
  });

  it('rejects an auth refresh or an unrelated target before invoking stop', async () => {
    const offer = await issue();
    const refreshed = Buffer.from(
      fixture('gmail').toString().replace('fake-access-gmail', 'fresh-access')
    );
    fs.writeFileSync(path.join(codexHome, 'auth.json'), refreshed);
    const events: string[] = [];
    await expect(
      activateCodexProfile('platyr', {
        codexHome,
        registry,
        runtime: runtime(events),
        confirmationToken: offer.token,
      })
    ).rejects.toMatchObject({ code: 'confirmation_stale' });
    expect(events).toEqual([]);
    expect(fs.readFileSync(path.join(codexHome, 'auth.json'))).toEqual(refreshed);
    const nextOffer = await issue();
    await expect(
      activateCodexProfile('gmail', {
        codexHome,
        registry,
        runtime: runtime(events),
        confirmationToken: nextOffer.token,
      })
    ).rejects.toMatchObject({ code: 'confirmation_stale' });
    expect(events).toEqual([]);
  });

  it('rejects a target-token refresh during lock wait before consuming approval to stop writers', async () => {
    const { acquireCodexActivationLock } = await import(
      '../../../src/codex-auth/codex-activation-lock'
    );
    const offer = await issue();
    const release = await acquireCodexActivationLock(codexHome);
    const events: string[] = [];
    const pending = activateCodexProfile('platyr', {
      codexHome,
      registry,
      runtime: runtime(events),
      confirmationToken: offer.token,
    });
    const refreshed = Buffer.from(
      fixture('platyr').toString().replace('fake-access-platyr', 'new-fake-access-platyr')
    );
    fs.writeFileSync(auth('platyr'), refreshed);
    await release();
    await expect(pending).rejects.toMatchObject({ code: 'confirmation_stale' });
    expect(events).toEqual([]);
    expect(fs.readFileSync(path.join(codexHome, 'auth.json'))).toEqual(fixture('gmail'));
    expect(fs.readFileSync(auth('platyr'))).toEqual(refreshed);
  });

  it('restores original auth and restarts after a confirmed startup fails, without creating a new capability', async () => {
    const offer = await issue();
    const events: string[] = [];
    let starts = 0;
    const stub = runtime(events);
    stub.start = async () => {
      events.push('start');
      if (++starts === 1) throw new CodexActivationRuntimeError('failed restart');
    };
    await expect(
      activateCodexProfile('platyr', {
        codexHome,
        registry,
        runtime: stub,
        confirmationToken: offer.token,
      })
    ).rejects.toMatchObject({ code: 'restart_failed', details: undefined });
    expect(events).toEqual(['stop', 'start', 'stop', 'start']);
    expect(fs.readFileSync(path.join(codexHome, 'auth.json'))).toEqual(fixture('gmail'));
    expect(fs.existsSync(path.join(codexHome, '.ccs-activation.lock'))).toBe(false);
  });

  it('rejects a changed saved target login before stopping any program', async () => {
    const offer = await issue();
    fs.writeFileSync(auth('platyr'), fixture('gmail'));
    const events: string[] = [];
    await expect(
      activateCodexProfile('platyr', {
        codexHome,
        registry,
        runtime: runtime(events),
        confirmationToken: offer.token,
      })
    ).rejects.toMatchObject({ code: 'confirmation_stale' });
    expect(events).toEqual([]);
    expect(fs.readFileSync(path.join(codexHome, 'auth.json'))).toEqual(fixture('gmail'));
  });
});

describe('activation CLI and live dashboard identity', () => {
  it('routes activate and emits only safe identity fields', async () => {
    const chunks: string[] = [];
    const oldWrite = process.stdout.write;
    process.stdout.write = ((chunk: string | Uint8Array) => {
      chunks.push(String(chunk));
      return true;
    }) as typeof process.stdout.write;
    try {
      const code = await runCodexAuth(['activate', 'platyr'], { codexHome, runtime: runtime([]) });
      expect(code).toBe(0);
      expect(chunks.join('')).toContain('platyr@example.test');
      expect(chunks.join('')).not.toContain('fake-access');
      expect(chunks.join('')).not.toContain('fake-refresh');
    } finally {
      process.stdout.write = oldWrite;
    }
  });

  it('rejects missing or extra activate arguments without touching auth', async () => {
    const oldWrite = process.stderr.write;
    const chunks: string[] = [];
    process.stderr.write = ((chunk: string | Uint8Array) => {
      chunks.push(String(chunk));
      return true;
    }) as typeof process.stderr.write;
    try {
      expect(await runCodexAuth(['activate'])).toBe(1);
      expect(await runCodexAuth(['activate', 'platyr', '--force'])).toBe(1);
      expect(chunks.join('')).toContain('ccs codex-auth activate <name>');
    } finally {
      process.stderr.write = oldWrite;
    }
  });

  it('reports shared live identity separately from launch default and follows external swaps', async () => {
    registry.setDefault('platyr');
    let summary = await getCodexAuthProfilesSummary(codexHome);
    expect(summary.active?.name).toBe('platyr');
    expect(summary.activated?.name).toBe('gmail');
    fs.writeFileSync(path.join(codexHome, 'auth.json'), fixture('platyr'));
    summary = await getCodexAuthProfilesSummary(codexHome);
    expect(summary.activated?.name).toBe('platyr');
    expect(summary.activated?.email).toBe('platyr@example.test');
    expect(JSON.stringify(summary)).not.toContain('fake-access');
    expect(JSON.stringify(summary)).not.toContain('fake-refresh');
  });
});

describe('activation target revalidation under the shared lifecycle lock', () => {
  it('does not stop writers if target membership disappeared while waiting for the lock', async () => {
    const { acquireCodexActivationLock } = await import(
      '../../../src/codex-auth/codex-activation-lock'
    );
    const release = await acquireCodexActivationLock(codexHome);
    const events: string[] = [];
    const pending = activateCodexProfile('platyr', {
      codexHome,
      registry,
      runtime: runtime(events),
    });
    registry.removeProfile('platyr');
    await release();
    await expect(pending).rejects.toThrow("Codex profile 'platyr' does not exist.");
    expect(events).toEqual([]);
    expect(fs.readFileSync(path.join(codexHome, 'auth.json'))).toEqual(fixture('gmail'));
    expect(fs.existsSync(path.join(codexHome, '.ccs-activation.lock'))).toBe(false);
  });

  it('does not stop writers if target auth vanished while waiting for the lock', async () => {
    const { acquireCodexActivationLock } = await import(
      '../../../src/codex-auth/codex-activation-lock'
    );
    const release = await acquireCodexActivationLock(codexHome);
    const events: string[] = [];
    const pending = activateCodexProfile('platyr', {
      codexHome,
      registry,
      runtime: runtime(events),
    });
    fs.rmSync(auth('platyr'));
    await release();
    await expect(pending).rejects.toThrow('Could not read Target profile auth.json.');
    expect(events).toEqual([]);
    expect(fs.readFileSync(path.join(codexHome, 'auth.json'))).toEqual(fixture('gmail'));
  });

  it('does not stop writers if target identity changed while waiting for the lock', async () => {
    const { acquireCodexActivationLock } = await import(
      '../../../src/codex-auth/codex-activation-lock'
    );
    const release = await acquireCodexActivationLock(codexHome);
    const events: string[] = [];
    const pending = activateCodexProfile('platyr', {
      codexHome,
      registry,
      runtime: runtime(events),
    });
    fs.writeFileSync(auth('platyr'), fixture('gmail'));
    await release();
    await expect(pending).rejects.toThrow('changed account before activation');
    expect(events).toEqual([]);
    expect(fs.readFileSync(path.join(codexHome, 'auth.json'))).toEqual(fixture('gmail'));
  });

  it('retains a newly activated profile when removal began during the activation', async () => {
    const { handleRemoveCodex } = await import('../../../src/codex-auth/commands/remove-command');
    const stopEntered = deferred();
    const allowStop = deferred();
    const removalWaiting = deferred();
    const events: string[] = [];
    const pendingActivation = activateCodexProfile('platyr', {
      codexHome,
      registry,
      runtime: {
        async stop() {
          events.push('stop');
          stopEntered.resolve();
          await allowStop.promise;
        },
        async start() {
          events.push('start');
        },
      },
    });
    await stopEntered.promise;
    const originalExit = process.exit;
    const originalError = console.error;
    let denied = false;
    process.exit = () => {
      denied = true;
      expect(fs.existsSync(path.join(codexHome, '.ccs-activation.lock'))).toBe(false);
      throw new Error('fixture exit');
    };
    console.error = () => {};
    try {
      const pendingRemoval = handleRemoveCodex(
        {
          registry: observedRegistry((call) => {
            if (call === 1) removalWaiting.resolve();
            else events.push('removal-locked');
          }),
          version: 'test',
        },
        ['platyr', '--yes', '--force'],
        { codexHome }
      ).catch((error: unknown) => {
        if (!(error instanceof Error) || error.message !== 'fixture exit') throw error;
      });
      // Removal has checked the profile and asked for the lock the activation holds.
      await removalWaiting.promise;
      expect(await activationLockHeld()).toBe(true);
      expect(fs.existsSync(auth('platyr'))).toBe(true);
      expect(registry.hasProfile('platyr')).toBe(true);
      allowStop.resolve();
      await pendingActivation;
      await pendingRemoval;
    } finally {
      allowStop.resolve();
      process.exit = originalExit;
      console.error = originalError;
    }
    expect(denied).toBe(true);
    // Removal reread the registry only after the activation released the lock.
    expect(events).toEqual(['stop', 'start', 'removal-locked']);
    expect(registry.hasProfile('platyr')).toBe(true);
    expect(fs.readFileSync(auth('platyr'))).toEqual(fixture('platyr'));
    expect(fs.readFileSync(path.join(codexHome, 'auth.json'))).toEqual(fixture('platyr'));
  });
});
