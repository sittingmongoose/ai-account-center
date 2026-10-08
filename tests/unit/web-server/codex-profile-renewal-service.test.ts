import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import { CodexProfileRenewalService } from '../../../src/web-server/services/codex-profile-renewal-service';
import type { CodexRenewalCycle } from '../../../src/codex-auth/codex-profile-renewal';
import {
  ALPHA,
  DAY,
  NOW,
  RenewalSandbox,
  expectNoSecrets,
} from '../codex-auth/codex-renewal-fixtures';

let box: RenewalSandbox;

beforeEach(() => {
  box = new RenewalSandbox();
  box.addProfile('alpha', {
    account: ALPHA,
    refresh: 'rt-FAKE-svc',
    label: 'svc',
    accessExp: NOW - DAY,
  });
});

afterEach(() => {
  box.cleanup();
});

function cycle(retryAt: string | null = null): CodexRenewalCycle {
  const at = new Date(NOW).toISOString();
  return { startedAt: at, finishedAt: at, enabled: true, plans: [], attempts: [], retryAt };
}

describe('CodexProfileRenewalService', () => {
  it('does not schedule anything when CCS_CODEX_RENEWAL=0', async () => {
    const service = new CodexProfileRenewalService({
      ccsDir: box.ccsDir,
      env: { CCS_CODEX_RENEWAL: '0' },
      now: () => NOW,
      renewal: box.options(),
    });
    service.start();
    const status = await service.getStatus();
    expect(status.enabled).toBe(false);
    expect(status.running).toBe(false);
    expect(status.nextCycleAt).toBeNull();
    expect(status.profiles[0].reason).toBe('disabled');
    service.stop();
  });

  it('schedules the first cycle 3-5 minutes after start and clears it on stop', async () => {
    const service = new CodexProfileRenewalService({
      ccsDir: box.ccsDir,
      env: {},
      now: () => NOW,
      random: () => 0.5,
      renewal: box.options(),
    });
    service.start();
    const status = await service.getStatus();
    expect(status.running).toBe(true);
    expect(Date.parse(status.nextCycleAt ?? '')).toBe(NOW + 4 * 60_000);
    expect(status.profiles[0]).toMatchObject({ name: 'alpha', state: 'due', reason: 'due' });
    expectNoSecrets(status);
    service.stop();
    expect((await service.getStatus()).nextCycleAt).toBeNull();
  });

  it('coalesces concurrent cycles and shows the profile being renewed', async () => {
    let release!: () => void;
    const gate = new Promise<void>((done) => (release = done));
    let calls = 0;
    let observed: string | null = null;
    const service = new CodexProfileRenewalService({
      ccsDir: box.ccsDir,
      env: {},
      now: () => NOW,
      renewal: box.options(),
      runCycle: async (options) => {
        calls++;
        options.onRenewing?.('alpha');
        observed = (await service.getStatus()).profiles[0].state;
        await gate;
        options.onRenewing?.(null);
        return cycle();
      },
    });
    const first = service.runCycle();
    const second = service.runCycle();
    expect(first).toBe(second);
    await Promise.resolve();
    await Promise.resolve();
    release();
    await first;
    expect(calls).toBe(1);
    expect(observed).toBe('renewing');
    const status = await service.getStatus();
    expect(status.renewing).toBeNull();
    expect(status.lastCycleAt).toBe(new Date(NOW).toISOString());
  });

  it('stops before the next profile once stopped', async () => {
    let continueAfterStop: boolean | undefined;
    const service = new CodexProfileRenewalService({
      ccsDir: box.ccsDir,
      env: {},
      now: () => NOW,
      renewal: box.options(),
      runCycle: async (options) => {
        service.stop();
        continueAfterStop = options.shouldContinue?.();
        return cycle();
      },
    });
    await service.runCycle();
    expect(continueAfterStop).toBe(false);
  });

  it('reports a failed cycle with a fixed message and never rejects', async () => {
    const service = new CodexProfileRenewalService({
      ccsDir: box.ccsDir,
      env: {},
      now: () => NOW,
      renewal: box.options(),
      runCycle: async () => {
        throw new Error('registry rt-FAKE-svc broke');
      },
    });
    expect(await service.runCycle()).toBeNull();
    const status = await service.getStatus();
    expect(status.message).toBe('The last renewal check could not read the saved Codex profiles.');
    expectNoSecrets(status);
  });
});
