import { afterEach, beforeEach, expect, it, mock, spyOn } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import * as childProcess from 'child_process';
import * as detector from '../../../../src/targets/codex-detector';
import * as activationLock from '../../../../src/codex-auth/codex-activation-lock';
import { CodexProfileRegistry } from '../../../../src/codex-auth/codex-profile-registry';
import { resolveCodexProfileDir } from '../../../../src/codex-auth/codex-profile-paths';
import { handleLoginCodex } from '../../../../src/codex-auth/commands/login-command';

const oldCcsHome = process.env.CCS_HOME;
let temporary: string;
let registry: CodexProfileRegistry;
let staged: string;
let native: string;
let exitCode: number | undefined;
const fixture = fs.readFileSync(path.join(__dirname, '../fixtures/activation/plum.auth.json'));
const other = fs.readFileSync(path.join(__dirname, '../fixtures/activation/gmail.auth.json'));

beforeEach(() => {
  temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-login-'));
  process.env.CCS_HOME = temporary;
  native = path.join(temporary, 'native');
  registry = new CodexProfileRegistry();
  exitCode = undefined;
  spyOn(activationLock, 'getActivationCodexHome').mockReturnValue(native);
  spyOn(detector, 'detectCodexCli').mockReturnValue('/fake/codex');
  spyOn(console, 'log').mockImplementation(() => {});
  spyOn(process.stderr, 'write').mockReturnValue(true);
  spyOn(process, 'exit').mockImplementation((code) => {
    exitCode = Number(code);
    throw new Error('test exit');
  });
});
afterEach(() => {
  mock.restore();
  if (oldCcsHome === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = oldCcsHome;
  fs.rmSync(temporary, { recursive: true, force: true });
});
function saved(name: string, content: Buffer) {
  registry.createProfile(name);
  fs.mkdirSync(resolveCodexProfileDir(name), { recursive: true });
  fs.writeFileSync(path.join(resolveCodexProfileDir(name), 'auth.json'), content);
}
function spawn(content: Buffer | null, code = 0, during?: () => Promise<void>) {
  spyOn(childProcess, 'spawn').mockImplementation((_file, _args, options) => {
    staged = String(options?.env?.CODEX_HOME);
    if (content) fs.writeFileSync(path.join(staged, 'auth.json'), content);
    const child = {
      on(event: string, callback: (code: number) => void) {
        if (event === 'exit')
          void Promise.resolve().then(async () => {
            await during?.();
            callback(code);
          });
        return child;
      },
    };
    return child as ReturnType<typeof childProcess.spawn>;
  });
}
async function run(name: string) {
  try {
    await handleLoginCodex({ registry, version: 'test' }, [name]);
  } catch (error) {
    if (exitCode === undefined) throw error;
  }
}
it('stages OAuth outside the profile without holding the activation lock, then publishes', async () => {
  spawn(fixture, 0, async () => {
    expect(registry.hasProfile('work')).toBe(false);
    const release = await activationLock.acquireCodexActivationLock(native);
    await release();
  });
  // The lock directory normally exists for a shared native login.
  fs.mkdirSync(native);
  await run('work');
  expect(exitCode).toBeUndefined();
  expect(staged).not.toBe(resolveCodexProfileDir('work'));
  expect(fs.existsSync(staged)).toBe(false);
  expect(fs.readFileSync(path.join(resolveCodexProfileDir('work'), 'auth.json'))).toEqual(fixture);
  expect(registry.getProfile('work').email).toBe('plum@example.test');
});
it.each(['different', 'duplicate', 'failed', 'incomplete', 'removed'] as const)(
  'preserves credentials on %s login failure',
  async (kind) => {
    saved('work', fixture);
    if (kind === 'duplicate') saved('second', fixture);
    spawn(
      kind === 'different' ? other : kind === 'incomplete' ? Buffer.from('{}') : fixture,
      kind === 'failed' ? 1 : 0,
      kind === 'removed' ? async () => registry.removeProfile('work') : undefined
    );
    await run('work');
    expect(exitCode).toBeDefined();
    expect(fs.readFileSync(path.join(resolveCodexProfileDir('work'), 'auth.json'))).toEqual(
      fixture
    );
    expect(fs.existsSync(staged)).toBe(false);
  }
);
it('refuses a duplicate on Add without publishing a profile', async () => {
  saved('other', fixture);
  spawn(fixture);
  await run('new');
  expect(exitCode).toBeDefined();
  expect(registry.hasProfile('new')).toBe(false);
  expect(fs.existsSync(resolveCodexProfileDir('new'))).toBe(false);
});
it('rejects unsupported options before creating staging', async () => {
  await expect(
    handleLoginCodex({ registry, version: 'test' }, ['work', '--json'])
  ).rejects.toThrow();
  expect(registry.hasProfile('work')).toBe(false);
});

it('reports a missing CLI without creating or changing a profile', async () => {
  spyOn(detector, 'detectCodexCli').mockReturnValue(null);
  await run('work');
  expect(exitCode).toBe(5);
  expect(registry.hasProfile('work')).toBe(false);
});
