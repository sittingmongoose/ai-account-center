/**
 * The sign-in job runner (CONTRACT-registry-lifecycle section 6.6) with a fake
 * CLI and injected timers: states, limits, timeouts, cancel, restart.
 */
import { describe, expect, it } from 'bun:test';
import {
  SignInJobConflict,
  SignInJobError,
  SignInJobRunner,
  type SignInFlowSpec,
  type SignInJob,
} from '../../../src/web-server/services/signin-jobs';
import type {
  SignInCommand,
  SignInProcessHandle,
  SignInSpawnFailure,
} from '../../../src/web-server/services/signin-process';

class FakeProcess implements SignInProcessHandle {
  private readonly dataListeners: Array<(chunk: string) => void> = [];
  private readonly exitListeners: Array<(code: number | null, f: SignInSpawnFailure) => void> = [];
  killed = 0;
  written: string[] = [];
  onData(listener: (chunk: string) => void) {
    this.dataListeners.push(listener);
  }
  onExit(listener: (code: number | null, failure: SignInSpawnFailure) => void) {
    this.exitListeners.push(listener);
  }
  write(text: string) {
    this.written.push(text);
    return true;
  }
  kill() {
    this.killed += 1;
  }
  emit(chunk: string) {
    for (const listener of this.dataListeners) listener(chunk);
  }
  exit(code: number | null, failure: SignInSpawnFailure = null) {
    for (const listener of this.exitListeners) listener(code, failure);
  }
}

interface Timer {
  callback: () => void;
  ms: number;
  cancelled: boolean;
}

const OUTPUT =
  'Open this link\n   https://auth.openai.com/codex/device?state=PRIVATE\nEnter this code\n   ABCD-12345\n';

function harness(options: { now?: () => number } = {}) {
  const processes: FakeProcess[] = [];
  const commands: SignInCommand[] = [];
  const timers: Timer[] = [];
  const changes: SignInJob[] = [];
  const finished: SignInJob[] = [];
  const runner = new SignInJobRunner({
    spawn: (command) => {
      commands.push(command);
      const process = new FakeProcess();
      processes.push(process);
      return process;
    },
    setTimer: (callback, ms) => {
      const timer = { callback, ms, cancelled: false };
      timers.push(timer);
      return () => {
        timer.cancelled = true;
      };
    },
    now: options.now,
    onChange: (job) => changes.push(job),
    onFinish: (job) => finished.push(job),
  });
  const fire = (ms: number) => {
    for (const timer of timers) if (timer.ms === ms && !timer.cancelled) timer.callback();
  };
  return { runner, processes, commands, timers, changes, finished, fire };
}

function spec(overrides: Partial<SignInFlowSpec> = {}) {
  const calls = { prepare: 0, complete: 0, cleanup: 0 };
  const value: SignInFlowSpec = {
    provider: 'codex',
    kind: 'device-code',
    mode: 'add',
    accountId: null,
    profileName: 'codex-4',
    platform: 'ubuntu',
    allowedOrigins: ['https://auth.openai.com'],
    timeoutMs: 15 * 60_000,
    prepare: async () => {
      calls.prepare += 1;
      return { file: '/fake/codex', args: ['login', '--device-auth'], env: {}, pty: true };
    },
    complete: async () => {
      calls.complete += 1;
      return { accountId: 'codex:codex-4', email: 'new@example.com', plan: 'pro' };
    },
    cleanup: async () => {
      calls.cleanup += 1;
    },
    ...overrides,
  };
  return { value, calls };
}

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

describe('SignInJobRunner', () => {
  it('parses the URL and code into waiting, then succeeds after a clean exit', async () => {
    const h = harness();
    const { value, calls } = spec();
    const job = h.runner.start(value);
    expect(job).toMatchObject({ state: 'starting', provider: 'codex', mode: 'add' });
    expect(job.id).toMatch(/^job_[a-f0-9]{16}$/);
    await tick();
    expect(h.commands).toEqual([
      { file: '/fake/codex', args: ['login', '--device-auth'], env: {}, pty: true },
    ]);
    h.processes[0].emit(OUTPUT);
    expect(h.runner.get(job.id)).toMatchObject({
      state: 'waiting',
      verification: { url: 'https://auth.openai.com/codex/device', userCode: 'ABCD-12345' },
    });
    h.processes[0].exit(0);
    await tick();
    await tick();
    const done = h.runner.get(job.id) as SignInJob;
    expect(done).toMatchObject({
      state: 'succeeded',
      accountId: 'codex:codex-4',
      result: { accountId: 'codex:codex-4', email: 'new@example.com', plan: 'pro' },
      verification: null,
      error: null,
    });
    expect(calls).toEqual({ prepare: 1, complete: 1, cleanup: 1 });
    expect(h.changes.map((change) => change.state)).toEqual([
      'starting',
      'waiting',
      'verifying',
      'succeeded',
    ]);
    expect(h.finished).toHaveLength(1);
    // Output beyond the two facts never reaches a job or an event.
    expect(JSON.stringify([h.changes, h.finished, done])).not.toContain('PRIVATE');
    expect(JSON.stringify(h.changes)).not.toContain('Enter this code');
  });

  it('fails an off-allowlist URL with unexpected_output and kills the CLI', async () => {
    const h = harness();
    const { value, calls } = spec();
    const job = h.runner.start(value);
    await tick();
    h.processes[0].emit('Open https://evil.example/codex/device\nABCD-1234\n');
    expect(h.runner.get(job.id)).toMatchObject({
      state: 'failed',
      error: { code: 'unexpected_output' },
    });
    expect(h.processes[0].killed).toBe(1);
    await tick();
    expect(calls.cleanup).toBe(1);
    expect(calls.complete).toBe(0);
  });

  it('allows one running job per provider and three in all', async () => {
    const h = harness();
    const first = h.runner.start(spec().value);
    expect(() => h.runner.start(spec().value)).toThrow(SignInJobConflict);
    try {
      h.runner.start(spec().value);
    } catch (error) {
      expect(error).toMatchObject({ code: 'job_running', jobId: first.id });
    }
    h.runner.start(spec({ provider: 'muse' }).value);
    h.runner.start(spec({ provider: 'antigravity', kind: 'supervised-cli' }).value);
    expect(h.runner.list()).toHaveLength(3);
    h.runner.cancel(first.id);
    expect(h.runner.list()).toHaveLength(2);
    expect(() => h.runner.start(spec().value)).not.toThrow();
  });

  it('cancel kills the process group and ends the job as cancelled', async () => {
    const h = harness();
    const job = h.runner.start(spec().value);
    await tick();
    h.processes[0].emit(OUTPUT);
    expect(h.runner.cancel(job.id)).toMatchObject({ state: 'cancelled', verification: null });
    expect(h.processes[0].killed).toBe(1);
    h.processes[0].exit(0);
    await tick();
    expect(h.runner.get(job.id)).toMatchObject({ state: 'cancelled' });
  });

  it('expires after the flow timeout and fails without output after 30 s', async () => {
    const h = harness();
    const expiring = h.runner.start(spec().value);
    await tick();
    h.processes[0].emit(OUTPUT);
    h.fire(30_000);
    expect(h.runner.get(expiring.id)).toMatchObject({ state: 'waiting' });
    h.fire(15 * 60_000);
    expect(h.runner.get(expiring.id)).toMatchObject({
      state: 'expired',
      error: { code: 'timeout' },
    });
    expect(h.processes[0].killed).toBe(1);
    const silent = h.runner.start(spec().value);
    await tick();
    h.fire(30_000);
    expect(h.runner.get(silent.id)).toMatchObject({
      state: 'failed',
      error: { code: 'unexpected_output' },
    });
  });

  it('maps exits and completion failures to their codes', async () => {
    const cases: Array<[string, (p: FakeProcess) => void, Partial<SignInFlowSpec>, string]> = [
      ['denied', (p) => (p.emit(OUTPUT), p.exit(1)), {}, 'provider_denied'],
      ['missing', (p) => p.exit(null, 'missing'), {}, 'tool_missing'],
      ['127', (p) => p.exit(127), {}, 'tool_missing'],
      ['silent exit', (p) => p.exit(0), {}, 'unexpected_output'],
      [
        'mismatch',
        (p) => (p.emit(OUTPUT), p.exit(0)),
        {
          complete: async () => {
            throw new SignInJobError('identity_mismatch');
          },
        },
        'identity_mismatch',
      ],
      [
        'write',
        (p) => (p.emit(OUTPUT), p.exit(0)),
        {
          complete: async () => {
            throw new Error('disk full at /home/private/path');
          },
        },
        'write_failed',
      ],
    ];
    for (const [, act, overrides, code] of cases) {
      const h = harness();
      const job = h.runner.start(spec(overrides).value);
      await tick();
      act(h.processes[0]);
      await tick();
      await tick();
      const result = h.runner.get(job.id) as SignInJob;
      expect([code, result.state, result.error?.code]).toEqual([code, 'failed', code]);
      expect(JSON.stringify(result)).not.toContain('/home/private');
    }
  });

  it('a prepare failure ends the job before anything runs', async () => {
    const h = harness();
    const job = h.runner.start(
      spec({
        prepare: async () => {
          throw new SignInJobError('tool_missing');
        },
      }).value
    );
    await tick();
    expect(h.processes).toHaveLength(0);
    expect(h.runner.get(job.id)).toMatchObject({
      state: 'failed',
      error: { code: 'tool_missing' },
    });
  });

  it('reports ids it never issued as failed with server_restarted, and forgets old jobs', async () => {
    let now = 1_000;
    const h = harness({ now: () => now });
    expect(h.runner.get('job_0123456789abcdef')).toMatchObject({
      id: 'job_0123456789abcdef',
      state: 'failed',
      provider: null,
      error: { code: 'server_restarted' },
    });
    const job = h.runner.start(spec().value);
    h.runner.cancel(job.id);
    now += 9 * 60_000;
    expect(h.runner.get(job.id)).toMatchObject({ state: 'cancelled' });
    now += 2 * 60_000;
    expect(h.runner.get(job.id)).toBeNull();
  });

  it('writes a supervised authorization code once and only when awaited', async () => {
    const h = harness();
    const job = h.runner.start(
      spec({
        provider: 'antigravity',
        kind: 'supervised-cli',
        allowedOrigins: ['https://accounts.google.com'],
      }).value
    );
    await tick();
    expect(h.runner.submitCode(job.id, 'abc')).toBe('not_expected');
    h.processes[0].emit('Visit https://accounts.google.com/o/oauth2/auth?state=1\n');
    expect(h.runner.get(job.id)).toMatchObject({ state: 'awaiting_code' });
    expect(h.runner.submitCode(job.id, 'bad code with spaces')).toBe('not_expected');
    expect(h.runner.submitCode(job.id, '4/0AbCd-EfGh_ij')).toMatchObject({ state: 'verifying' });
    expect(h.processes[0].written).toEqual(['4/0AbCd-EfGh_ij\n']);
    expect(h.runner.submitCode(job.id, '4/0AbCd-EfGh_ij')).toBe('not_expected');
    const device = harness();
    const deviceJob = device.runner.start(spec().value);
    await tick();
    device.processes[0].emit(OUTPUT);
    expect(device.runner.submitCode(deviceJob.id, 'abc')).toBe('not_expected');
  });

  it('lays running sign-in-again jobs over their accounts and stops everything on shutdown', async () => {
    const h = harness();
    const job = h.runner.start(spec({ mode: 'signin-again', accountId: 'codex:party' }).value);
    await tick();
    expect([...h.runner.accountStates()]).toEqual([
      ['codex:party', { state: 'signing_in', jobId: job.id }],
    ]);
    expect(h.runner.runningForAccount('codex:party')?.id).toBe(job.id);
    expect(h.runner.runningForProvider('codex')?.id).toBe(job.id);
    h.runner.shutdown();
    expect(h.runner.get(job.id)).toMatchObject({
      state: 'failed',
      error: { code: 'server_restarted' },
    });
    expect(h.processes[0].killed).toBe(1);
    expect(h.runner.accountStates().size).toBe(0);
  });
});
