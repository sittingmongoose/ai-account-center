import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
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
    const first: CodexActivationRuntime = {
      async stop() {
        events.push('first-stop');
        await new Promise((resolve) => setTimeout(resolve, 180));
      },
      async start() {
        events.push('first-start');
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
    await new Promise((resolve) => setTimeout(resolve, 25));
    const secondActivation = activateCodexProfile('gmail', {
      codexHome,
      registry,
      runtime: second,
    });
    await Promise.all([firstActivation, secondActivation]);
    expect(events).toEqual(['first-stop', 'first-start', 'second-stop', 'second-start']);
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
    const payload = Buffer.from('{"email":"unknown@example.test"}').toString('base64url');
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
