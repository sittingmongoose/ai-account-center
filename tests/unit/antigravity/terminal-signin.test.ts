/**
 * `ai-account-center antigravity signin <profile>` with a fake sandboxed CLI:
 * the fake writes the new credential into the mask it was given, as the
 * official CLI does after the user pastes the code. The provider identity
 * check and the live native login are fakes; temporary folders only.
 */
import { afterEach, beforeEach, describe, expect, it, spyOn } from 'bun:test';
import { spawn } from 'child_process';
import { EventEmitter } from 'events';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { AntigravityAccountLifecycle } from '../../../src/antigravity/account-lifecycle';
import { AntigravityProfileRegistry } from '../../../src/antigravity/registry';
import {
  readMaskedCredential,
  runAntigravityTerminalSignIn,
  type SandboxChild,
  type TerminalSignInDeps,
} from '../../../src/antigravity/terminal-signin';
import type { NativeCredential, VerifiedIdentity } from '../../../src/antigravity/types';
import { prepareSignInStaging } from '../../../src/antigravity/signin-sandbox';
import { claimAntigravitySignInMarker } from '../../../src/antigravity/signin-marker';

const NOW = Date.parse('2026-10-02T12:00:00.000Z');
let root: string;
let ccsDir: string;
let home: string;

beforeEach(() => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-terminal-'));
  ccsDir = path.join(root, '.ccs');
  home = path.join(root, 'home');
  fs.mkdirSync(ccsDir, { mode: 0o700 });
  fs.mkdirSync(home, { mode: 0o700 });
});

afterEach(() => {
  fs.rmSync(root, { recursive: true, force: true });
});

function envelope(email: string, version = 1, extra: Record<string, unknown> = {}): string {
  return JSON.stringify({
    auth_method: 'consumer',
    token: { access_token: `access:${email}:${version}`, refresh_token: `refresh:${email}` },
    ...extra,
  });
}

function identityOf(value: NativeCredential): VerifiedIdentity {
  const email = (JSON.parse(value.bytes.toString('utf8')).token.access_token as string).split(
    ':'
  )[1];
  return {
    email,
    subject: `subject-${email}`,
    plan: 'Google AI Pro',
    verifiedAt: new Date(NOW).toISOString(),
    source: 'provider-userinfo',
  };
}

async function save(id: string, email = `${id}@example.com`): Promise<void> {
  const registry = new AntigravityProfileRegistry(ccsDir);
  const value = { format: 'antigravity-consumer-json', bytes: Buffer.from(envelope(email)) };
  await registry.withLock(async () =>
    registry.saveCredential(id, 'ubuntu', value, identityOf(value), NOW)
  );
}

interface Run {
  code: number;
  output: string;
  spawned: Array<{ file: string; args: string[]; env: Record<string, string> }>;
  signals: string[];
  terminal: string[];
  masks: string[];
}

interface FakeCli {
  /** What the fake CLI writes into the mask, after how many polls; null writes nothing. */
  write: { content: string; mode?: number; afterPolls?: number } | null;
  /** The fake exits by itself after this many polls (the user quit), else only on a signal. */
  exitAfterPolls?: number;
  /** Ignores SIGTERM (only SIGKILL ends it). */
  stubborn?: boolean;
}

async function run(
  profile: string,
  cli: FakeCli,
  overrides: Partial<TerminalSignInDeps> = {}
): Promise<Run> {
  const result: Run = { code: -1, output: '', spawned: [], signals: [], terminal: [], masks: [] };
  let polls = 0;
  let clock = NOW;
  let child: { exit: (code: number | null) => void; exited: boolean } | null = null;
  const deps: TerminalSignInDeps = {
    ccsDir,
    realHome: home,
    env: { TERM: 'xterm-256color', SSH_CONNECTION: '1 2 3 4', GEMINI_API_KEY: 'never-passed' },
    lifecycle: new AntigravityAccountLifecycle({
      ccsDir: () => ccsDir,
      home: () => home,
      now: () => NOW,
      validateCredential: async (value) => identityOf(value),
      readNativeCredential: async () => {
        throw new Error('no live login in this fixture');
      },
    }),
    io: {
      isInteractive: () => true,
      write: (text) => {
        result.output += text;
      },
      saveTerminal: () => {
        result.terminal.push('save');
        return '500:5:bf:8a3b';
      },
      restoreTerminal: (saved) => {
        result.terminal.push(`restore:${saved}`);
      },
    },
    preflight: () => ({ ok: true, nativeBinary: '/fixture/agy', apparmorLeaf: null }),
    guardSignals: false,
    now: () => clock,
    wait: async (ms) => {
      clock += ms;
      polls += 1;
      await Promise.resolve();
      if (!child || child.exited) return;
      const mask = result.masks[0];
      if (cli.write && polls === (cli.write.afterPolls ?? 2)) {
        const token = path.join(mask, 'antigravity-cli', 'antigravity-oauth-token');
        fs.writeFileSync(token, cli.write.content, { mode: cli.write.mode ?? 0o600 });
        fs.chmodSync(token, cli.write.mode ?? 0o600);
      }
      if (cli.exitAfterPolls !== undefined && polls >= cli.exitAfterPolls) child.exit(0);
    },
    spawnSandbox: (file, args, env): SandboxChild => {
      result.spawned.push({ file, args, env });
      result.masks.push(args[args.indexOf('--bind') + 1]);
      const listeners: Array<(code: number | null) => void> = [];
      const state = {
        exited: false,
        exit: (code: number | null) => {
          if (state.exited) return;
          state.exited = true;
          for (const listener of listeners) listener(code);
        },
      };
      child = state;
      return {
        onExit: (listener) => {
          listeners.push(listener);
        },
        kill: (signal) => {
          result.signals.push(signal);
          if (signal === 'SIGKILL' || !cli.stubborn) state.exit(null);
        },
      };
    },
    ...overrides,
  };
  result.code = await runAntigravityTerminalSignIn(profile, deps);
  return result;
}

const stagingLeft = () => {
  const directory = path.join(ccsDir, 'antigravity-signin');
  return fs.existsSync(directory) ? fs.readdirSync(directory) : [];
};

describe('terminal sign-in', () => {
  it('adds a profile: the CLI is stopped once the credential is complete, then it is verified and saved', async () => {
    const result = await run('party', { write: { content: envelope('party@example.com') } });
    expect(result.code).toBe(0);
    expect(result.output).toContain('Adding Antigravity profile "party".');
    expect(result.output).toContain(
      'Saved Antigravity profile "party" (party@example.com). It now appears in the dashboard.'
    );
    expect(result.signals).toEqual(['SIGTERM']);
    expect(result.spawned.length).toBe(1);
    const { file, args, env } = result.spawned[0];
    expect(file).toBe('/usr/bin/bwrap');
    expect(args.slice(0, 8)).toEqual([
      '--die-with-parent',
      '--unshare-user',
      '--unshare-pid',
      '--unshare-ipc',
      '--unshare-uts',
      '--ro-bind',
      '/',
      '/',
    ]);
    expect(args[args.indexOf('--bind') + 2]).toBe(path.join(home, '.gemini'));
    expect(args[args.length - 1]).toBe('/fixture/agy');
    expect(env.HOME).toBe(home);
    expect(env.GEMINI_API_KEY).toBeUndefined();
    expect(env.SSH_CONNECTION).toBe('1 2 3 4');
    expect(result.terminal).toEqual(['save', 'restore:500:5:bf:8a3b']);
    expect(result.output).toContain('\x1b[?1049l');
    const registry = new AntigravityProfileRegistry(ccsDir);
    expect(registry.listProfiles().map((profile) => [profile.id, profile.email])).toEqual([
      ['party', 'party@example.com'],
    ]);
    expect(stagingLeft()).toEqual([]);
    expect(result.output).not.toContain('access:');
    expect(result.output).not.toContain('refresh:');
  });

  it('signs in again to the same account and refuses another account', async () => {
    await save('party');
    const before = new AntigravityProfileRegistry(ccsDir).readCredential('party', 'ubuntu');
    const wrong = await run('party', { write: { content: envelope('other@example.com') } });
    expect(wrong.code).toBe(1);
    expect(wrong.output).toContain('different Google account');
    expect(
      new AntigravityProfileRegistry(ccsDir).readCredential('party', 'ubuntu').credentialRevision
    ).toBe(before.credentialRevision);
    const right = await run('party', { write: { content: envelope('party@example.com', 4) } });
    expect(right.code).toBe(0);
    expect(right.output).toContain('Signed in again: Antigravity profile "party"');
    expect(
      new AntigravityProfileRegistry(ccsDir).readCredential('party', 'ubuntu').credentialRevision
    ).not.toBe(before.credentialRevision);
    expect(stagingLeft()).toEqual([]);
  });

  it('saves nothing when the CLI exits without a complete credential', async () => {
    const quit = await run('party', { write: null, exitAfterPolls: 3 });
    expect(quit.code).toBe(1);
    expect(quit.output).toContain('The sign-in did not finish. Nothing was saved.');
    expect(quit.signals).toEqual([]);
    const partial = await run('party', {
      write: { content: '{"auth_method":"consumer","token":{"access_token":"a"}}' },
      exitAfterPolls: 5,
    });
    expect(partial.code).toBe(1);
    expect(fs.existsSync(path.join(ccsDir, 'antigravity-profiles'))).toBe(false);
    expect(stagingLeft()).toEqual([]);
  });

  it('stops the CLI after 15 minutes, with SIGKILL when SIGTERM is ignored', async () => {
    const result = await run('party', { write: null, stubborn: true });
    expect(result.code).toBe(1);
    expect(result.output).toContain('took longer than 15 minutes');
    expect(result.signals).toEqual(['SIGTERM', 'SIGKILL']);
    expect(result.terminal).toEqual(['save', 'restore:500:5:bf:8a3b']);
    expect(stagingLeft()).toEqual([]);
  });

  it('refuses an unprotected credential file', async () => {
    const result = await run('party', {
      write: { content: envelope('party@example.com'), mode: 0o644 },
    });
    expect(result.code).toBe(1);
    expect(result.output).toContain('could not be verified or saved safely');
    expect(fs.existsSync(path.join(ccsDir, 'antigravity-profiles'))).toBe(false);
  });

  it('starts nothing without a terminal, with a failed preflight, a bad name or a running switch', async () => {
    const noTerminal = await run(
      'party',
      { write: null },
      {
        io: {
          isInteractive: () => false,
          write: () => undefined,
          saveTerminal: () => null,
          restoreTerminal: () => undefined,
        },
      }
    );
    expect(noTerminal.code).toBe(1);
    expect(noTerminal.spawned).toEqual([]);
    const preflight = await run(
      'party',
      { write: null },
      {
        preflight: () => ({ ok: false, reason: 'preflight_failed', detail: 'No namespaces.' }),
      }
    );
    expect(preflight.code).toBe(1);
    expect(preflight.output).toContain('No namespaces. Nothing was started.');
    expect(preflight.spawned).toEqual([]);
    const badName = await run('Party Time', { write: null });
    expect(badName.code).toBe(1);
    expect(badName.spawned).toEqual([]);
    await save('gmail');
    const lock = path.join(ccsDir, 'antigravity-profiles', '.transaction-lock');
    fs.mkdirSync(lock, { mode: 0o700 });
    const locked = await run('party', { write: null });
    expect(locked.code).toBe(1);
    expect(locked.output).toContain('An Antigravity switch is running.');
    expect(locked.spawned).toEqual([]);
    expect(stagingLeft()).toEqual([]);
  });

  it('refuses Sign in again for the live login before starting anything', async () => {
    await save('gmail');
    const liveFile = path.join(home, '.gemini', 'antigravity-cli', 'antigravity-oauth-token');
    fs.mkdirSync(path.dirname(liveFile), { recursive: true });
    fs.writeFileSync(liveFile, 'placeholder', { mode: 0o600 });
    const live = {
      format: 'antigravity-consumer-json',
      bytes: Buffer.from(envelope('gmail@example.com', 3)),
    };
    const result = await run(
      'gmail',
      { write: null },
      {
        lifecycle: new AntigravityAccountLifecycle({
          ccsDir: () => ccsDir,
          home: () => home,
          now: () => NOW,
          validateCredential: async (value) => identityOf(value),
          readNativeCredential: async () => live,
        }),
      }
    );
    expect(result.code).toBe(1);
    expect(result.output).toContain('This profile is the live Antigravity login.');
    expect(result.spawned).toEqual([]);
  });
});

describe('terminal sign-in review fixes', () => {
  it('takes over a lock its own dead run left behind and saves the profile', async () => {
    await save('gmail');
    const child = spawn(process.execPath, ['--version'], { stdio: 'ignore' });
    const pid = child.pid as number;
    await new Promise<void>((resolve) => child.once('exit', () => resolve()));
    const lock = path.join(ccsDir, 'antigravity-profiles', '.transaction-lock');
    fs.mkdirSync(lock, { mode: 0o700 });
    fs.writeFileSync(
      path.join(lock, 'holder.json'),
      JSON.stringify({ pid, startedAt: new Date().toISOString() }),
      { mode: 0o600 }
    );
    const warn = spyOn(console, 'warn').mockImplementation(() => {});
    let result: Run;
    try {
      result = await run('party', { write: { content: envelope('party@example.com') } });
    } finally {
      warn.mockRestore();
    }
    expect(result.code).toBe(0);
    expect(fs.existsSync(lock)).toBe(false);
    expect(
      new AntigravityProfileRegistry(ccsDir).listProfiles().map((profile) => profile.id)
    ).toEqual(['gmail', 'party']);
  });

  it('reports an unreadable registry plainly and starts nothing', async () => {
    const profiles = path.join(ccsDir, 'antigravity-profiles');
    fs.mkdirSync(profiles, { mode: 0o700 });
    fs.writeFileSync(path.join(profiles, 'registry-000000000001.json'), '{not json', {
      mode: 0o600,
    });
    const result = await run('party', { write: null });
    expect(result.code).toBe(1);
    expect(result.output).toContain('could not be read safely. Nothing was started.');
    expect(result.spawned).toEqual([]);
  });

  it('refuses a second sign-in for a profile while one runs, and leaves no marker after a run', async () => {
    const held = claimAntigravitySignInMarker(ccsDir, 'party');
    expect(held).not.toBeNull();
    const second = await run('party', { write: null });
    expect(second.code).toBe(1);
    expect(second.output).toContain('A sign-in for this profile is already running.');
    expect(second.spawned).toEqual([]);
    held!.release();
    const done = await run('party', { write: { content: envelope('party@example.com') } });
    expect(done.code).toBe(0);
    expect(stagingLeft()).toEqual([]);
  });

  it('on SIGHUP stops the CLI, removes the new credential and the marker, then exits', async () => {
    const exits: number[] = [];
    // A private emitter: the test never raises a signal on the test process.
    const signals = new EventEmitter();
    let staging = '';
    const result = await run(
      'party',
      { write: { content: envelope('party@example.com'), afterPolls: 1 } },
      {
        guardSignals: true,
        signals,
        exit: (code) => {
          exits.push(code);
        },
        wait: (() => {
          let polls = 0;
          return async () => {
            polls += 1;
            await Promise.resolve();
            const directory = path.join(ccsDir, 'antigravity-signin');
            const name = fs.readdirSync(directory).find((entry) => entry.startsWith('.staging-'));
            if (polls === 1 && name) {
              staging = path.join(directory, name);
              fs.writeFileSync(
                path.join(staging, 'home', '.gemini', 'antigravity-cli', 'antigravity-oauth-token'),
                envelope('party@example.com'),
                { mode: 0o600 }
              );
              signals.emit('SIGHUP');
            }
          };
        })(),
      }
    );
    expect(exits).toEqual([129]);
    expect(result.signals[0]).toBe('SIGKILL');
    expect(staging).not.toBe('');
    expect(fs.existsSync(staging)).toBe(false);
    expect(stagingLeft()).toEqual([]);
    expect(result.code).toBe(1);
    expect(fs.existsSync(path.join(ccsDir, 'antigravity-profiles'))).toBe(false);
    for (const name of ['SIGHUP', 'SIGTERM', 'SIGINT', 'SIGQUIT', 'SIGTSTP'])
      expect(signals.listenerCount(name)).toBe(0);
  });

  it('masks the user runtime folder when the preflight found one', async () => {
    const result = await run(
      'party',
      { write: { content: envelope('party@example.com') } },
      {
        preflight: () => ({
          ok: true,
          nativeBinary: '/fixture/agy',
          apparmorLeaf: null,
          runtimeDir: '/run/user/4242',
        }),
      }
    );
    expect(result.code).toBe(0);
    const args = result.spawned[0].args;
    expect(args.slice(8, 12)).toEqual(['--tmpfs', '/tmp', '--tmpfs', '/run/user/4242']);
  });
});

describe('reading the new credential', () => {
  it('reads an owned 0600 single-link file and refuses links and loose modes', () => {
    const staging = prepareSignInStaging(ccsDir);
    fs.writeFileSync(staging.token, envelope('party@example.com'), { mode: 0o600 });
    expect(readMaskedCredential(staging).format).toBe('antigravity-consumer-json');
    fs.linkSync(staging.token, path.join(staging.directory, 'second-link'));
    expect(() => readMaskedCredential(staging)).toThrow();
    fs.rmSync(path.join(staging.directory, 'second-link'));
    fs.chmodSync(staging.token, 0o640);
    expect(() => readMaskedCredential(staging)).toThrow();
    fs.rmSync(staging.token);
    fs.symlinkSync(path.join(staging.directory, 'session-bus.conf'), staging.token);
    expect(() => readMaskedCredential(staging)).toThrow();
  });
});
