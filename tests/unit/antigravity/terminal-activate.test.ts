/**
 * The local activate command over fakes: the switch-service answers are
 * scripted, the terminal is a list of lines, and nothing native ever runs.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { AntigravityAccountLifecycle } from '../../../src/antigravity/account-lifecycle';
import { PrivateStorageError } from '../../../src/antigravity/registry';
import {
  ownsAntigravityState,
  runAntigravityTerminalActivate,
  type TerminalActivateDeps,
  type TerminalActivateRuntime,
} from '../../../src/antigravity/terminal-activate';
import type {
  ActivateRequest,
  ActivationResult,
  SafeProcessDisplay,
} from '../../../src/antigravity/types';

let root: string;
let ccsDir: string;
let lifecycle: AntigravityAccountLifecycle;

beforeEach(() => {
  root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agy-activate-')));
  ccsDir = path.join(root, '.ccs');
  fs.mkdirSync(ccsDir, { mode: 0o700 });
  lifecycle = new AntigravityAccountLifecycle({ ccsDir: () => ccsDir, home: () => root });
});

afterEach(() => {
  fs.rmSync(root, { recursive: true, force: true });
});

function harness(options: {
  results?: Array<ActivationResult | Error>;
  answers?: string[];
  interactive?: boolean;
  runtime?: TerminalActivateRuntime | null;
  ownsState?: (dir: string, uid: number | null) => boolean;
}): { deps: TerminalActivateDeps; lines: string[]; requests: ActivateRequest[] } {
  const lines: string[] = [];
  const requests: ActivateRequest[] = [];
  const script = [...(options.results ?? [])];
  const answers = [...(options.answers ?? [])];
  const runtime =
    options.runtime === undefined
      ? {
          activate: async (request: ActivateRequest) => {
            requests.push(request);
            const next = script.shift();
            if (next instanceof Error) throw next;
            if (!next) throw new Error('No scripted activation result.');
            return next;
          },
        }
      : options.runtime;
  return {
    deps: {
      runtime,
      lifecycle,
      ccsDir,
      uid: process.getuid?.() ?? null,
      io: {
        isInteractive: () => options.interactive ?? true,
        write: (text) => lines.push(text),
        readAnswer: async () => answers.shift() ?? '',
      },
      ...(options.ownsState ? { ownsState: options.ownsState } : {}),
    },
    lines,
    requests,
  };
}

function confirmationResult(processes: unknown): ActivationResult {
  return {
    status: 'confirmation-required',
    profileId: 'party',
    hostId: 'ubuntu',
    email: 'party@example.com',
    confirmation: {
      token: 'owned-fixture-token-0123456789',
      expiresAt: new Date(Date.now() + 60_000).toISOString(),
      profileId: 'party',
      hostId: 'ubuntu',
      email: 'party@example.com',
      processes: processes as SafeProcessDisplay[],
      warning: 'fixture warning, never shown',
    },
  };
}

describe('local antigravity activate', () => {
  it('refuses foreign state and bad names before touching the switcher', async () => {
    const foreign = harness({ ownsState: () => false });
    expect(await runAntigravityTerminalActivate('party', foreign.deps)).toBe(1);
    expect(foreign.lines.join('')).toContain('only as the user that owns');
    expect(foreign.requests).toHaveLength(0);
    const badName = harness({});
    expect(await runAntigravityTerminalActivate('Party!', badName.deps)).toBe(1);
    expect(badName.lines.join('')).toContain('Profile names start with a lowercase letter');
    expect(badName.requests).toHaveLength(0);
    const missing = harness({ runtime: null });
    expect(await runAntigravityTerminalActivate('party', missing.deps)).toBe(1);
    expect(missing.lines.join('')).toContain('No Antigravity profiles');
  });

  it('runs the same manual Ubuntu request the dashboard route sends', async () => {
    const active = harness({
      results: [
        { status: 'active', profileId: 'party', hostId: 'ubuntu', email: 'party@example.com' },
      ],
    });
    expect(await runAntigravityTerminalActivate('party', active.deps)).toBe(0);
    expect(active.requests).toEqual([{ profileId: 'party', hostId: 'ubuntu', mode: 'manual' }]);
    expect(active.lines.join('')).toContain('party (party@example.com) is now the active profile');
    const already = harness({
      results: [{ status: 'already-active', profileId: 'party', hostId: 'ubuntu' }],
    });
    expect(await runAntigravityTerminalActivate('party', already.deps)).toBe(0);
    expect(already.lines.join('')).toContain('already active');
  });

  it('names each terminal outcome without retrying', async () => {
    for (const [status, headline] of [
      ['invalid-profile', 'No saved Antigravity profile'],
      ['busy', 'An Antigravity switch is running'],
      ['deferred', 'deferred this switch'],
      ['failed-rolled-back', 'rolled the live login back'],
      ['recovery-required', 'antigravity recover'],
      ['stale-confirmation', 'review expired'],
      ['unsupported-runtime-probe', 'switching is paused until the runtime is reviewed'],
    ] as const) {
      const run = harness({
        results: [{ status, profileId: 'party', hostId: 'ubuntu' }],
      });
      expect(await runAntigravityTerminalActivate('party', run.deps)).toBe(1);
      expect(run.lines.join('')).toContain(headline);
      expect(run.requests).toHaveLength(1);
    }
  });

  it('lists running programs, then confirms with the issued token only on yes', async () => {
    const yes = harness({
      results: [
        confirmationResult([
          { pid: 4242, role: 'cli' },
          { pid: 8484, role: 'language-server' },
        ]),
        { status: 'active', profileId: 'party', hostId: 'ubuntu', email: 'party@example.com' },
      ],
      answers: ['y'],
    });
    expect(await runAntigravityTerminalActivate('party', yes.deps)).toBe(0);
    expect(yes.requests).toHaveLength(2);
    expect(yes.requests[1]).toMatchObject({
      profileId: 'party',
      hostId: 'ubuntu',
      mode: 'manual',
      confirmationToken: 'owned-fixture-token-0123456789',
    });
    const text = yes.lines.join('');
    expect(text).toContain('Antigravity CLI, process 4242');
    expect(text).toContain('Antigravity language server, process 8484');
    expect(text).not.toContain('fixture warning');
    for (const answer of ['', 'n', 'no', 'YES please']) {
      const run = harness({
        results: [confirmationResult([{ pid: 4242, role: 'cli' }])],
        answers: [answer],
      });
      expect(await runAntigravityTerminalActivate('party', run.deps)).toBe(1);
      expect(run.requests).toHaveLength(1);
      expect(run.lines.join('')).toContain('Nothing was changed');
    }
    const padded = harness({
      results: [
        confirmationResult([{ pid: 4242, role: 'cli' }]),
        { status: 'already-active', profileId: 'party', hostId: 'ubuntu' },
      ],
      answers: ['  YES  '],
    });
    expect(await runAntigravityTerminalActivate('party', padded.deps)).toBe(0);
    expect(padded.requests).toHaveLength(2);
  });

  it('needs an interactive terminal and a safe program list before any review', async () => {
    const headless = harness({
      results: [confirmationResult([{ pid: 4242, role: 'cli' }])],
      interactive: false,
    });
    expect(await runAntigravityTerminalActivate('party', headless.deps)).toBe(1);
    expect(headless.lines.join('')).toContain('not an interactive terminal');
    expect(headless.requests).toHaveLength(1);
    for (const processes of [
      [{ pid: 4242, role: 'owned-fake-role' }],
      [{ pid: '4242', role: 'cli' }],
      [{ pid: 0, role: 'cli' }],
      Array.from({ length: 33 }, (_, index) => ({ pid: index + 1, role: 'cli' })),
      'not-a-list',
    ]) {
      const run = harness({ results: [confirmationResult(processes)] });
      expect(await runAntigravityTerminalActivate('party', run.deps)).toBe(1);
      expect(run.lines.join('')).toContain('could not be listed safely');
      expect(run.lines.join('')).not.toContain('owned-fake');
      expect(run.requests).toHaveLength(1);
    }
    const relabeled = harness({
      results: [
        confirmationResult([{ pid: 4242, role: 'cli', label: 'owned-fake-label; rm -rf /' }]),
      ],
      answers: ['n'],
    });
    expect(await runAntigravityTerminalActivate('party', relabeled.deps)).toBe(1);
    expect(relabeled.lines.join('')).toContain('Antigravity CLI, process 4242');
    expect(relabeled.lines.join('')).not.toContain('owned-fake');
  });

  it('reports an expired review and a busy lock without changing anything', async () => {
    const expired = harness({
      results: [
        confirmationResult([{ pid: 4242, role: 'cli' }]),
        { status: 'stale-confirmation', profileId: 'party', hostId: 'ubuntu' },
      ],
      answers: ['yes'],
    });
    expect(await runAntigravityTerminalActivate('party', expired.deps)).toBe(1);
    expect(expired.lines.join('')).toContain('review expired');
    const locked = harness({ results: [new PrivateStorageError('busy')] });
    expect(await runAntigravityTerminalActivate('party', locked.deps)).toBe(1);
    expect(locked.lines.join('')).toContain('An Antigravity switch is running');
  });

  it('owns only a real directory held by the calling user', () => {
    const uid = process.getuid?.() ?? null;
    expect(ownsAntigravityState(ccsDir, uid)).toBe(true);
    expect(ownsAntigravityState(ccsDir, (uid ?? 0) + 1)).toBe(false);
    expect(ownsAntigravityState(ccsDir, null)).toBe(false);
    expect(ownsAntigravityState(path.join(root, 'missing'), uid)).toBe(false);
    const file = path.join(root, 'file');
    fs.writeFileSync(file, 'x');
    expect(ownsAntigravityState(file, uid)).toBe(false);
    const link = path.join(root, 'link');
    fs.symlinkSync(ccsDir, link);
    expect(ownsAntigravityState(link, uid)).toBe(false);
  });
});
