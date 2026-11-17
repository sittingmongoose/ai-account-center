/**
 * R3 review L1: while the Rust service hosts the store, its backup/maintain failures still
 * reach the operator as the in-process loop's warnings. The watch polls the host's job
 * record (whose failure markers persist across host restarts) and warns once per new
 * failure, with the same events, words and fixed codes startAacStoreMaintenance logs.
 * Fixture store and log root only; never the real home or store.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createEmptyUnifiedConfig } from '../../../src/config/unified-config-types';
import { saveUnifiedConfig } from '../../../src/config/unified-config-loader';
import {
  clearRecentLogEntries,
  getRecentLogEntries,
} from '../../../src/services/logging/log-buffer';
import { invalidateLoggingConfigCache } from '../../../src/services/logging/log-config';
import { AacStore, startAacStoreHostJobsWatch } from '../../../src/store/store-service';
import type { NativeStoreHandle } from '../../../src/store/native-loader';

const ORIGINAL_CCS_HOME = process.env.CCS_HOME;
let root = '';

beforeEach(() => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-host-jobs-'));
  process.env.CCS_HOME = root;
  clearRecentLogEntries();
  invalidateLoggingConfigCache();
  const config = createEmptyUnifiedConfig();
  config.logging = { ...config.logging, enabled: true, level: 'debug', redact: false };
  saveUnifiedConfig(config);
  invalidateLoggingConfigCache();
});

afterEach(() => {
  if (ORIGINAL_CCS_HOME === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = ORIGINAL_CCS_HOME;
  clearRecentLogEntries();
  invalidateLoggingConfigCache();
  fs.rmSync(root, { recursive: true, force: true });
});

/** The host's job record as the Rust host answers it; markers advance as passes fail. */
function scriptedHost() {
  const state = {
    passes: 1,
    lastPassMs: 1_000,
    lastError: 'io: a private path that must never be logged',
    lastErrorCode: 'io' as string | null,
    lastErrorMs: 1_000 as number | null,
    lastErrorSeq: 1 as number | null,
    lastShareFailedMs: 2_000 as number | null,
    lastShareSeq: 1 as number | null,
  };
  let down = false;
  const handle: NativeStoreHandle = {
    async call(method) {
      if (method !== 'hostJobs') throw new Error(`unexpected method ${method}`);
      if (down) throw new Error('service_down: the store service is not answering');
      return JSON.stringify({ open: true, bootError: null, jobs: state });
    },
  };
  return {
    store: new AacStore(handle, path.join(root, 'store')),
    state,
    setDown: (value: boolean) => {
      down = value;
    },
  };
}

/** A fake clock: every scheduled callback is collected and fired by the test. */
function manualSchedule() {
  const ticks: Array<() => void> = [];
  const schedule = (callback: () => void, _ms: number) => {
    ticks.push(callback);
    // The watcher only calls unref?.(); a bare token serves the fake clock.
    const token = { unref: () => {} } as ReturnType<typeof setTimeout>;
    return token;
  };
  return { ticks, schedule };
}

const flush = () => {
  const { promise, resolve } = Promise.withResolvers<void>();
  setImmediate(resolve);
  return promise;
};
const warnings = () =>
  getRecentLogEntries().filter((entry) => entry.level === 'warn' && entry.source === 'store');

describe('the hosted store jobs watch', () => {
  it('warns once per new failure with the in-process loop events, words and codes', async () => {
    const host = scriptedHost();
    const clock = manualSchedule();
    const stop = startAacStoreHostJobsWatch({ store: () => host.store, schedule: clock.schedule });
    expect(clock.ticks.length).toBe(1);
    clock.ticks.shift()!();
    await flush();
    const first = warnings();
    expect(first.map((entry) => entry.event)).toEqual([
      'store.maintenance_failed',
      'store.share_backup_failed',
    ]);
    expect(first[0].message).toBe('Store maintenance did not finish');
    expect(first[0].context).toEqual({ code: 'io' });
    expect(first[1].message).toBe('The share copy of the store backup failed');
    expect(JSON.stringify(first)).not.toContain('a private path');

    // The same markers warn no second time.
    clock.ticks.shift()!();
    await flush();
    expect(warnings().length).toBe(2);

    // A later pass fails again: new sequences, one new warning per failure, new fixed code.
    host.state.lastErrorMs = 3_000;
    host.state.lastErrorSeq = 2;
    host.state.lastErrorCode = 'db_unavailable';
    host.state.lastShareFailedMs = 4_000;
    host.state.lastShareSeq = 2;
    clock.ticks.shift()!();
    await flush();
    const all = warnings();
    expect(all.length).toBe(4);
    expect(all[2].context).toEqual({ code: 'db_unavailable' });
    stop();
  });

  it('a persisted failure warns again after a dashboard restart', async () => {
    const host = scriptedHost();
    const first = manualSchedule();
    const stopFirst = startAacStoreHostJobsWatch({
      store: () => host.store,
      schedule: first.schedule,
    });
    first.ticks.shift()!();
    await flush();
    expect(warnings().length).toBe(2);
    stopFirst();
    clearRecentLogEntries();
    // A new process reads the same persisted markers: the failure is visible again.
    const second = manualSchedule();
    const stopSecond = startAacStoreHostJobsWatch({
      store: () => host.store,
      schedule: second.schedule,
    });
    second.ticks.shift()!();
    await flush();
    expect(warnings().map((entry) => entry.event)).toEqual([
      'store.maintenance_failed',
      'store.share_backup_failed',
    ]);
    stopSecond();
  });

  it('a down service warns nothing and the next tick recovers', async () => {
    const host = scriptedHost();
    host.setDown(true);
    const clock = manualSchedule();
    const stop = startAacStoreHostJobsWatch({ store: () => host.store, schedule: clock.schedule });
    clock.ticks.shift()!();
    await flush();
    expect(warnings().length).toBe(0);
    host.setDown(false);
    clock.ticks.shift()!();
    await flush();
    expect(warnings().length).toBe(2);
    stop();
  });

  it('stop ends the polling', async () => {
    const host = scriptedHost();
    const clock = manualSchedule();
    const stop = startAacStoreHostJobsWatch({ store: () => host.store, schedule: clock.schedule });
    stop();
    expect(clock.ticks.length).toBe(1);
    clock.ticks.shift()!();
    await flush();
    expect(warnings().length).toBe(0);
  });

  it('a backward clock still warns for new failures (monotonic sequence)', async () => {
    const host = scriptedHost();
    const clock = manualSchedule();
    const stop = startAacStoreHostJobsWatch({ store: () => host.store, schedule: clock.schedule });
    clock.ticks.shift()!();
    await flush();
    expect(warnings().length).toBe(2);
    // The clock moves back; the next failed pass is stamped earlier but is a new failure.
    host.state.lastErrorMs = 500;
    host.state.lastErrorSeq = 2;
    host.state.lastErrorCode = 'db_unavailable';
    host.state.lastShareFailedMs = 600;
    host.state.lastShareSeq = 2;
    clock.ticks.shift()!();
    await flush();
    const all = warnings();
    expect(all.length).toBe(4);
    expect(all[2].event).toBe('store.maintenance_failed');
    expect(all[2].context).toEqual({ code: 'db_unavailable' });
    expect(all[3].event).toBe('store.share_backup_failed');
    // The same sequence warns no second time, even if the clock moves again.
    host.state.lastErrorMs = 400;
    host.state.lastShareFailedMs = 300;
    clock.ticks.shift()!();
    await flush();
    expect(warnings().length).toBe(4);
    stop();
  });
});
