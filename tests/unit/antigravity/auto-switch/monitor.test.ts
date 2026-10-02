import { describe, expect, test } from 'bun:test';
import { AntigravityAutoSwitchService } from '../../../../src/antigravity/auto-switch/monitor';
import type {
  AntigravityAutoSwitchDeps,
  AntigravityAutoSwitchStoredState,
} from '../../../../src/antigravity/auto-switch/types';
import { NOW, copy, observation, state } from './fixtures';

function harness(overrides: Partial<AntigravityAutoSwitchDeps> = {}) {
  let saved = state();
  let time = NOW;
  let observations = 0;
  let activations = 0;
  const requests: Parameters<AntigravityAutoSwitchDeps['activate']>[0][] = [];
  const timers = new Map<object, { callback: () => void; delay: number }>();
  const deps: AntigravityAutoSwitchDeps = {
    store: {
      read: () => copy(saved),
      write: (value) => {
        saved = copy(value);
      },
    },
    observe: async () => {
      observations++;
      return observation();
    },
    activate: async (request) => {
      activations++;
      requests.push(request);
      const allowed = await request.revalidateAutomatic({
        hostId: 'ubuntu',
        currentIdentityKey: 'verified-google-subject:gmail',
        targetIdentityKey: 'verified-google-subject:party',
      });
      return { status: allowed ? 'active' : 'deferred' };
    },
    now: () => time,
    setTimer: (callback, delay) => {
      const token = {};
      timers.set(token, { callback, delay });
      return token;
    },
    clearTimer: (token) => {
      timers.delete(token as object);
    },
    ...overrides,
  };
  const service = new AntigravityAutoSwitchService(deps);
  return {
    service,
    deps,
    requests,
    timers,
    setTime: (value: number) => {
      time = value;
    },
    getState: () => copy(saved),
    setState: (value: AntigravityAutoSwitchStoredState) => {
      saved = copy(value);
    },
    counts: () => ({ observations, activations }),
  };
}

describe('automatic monitor under transaction guards', () => {
  test('uses only an automatic request and persists an actual successful switch', async () => {
    const value = harness();
    await value.service.runCycle();
    expect(value.counts()).toEqual({ observations: 2, activations: 1 });
    expect(value.requests[0].mode).toBe('automatic');
    expect(value.requests[0].hostId).toBe('ubuntu');
    expect(Object.keys(value.requests[0]).sort()).toEqual([
      'expectedActiveIdentityKey',
      'hostId',
      'mode',
      'profileId',
      'revalidateAutomatic',
    ]);
    expect(value.getState().lastSwitch).toEqual({
      at: new Date(NOW).toISOString(),
      hostId: 'ubuntu',
      profileId: 'party',
    });
    expect(value.service.getStatus().outcome).toBe('switched');
    expect(value.service.getStatus().activationInProgress).toBe(false);
    const serialized = JSON.stringify(value.service.getStatus());
    expect(serialized).not.toContain('verified-google-subject');
    expect(serialized).not.toContain('private-revision');
    expect(serialized).not.toContain('confirmationToken');
  });

  test('disabled or missing-pool setup never queries a vendor or attempts activation', async () => {
    const value = harness();
    const disabled = state();
    disabled.settings.enabled = false;
    value.setState(disabled);
    await value.service.runCycle();
    expect(value.counts()).toEqual({ observations: 0, activations: 0 });
    expect(value.service.getStatus().outcome).toBe('disabled');
    disabled.settings.enabled = true;
    disabled.settings.requestedPoolId = null;
    value.setState(disabled);
    await value.service.runCycle();
    expect(value.counts()).toEqual({ observations: 0, activations: 0 });
    expect(value.service.getStatus().outcome).toBe('setup_required');
  });

  test('concurrent cycles share one observation and one activation', async () => {
    let resolve!: () => void;
    const pending = new Promise<void>((done) => {
      resolve = done;
    });
    const value = harness({
      observe: async () => {
        await pending;
        return observation();
      },
    });
    const first = value.service.runCycle();
    const second = value.service.runCycle();
    expect(first).toBe(second);
    resolve();
    await Promise.all([first, second]);
    expect(value.requests).toHaveLength(1);
  });

  test('disabled while an observation is pending cancels activation', async () => {
    let resolve!: () => void;
    const pending = new Promise<void>((done) => {
      resolve = done;
    });
    const value = harness({
      observe: async () => {
        await pending;
        return observation();
      },
    });
    const cycle = value.service.runCycle();
    await Promise.resolve();
    value.service.updateSettings({ enabled: false });
    resolve();
    await cycle;
    expect(value.requests).toHaveLength(0);
    expect(value.service.getStatus().outcome).toBe('disabled');
  });

  test('settings changed outside the service during observation cancel the earlier decision', async () => {
    let resolve!: () => void;
    const pending = new Promise<void>((done) => {
      resolve = done;
    });
    const value = harness({
      observe: async () => {
        await pending;
        return observation();
      },
    });
    const cycle = value.service.runCycle();
    await Promise.resolve();
    const changed = value.getState();
    changed.settings.thresholdUsedPercent = 99;
    value.setState(changed);
    resolve();
    await cycle;
    expect(value.requests).toHaveLength(0);
    expect(value.service.getStatus().outcome).toBe('deferred');
  });

  test('stop while a check is pending cancels it and clears its only timer', async () => {
    let resolve!: () => void;
    const pending = new Promise<void>((done) => {
      resolve = done;
    });
    const value = harness({
      observe: async () => {
        await pending;
        return observation();
      },
    });
    value.service.start();
    expect(value.timers.size).toBe(1);
    const cycle = value.service.runCycle();
    await Promise.resolve();
    value.service.stop();
    resolve();
    await cycle;
    expect(value.timers.size).toBe(0);
    expect(value.requests).toHaveLength(0);
  });

  test('revalidates settings while manual activation lock was pending', async () => {
    let service!: AntigravityAutoSwitchService;
    const value = harness({
      activate: async (request) => {
        service.updateSettings({ enabled: false });
        const valid = await request.revalidateAutomatic({
          hostId: 'ubuntu',
          currentIdentityKey: 'verified-google-subject:gmail',
          targetIdentityKey: 'verified-google-subject:party',
        });
        expect(valid).toBe(false);
        return { status: 'deferred' };
      },
    });
    service = value.service;
    await service.runCycle();
    expect(value.getState().lastSwitch).toBeNull();
    expect(service.getStatus().enabled).toBe(false);
  });

  test('revalidates again if user settings change during the under-lock observation', async () => {
    let service!: AntigravityAutoSwitchService;
    let observationCount = 0;
    const value = harness({
      observe: async () => {
        observationCount++;
        if (observationCount === 2) service.updateSettings({ requestedPoolId: 'claude-gpt-pool' });
        return observation();
      },
    });
    service = value.service;
    await service.runCycle();
    expect(value.getState().lastSwitch).toBeNull();
    expect(service.getStatus().outcome).toBe('deferred');
  });

  test('actual native current or target identity mismatch rejects the under-lock callback', async () => {
    for (const context of [
      {
        hostId: 'ubuntu' as const,
        currentIdentityKey: 'foreign-current',
        targetIdentityKey: 'verified-google-subject:party',
      },
      {
        hostId: 'ubuntu' as const,
        currentIdentityKey: 'verified-google-subject:gmail',
        targetIdentityKey: 'foreign-target',
      },
    ]) {
      const value = harness({
        activate: async (request) => ({
          status: (await request.revalidateAutomatic(context)) ? 'active' : 'deferred',
        }),
      });
      await value.service.runCycle();
      expect(value.getState().lastSwitch).toBeNull();
      expect(value.service.getStatus().outcome).toBe('deferred');
    }
  });

  test('quota reset crossing while waiting for the lock rejects the queued candidate', async () => {
    let count = 0;
    const value = harness({
      observe: async () => {
        const reading = observation();
        count++;
        if (count === 2)
          reading.quotas[1].pools[0].windows[0].resetAt = new Date(NOW).toISOString();
        return reading;
      },
    });
    await value.service.runCycle();
    expect(value.getState().lastSwitch).toBeNull();
    expect(value.service.getStatus().outcome).toBe('deferred');
  });

  test('native process becoming busy under the lock defers, without a confirmation or stop', async () => {
    let count = 0;
    const value = harness({
      observe: async () => {
        const reading = observation();
        if (++count === 2) reading.hosts[0].busy = true;
        return reading;
      },
    });
    await value.service.runCycle();
    expect(value.getState().lastSwitch).toBeNull();
    expect(value.service.getStatus().outcome).toBe('deferred');
    expect(value.requests[0]).not.toHaveProperty('confirmationToken');
  });

  test('busy results cannot be auto-confirmed and do not record a switch', async () => {
    for (const status of ['busy', 'confirmation-required'] as const) {
      const value = harness({ activate: async () => ({ status }) });
      await value.service.runCycle();
      expect(value.service.getStatus().outcome).toBe('waiting_idle');
      expect(value.getState().lastSwitch).toBeNull();
    }
  });

  test('cannot accept an adapter success that skipped the required locked validation', async () => {
    const value = harness({ activate: async () => ({ status: 'active' }) });
    await value.service.runCycle();
    expect(value.service.getStatus().outcome).toBe('error');
    expect(value.getState().lastSwitch).toBeNull();
  });

  test('a later failed locked callback supersedes an earlier successful callback', async () => {
    const value = harness({
      activate: async (request) => {
        expect(
          await request.revalidateAutomatic({
            hostId: 'ubuntu',
            currentIdentityKey: 'verified-google-subject:gmail',
            targetIdentityKey: 'verified-google-subject:party',
          })
        ).toBe(true);
        expect(
          await request.revalidateAutomatic({
            hostId: 'ubuntu',
            currentIdentityKey: 'verified-google-subject:gmail',
            targetIdentityKey: 'foreign-target',
          })
        ).toBe(false);
        // An incorrect adapter must not manufacture a completed public switch.
        return { status: 'active' };
      },
    });
    await value.service.runCycle();
    expect(value.service.getStatus().outcome).toBe('error');
    expect(value.getState().lastSwitch).toBeNull();
  });

  test('transaction failure and raw errors use fixed public messages and back off', async () => {
    let reads = 0;
    const value = harness({
      observe: async () => {
        reads++;
        throw new Error('Bearer private-provider-value');
      },
    });
    await value.service.runCycle();
    await value.service.runCycle();
    expect(reads).toBe(1);
    expect(value.service.getStatus().outcome).toBe('error');
    expect(JSON.stringify(value.service.getStatus())).not.toContain('Bearer');
    value.setTime(NOW + 300_000);
    await value.service.runCycle();
    expect(reads).toBe(2);
  });

  test('retains changed settings after native activation while persisting the switch', async () => {
    let service!: AntigravityAutoSwitchService;
    const value = harness({
      activate: async (request) => {
        expect(
          await request.revalidateAutomatic({
            hostId: 'ubuntu',
            currentIdentityKey: 'verified-google-subject:gmail',
            targetIdentityKey: 'verified-google-subject:party',
          })
        ).toBe(true);
        service.updateSettings({ enabled: false, pollIntervalSeconds: 120 });
        return { status: 'active' };
      },
    });
    service = value.service;
    await service.runCycle();
    expect(value.getState().settings.enabled).toBe(false);
    expect(value.getState().settings.pollIntervalSeconds).toBe(120);
    expect(value.getState().lastSwitch?.profileId).toBe('party');
  });

  test('persisted cooldown blocks another monitor instance from bouncing', async () => {
    const value = harness();
    await value.service.runCycle();
    const recreated = new AntigravityAutoSwitchService(value.deps);
    await recreated.runCycle();
    expect(value.requests).toHaveLength(1);
    expect(recreated.getStatus().outcome).toBe('cooldown');
  });

  test('single owned timer respects changed poll interval, repeated starts and stop', async () => {
    const value = harness();
    value.service.start();
    value.service.start();
    expect(value.timers.size).toBe(1);
    value.service.updateSettings({ enabled: false, pollIntervalSeconds: 120 });
    expect([...value.timers.values()][0].delay).toBe(120_000);
    value.service.stop();
    expect(value.timers.size).toBe(0);
  });

  test('a settings write failure does not change the persisted decision or run a switch', () => {
    const value = harness({
      store: {
        read: () => state(),
        write: () => {
          throw new Error('private path');
        },
      },
    });
    expect(() => value.service.updateSettings({ enabled: false })).toThrow();
    expect(value.service.getStatus().enabled).toBe(true);
    expect(value.requests).toHaveLength(0);
  });

  test('a failed post-switch state write keeps the in-memory cooldown and reports an error', async () => {
    const original = state();
    let writes = 0;
    const value = harness({
      store: {
        read: () => copy(original),
        write: () => {
          writes++;
          throw new Error('fixture storage failed');
        },
      },
    });
    await value.service.runCycle();
    expect(writes).toBe(1);
    expect(value.service.getStatus().outcome).toBe('error');
    expect(value.service.getStatus().lastProfileId).toBe('party');
    value.setTime(NOW + 300_000 - 1);
    await value.service.runCycle();
    expect(value.requests).toHaveLength(1);
  });
});
