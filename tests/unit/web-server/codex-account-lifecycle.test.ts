/**
 * Codex Add, Sign in again and Remove (CONTRACT-registry-lifecycle 6.2, 6.3,
 * 6.6, 6.7) against a temporary CCS_HOME, a temporary native Codex home and a
 * fake `codex login --device-auth` that writes auth.json into its CODEX_HOME.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import * as lockfile from 'proper-lockfile';
import { CodexProfileRegistry } from '../../../src/codex-auth/codex-profile-registry';
import { invalidateCodexAuthProfilesCache } from '../../../src/codex-auth/codex-auth-dashboard-service';
import {
  CODEX_STAGING_PREFIX,
  CodexAccountLifecycle,
  CodexLifecycleError,
} from '../../../src/web-server/services/codex-account-lifecycle';
import { SignInJobRunner, type SignInJob } from '../../../src/web-server/services/signin-jobs';
import type {
  SignInCommand,
  SignInProcessHandle,
} from '../../../src/web-server/services/signin-process';

const ORIGINAL_CCS_HOME = process.env.CCS_HOME;
let root: string;
let ccsDir: string;
let codexHome: string;
let sharedConfig: string;

function token(email: string, accountId: string, plan = 'pro'): string {
  const part = (value: unknown) => Buffer.from(JSON.stringify(value)).toString('base64url');
  return [
    part({ alg: 'none' }),
    part({
      email,
      'https://api.openai.com/auth': {
        chatgpt_plan_type: plan,
        chatgpt_account_id: accountId,
      },
    }),
    'sig',
  ].join('.');
}

function login(email: string, accountId: string, nonce = 'a'): string {
  return JSON.stringify({
    tokens: {
      id_token: token(email, accountId),
      access_token: `access-${nonce}`,
      refresh_token: `refresh-${nonce}`,
    },
  });
}

function addProfile(name: string, email: string, accountId = `acct-${name}`): void {
  const registry = new CodexProfileRegistry();
  registry.createProfile(name, { created: new Date().toISOString(), last_used: null, email });
  const dir = path.join(ccsDir, 'codex-instances', name);
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  fs.writeFileSync(path.join(dir, 'auth.json'), login(email, accountId, name), { mode: 0o600 });
}

beforeEach(() => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-codex-lifecycle-'));
  process.env.CCS_HOME = path.join(root, 'home');
  ccsDir = path.join(root, 'home', '.ccs');
  codexHome = path.join(root, 'native-codex');
  sharedConfig = path.join(root, 'shared', 'config.toml');
  fs.mkdirSync(ccsDir, { recursive: true });
  fs.mkdirSync(codexHome);
  fs.mkdirSync(path.dirname(sharedConfig));
  fs.writeFileSync(sharedConfig, 'model = "fixture"\n');
  invalidateCodexAuthProfilesCache();
});

afterEach(() => {
  if (ORIGINAL_CCS_HOME === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = ORIGINAL_CCS_HOME;
  invalidateCodexAuthProfilesCache();
  fs.rmSync(root, { recursive: true, force: true });
});

function lifecycle(extra: ConstructorParameters<typeof CodexAccountLifecycle>[0] = {}) {
  return new CodexAccountLifecycle({
    codexHome,
    sharedConfigPath: sharedConfig,
    codexCli: () => '/fake/codex',
    env: { PATH: '/usr/bin', HOME: root, AAC_PRIVATE: 'never-passed' },
    ...extra,
  });
}

/** A runner whose fake CLI prints the device prompt, writes `auth` into CODEX_HOME and exits 0. */
function fakeRunner(auth: string | null, commands: SignInCommand[] = []) {
  return new SignInJobRunner({
    spawn: (command) => {
      commands.push(command);
      let onData: (chunk: string) => void = () => undefined;
      let onExit: (code: number | null, failure: null) => void = () => undefined;
      const handle: SignInProcessHandle = {
        onData: (listener) => {
          onData = listener;
        },
        onExit: (listener) => {
          onExit = listener;
        },
        write: () => false,
        kill: () => undefined,
      };
      setTimeout(() => {
        onData('https://auth.openai.com/codex/device\n  WXYZ-12345\n');
        if (auth !== null) {
          fs.writeFileSync(path.join(command.env.CODEX_HOME, 'auth.json'), auth, { mode: 0o600 });
        }
        onExit(auth === null ? 1 : 0, null);
      }, 5);
      return handle;
    },
  });
}

async function finished(runner: SignInJobRunner, id: string): Promise<SignInJob> {
  for (let attempt = 0; attempt < 400; attempt += 1) {
    const job = runner.get(id) as SignInJob;
    if (['succeeded', 'failed', 'expired', 'cancelled'].includes(job.state)) {
      await new Promise((resolve) => setTimeout(resolve, 20));
      return job;
    }
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error('job did not finish');
}

function instances(): string[] {
  return fs.readdirSync(path.join(ccsDir, 'codex-instances')).sort();
}

describe('Codex Add', () => {
  it('signs in into staging, then renames into place and registers, never touching ~/.codex', async () => {
    addProfile('gmail', 'gmail@example.com');
    const commands: SignInCommand[] = [];
    const runner = fakeRunner(login('new@example.com', 'acct-new', 'new'), commands);
    const job = runner.start(lifecycle().addFlow('codex-4'));
    const done = await finished(runner, job.id);
    expect(done).toMatchObject({
      state: 'succeeded',
      accountId: 'codex:codex-4',
      result: { accountId: 'codex:codex-4', email: 'new@example.com', plan: 'pro' },
    });
    expect(commands).toHaveLength(1);
    expect(commands[0]).toMatchObject({
      file: '/fake/codex',
      args: ['login', '--device-auth'],
      pty: true,
    });
    expect(commands[0].env).toEqual({
      PATH: '/usr/bin',
      HOME: root,
      TERM: 'dumb',
      NO_COLOR: '1',
      CODEX_HOME: path.join(ccsDir, 'codex-instances', `${CODEX_STAGING_PREFIX}${job.id}`),
    });
    expect(instances()).toEqual(['codex-4', 'gmail']);
    const profileDir = path.join(ccsDir, 'codex-instances', 'codex-4');
    expect(fs.statSync(profileDir).mode & 0o777).toBe(0o700);
    expect(fs.readlinkSync(path.join(profileDir, 'config.toml'))).toBe(sharedConfig);
    expect(JSON.parse(fs.readFileSync(path.join(profileDir, 'auth.json'), 'utf8'))).toMatchObject({
      tokens: { access_token: 'access-new' },
    });
    expect(new CodexProfileRegistry().getProfile('codex-4')).toMatchObject({
      email: 'new@example.com',
      plan_type: 'pro',
      account_id: 'acct-new',
    });
    expect(new CodexProfileRegistry().getDefault()).toBeNull();
    expect(fs.readdirSync(codexHome)).toEqual([]);
  });

  it('refuses an email already saved in another profile and leaves nothing behind', async () => {
    addProfile('gmail', 'gmail@example.com');
    const runner = fakeRunner(login('GMAIL@example.com', 'acct-other'));
    const done = await finished(runner, runner.start(lifecycle().addFlow('codex-4')).id);
    expect(done).toMatchObject({ state: 'failed', error: { code: 'duplicate_identity' } });
    expect(instances()).toEqual(['gmail']);
    expect(new CodexProfileRegistry().listProfiles()).toEqual(['gmail']);
  });

  it('fails a denied sign-in or a login without tokens, and needs the codex binary', async () => {
    const denied = fakeRunner(null);
    expect(await finished(denied, denied.start(lifecycle().addFlow('codex-4')).id)).toMatchObject({
      state: 'failed',
      error: { code: 'provider_denied' },
    });
    const partial = fakeRunner(
      JSON.stringify({ tokens: { id_token: token('x@example.com', 'a') } })
    );
    expect(await finished(partial, partial.start(lifecycle().addFlow('codex-4')).id)).toMatchObject(
      { state: 'failed', error: { code: 'write_failed' } }
    );
    const missing = fakeRunner(login('x@example.com', 'a'));
    expect(
      await finished(
        missing,
        missing.start(lifecycle({ codexCli: () => null }).addFlow('codex-4')).id
      )
    ).toMatchObject({ state: 'failed', error: { code: 'tool_missing' } });
    expect(fs.readdirSync(path.join(ccsDir, 'codex-instances'))).toEqual([]);
  });
});

describe('Codex Sign in again', () => {
  it('replaces only the saved login of the same account', async () => {
    addProfile('gmail', 'gmail@example.com');
    addProfile('party', 'party@example.com', 'acct-party');
    const authPath = path.join(ccsDir, 'codex-instances', 'party', 'auth.json');
    const runner = fakeRunner(login('party@example.com', 'acct-party', 'renewed'));
    const done = await finished(runner, runner.start(lifecycle().signInAgainFlow('party')).id);
    expect(done).toMatchObject({
      state: 'succeeded',
      accountId: 'codex:party',
      mode: 'signin-again',
    });
    expect(JSON.parse(fs.readFileSync(authPath, 'utf8')).tokens.access_token).toBe(
      'access-renewed'
    );
    expect(fs.statSync(authPath).mode & 0o777).toBe(0o600);
    expect(instances()).toEqual(['gmail', 'party']);
    expect(fs.readdirSync(codexHome)).toEqual([]);
  });

  it('fails identity_mismatch for another email or ChatGPT account and changes nothing', async () => {
    addProfile('party', 'party@example.com', 'acct-party');
    const authPath = path.join(ccsDir, 'codex-instances', 'party', 'auth.json');
    const before = fs.readFileSync(authPath);
    for (const auth of [
      login('other@example.com', 'acct-party'),
      login('party@example.com', 'acct-other'),
    ]) {
      const runner = fakeRunner(auth);
      const done = await finished(runner, runner.start(lifecycle().signInAgainFlow('party')).id);
      expect(done).toMatchObject({ state: 'failed', error: { code: 'identity_mismatch' } });
      expect(fs.readFileSync(authPath)).toEqual(before);
    }
  });

  it('will not install a new login into the live active profile', async () => {
    addProfile('party', 'party@example.com', 'acct-party');
    addProfile('gmail', 'gmail@example.com');
    fs.copyFileSync(
      path.join(ccsDir, 'codex-instances', 'party', 'auth.json'),
      path.join(codexHome, 'auth.json')
    );
    const authPath = path.join(ccsDir, 'codex-instances', 'party', 'auth.json');
    const before = fs.readFileSync(authPath);
    const runner = fakeRunner(login('party@example.com', 'acct-party', 'renewed'));
    const done = await finished(runner, runner.start(lifecycle().signInAgainFlow('party')).id);
    expect(done).toMatchObject({ state: 'failed', error: { code: 'write_failed' } });
    expect(fs.readFileSync(authPath)).toEqual(before);
  });
});

describe('Codex Remove', () => {
  it('names the refusal a remove would hit', async () => {
    addProfile('gmail', 'gmail@example.com');
    const solo = lifecycle();
    expect(await solo.removeRefusal('gmail', false)).toBe('last_account');
    addProfile('party', 'party@example.com');
    addProfile('spare', 'spare@example.com');
    new CodexProfileRegistry().setDefault('gmail');
    fs.copyFileSync(
      path.join(ccsDir, 'codex-instances', 'party', 'auth.json'),
      path.join(codexHome, 'auth.json')
    );
    invalidateCodexAuthProfilesCache();
    const live = lifecycle();
    expect(await live.removeRefusal('party', false)).toBe('account_active');
    expect(await live.removeRefusal('gmail', false)).toBe('account_default');
    expect(await live.removeRefusal('spare', true)).toBe('signin_running');
    expect(await live.removeRefusal('spare', false)).toBeNull();
    const release = await lockfile.lock(codexHome, {
      realpath: false,
      lockfilePath: path.join(codexHome, '.ccs-activation.lock'),
    });
    try {
      expect(await live.removeRefusal('spare', false)).toBe('activation_running');
    } finally {
      await release();
    }
  });

  it('runs the shared staged delete and refuses the live account at the last moment', async () => {
    addProfile('gmail', 'gmail@example.com');
    addProfile('party', 'party@example.com');
    addProfile('spare', 'spare@example.com');
    new CodexProfileRegistry().setDefault('gmail');
    const subject = lifecycle();
    const before = await subject.removeFingerprint('spare');
    await subject.remove('spare');
    expect(instances()).toEqual(['gmail', 'party']);
    expect(new CodexProfileRegistry().listProfiles().sort()).toEqual(['gmail', 'party']);
    expect(new CodexProfileRegistry().getDefault()).toBe('gmail');
    expect(await subject.removeFingerprint('spare')).not.toBe(before);
    fs.copyFileSync(
      path.join(ccsDir, 'codex-instances', 'party', 'auth.json'),
      path.join(codexHome, 'auth.json')
    );
    await expect(subject.remove('party')).rejects.toMatchObject({ code: 'account_active' });
    await expect(subject.remove('gmail')).rejects.toMatchObject({ code: 'account_default' });
    expect(instances()).toEqual(['gmail', 'party']);
    await expect(subject.remove('nobody')).rejects.toBeInstanceOf(CodexLifecycleError);
  });

  it('sweeps only staging folders older than an hour', async () => {
    const dir = path.join(ccsDir, 'codex-instances');
    fs.mkdirSync(dir, { recursive: true });
    const old = path.join(dir, '.staging-job_0000000000000001');
    const young = path.join(dir, '.staging-job_0000000000000002');
    const other = path.join(dir, '.staging-something');
    for (const folder of [old, young, other]) fs.mkdirSync(folder);
    const hourAgo = (Date.now() - 2 * 60 * 60_000) / 1000;
    fs.utimesSync(old, hourAgo, hourAgo);
    fs.utimesSync(other, hourAgo, hourAgo);
    expect(await lifecycle().sweepStaging()).toBe(1);
    expect(fs.readdirSync(dir).sort()).toEqual([
      '.staging-job_0000000000000002',
      '.staging-something',
    ]);
  });
});
