/**
 * A panel's secondary source (browser-check in Machines, the pm-mail registry in Threads) shows its own
 * failure on the readings it supplied, and a usage reading whose reset passed between samples is history.
 * Real StatusService, fake collectors, a private store.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import path from 'path';
import { AgentReportsReader } from '../../../../src/web-server/status/agent-reports-reader';
import { StatusAnswers } from '../../../../src/web-server/status/answers';
import { PmMailRegistryReader } from '../../../../src/web-server/status/status-collectors';
import { StatusService } from '../../../../src/web-server/status/status-service';
import { tempDirs, threadDoc, writeReport } from './status-test-helpers';

const dirs = tempDirs();
afterEach(() => dirs.cleanup());

const GIB = 1024 ** 3;
const START = Date.parse('2026-11-07T15:00:00.000Z');

function setup(options: { registryDir?: string } = {}) {
  const scratch = dirs.make();
  const reports = path.join(scratch, 'agent-reports');
  const clock = { now: START };
  writeReport(
    reports,
    'vm.report',
    'thread.json',
    threadDoc('vm.report', { writtenAt: new Date(START).toISOString() })
  );
  const reader = new AgentReportsReader({ root: () => reports, now: () => clock.now });
  const answers = new StatusAnswers({
    storeDir: () => path.join(scratch, 'store'),
    reportsRoot: () => reports,
    readCard: (id: string) => reader.readCardNow(id),
    doorbell: async () => ({ ok: true, detail: 'Local stub rang.' }),
    now: () => clock.now,
  });
  const fail = {
    browsers: false,
    registry: false,
    nas1: 'ok' as 'ok' | 'error' | 'absent',
    simlock: 'ok' as 'ok' | 'starting' | 'worker',
  };
  const registryReader = {
    scan: async () => {
      if (fail.registry) throw new Error('The pm-mail registry could not be listed.');
      return {
        changed: true,
        threads: [
          {
            name: 'registry-worker',
            kind: 'fake',
            machine: 'vm',
            about: 'Waiting for review',
            lastSeen: new Date(START).toISOString(),
          },
        ],
      };
    },
  };
  const machine = async () => ({
    cores: 8,
    load1: 1,
    busyPct: 12,
    memAvailableBytes: 16 * GIB,
    memTotalBytes: 32 * GIB,
    disk: { mount: '/', freeBytes: 200 * GIB, totalBytes: 500 * GIB },
    cpuTimes: null,
  });
  const good = {
    caps: { max_browsers: 12, max_recordings: 5 },
    browsers: 6,
    recordings: 1,
    gpu_util: 39,
    mem_gb: 16,
    sessions: [],
  };
  // browser-check's own shape: a host it could not read is `state: { error }` with `free: null`, exit 0.
  const slots = () =>
    JSON.stringify({
      rows: [
        { machine: { name: 'vm', label: 'Ubuntu VM' }, state: good },
        ...(fail.nas1 === 'absent'
          ? []
          : [
              {
                machine: { name: 'nas1', label: 'Nas1' },
                state:
                  fail.nas1 === 'error'
                    ? { error: 'session list failed: exit 1: backend unavailable' }
                    : good,
                free: fail.nas1 === 'error' ? null : { headless: 6, record: 4 },
              },
            ]),
      ],
    });
  const simlock = () =>
    fail.simlock === 'starting'
      ? { daemon: { health: 'starting', mode: 'gateway' }, host: {} }
      : {
          daemon: { health: 'running', mode: 'gateway' },
          capacity: { ios: { running: 1, maxRunning: 2, reserved: 0, overLimit: false } },
          devices: [{ id: 'dev_1', workerId: 'mac', spec: { platform: 'ios', model: 'iPhone' } }],
          leases: [
            { deviceId: 'dev_1', workerId: 'mac', requesterId: 'agent-1', grantedAt: START },
          ],
          workers: [
            {
              id: 'mac',
              label: 'Mac',
              connection: fail.simlock === 'worker' ? 'disconnected' : 'connected',
              drained: false,
              lastSeenAt: START,
            },
          ],
        };
  const service = new StatusService({
    reader,
    answers,
    registryReader: (options.registryDir
      ? new PmMailRegistryReader(() => options.registryDir!)
      : registryReader) as never,
    now: () => clock.now,
    timeZone: () => 'America/New_York',
    notify: () => undefined,
    localMachine: () => 'vm',
    readLocalMachine: machine,
    readRemoteMachine: machine,
    refreshSeconds: () => 60,
    run: async (_file: string, args: string[]) =>
      args[0] === 'slots'
        ? {
            code: fail.browsers ? 1 : 0,
            stdout: fail.browsers ? '' : slots(),
            timedOut: false,
            missing: false,
          }
        : {
            code: 0,
            stdout: JSON.stringify(simlock()),
            timedOut: false,
            missing: false,
          },
  } as never);
  return { service, clock, fail };
}

async function warm(service: StatusService) {
  await service.scanReports();
  await service.scanRegistry();
  await service.collect();
  service.view();
  await service.collect();
}

describe('secondary sources on their panels', () => {
  it('a failing browser-check marks its readings cached on a machine whose own reading is fresh', async () => {
    const { service, clock, fail } = setup();
    await warm(service);
    const before = service.view();
    expect(before.machines.also).toEqual(['browsers']);
    expect(before.machines.rows[0].slotsSource.state).toBe('ok');

    fail.browsers = true;
    clock.now += 60_000;
    await service.collect();
    const view = service.view();
    const browsers = view.sources.find((s) => s.id === 'browsers')!;
    expect(browsers.state).toBe('error');
    expect(view.sources.find((s) => s.id === 'machines')?.state).toBe('ok');
    const vm = view.machines.rows.find((r) => r.id === 'vm')!;
    // The machine's own reading is current; the browser readings are the last good ones, said so.
    expect(vm.state).toBe('ok');
    expect(vm.browsers).toEqual({ running: 6, max: 12 });
    expect(vm.recordings).toEqual({ running: 1, max: 5 });
    expect(vm.gpuPct).toBe(39);
    expect(vm.slotsSource).toEqual({
      id: 'browsers',
      label: 'Browsers and recordings',
      state: 'error',
      asOf: new Date(START).toISOString(),
      ageSeconds: 60,
      detail: browsers.detail,
    });
    expect(vm.slotsSource.detail).toContain('Showing the reading from 10:00 AM.');
  });

  it('a failing pm-mail registry marks its rows while the agent reports stay healthy', async () => {
    const { service, clock, fail } = setup();
    await warm(service);
    const before = service.view();
    expect(before.threads.also).toEqual(['pm-mail']);
    const fresh = before.threads.rows.find((t) => t.name === 'registry-worker')!;
    expect(fresh.registrySource?.state).toBe('ok');
    expect(before.threads.rows.find((t) => t.source === 'report')?.registrySource).toBeNull();

    fail.registry = true;
    clock.now += 60_000;
    await service.scanRegistry();
    await service.scanReports();
    const view = service.view();
    const registry = view.sources.find((s) => s.id === 'pm-mail')!;
    expect(registry.state).toBe('error');
    expect(view.sources.find((s) => s.id === 'agent-reports')?.state).toBe('ok');
    const row = view.threads.rows.find((t) => t.name === 'registry-worker')!;
    expect(row.doing).toBe('Waiting for review');
    expect(row.registrySource).toEqual({
      id: 'pm-mail',
      label: 'pm-mail registry',
      state: 'error',
      asOf: new Date(START).toISOString(),
      ageSeconds: 60,
      detail: registry.detail,
    });
  });
});

describe('partial failures inside a secondary source', () => {
  function registryFile(dir: string, name: string, about: string, at: number) {
    fs.writeFileSync(
      path.join(dir, `${name}.json`),
      JSON.stringify({
        name,
        machine: 'vm',
        kind: 'fake',
        about,
        last_seen: new Date(at).toISOString(),
      })
    );
  }

  for (const fault of ['EACCES', 'JSON'] as const) {
    it(`a registry file that fails (${fault}) degrades the registry and marks its row cached`, async () => {
      const registry = dirs.make();
      const file = path.join(registry, 'registry-worker.json');
      registryFile(registry, 'registry-worker', 'Waiting for review', START);
      registryFile(registry, 'other-worker', 'Healthy neighbour', START);
      const { service, clock } = setup({ registryDir: registry });
      await warm(service);
      const before = service.view();
      expect(before.sources.find((s) => s.id === 'pm-mail')?.state).toBe('ok');
      expect(before.threads.rows.find((t) => t.name === 'registry-worker')?.problem).toBeNull();

      clock.now += 60_000;
      if (fault === 'EACCES') {
        registryFile(registry, 'registry-worker', 'New state hidden by the failed read', clock.now);
        fs.chmodSync(file, 0);
      } else fs.writeFileSync(file, '{"broken":');
      registryFile(registry, 'other-worker', 'Healthy neighbour, read again', clock.now);
      try {
        await service.scanRegistry();
        await service.scanReports();
        const view = service.view();
        // The folder listing and the agent reports both succeeded; only one file failed.
        expect(view.sources.find((s) => s.id === 'agent-reports')?.state).toBe('ok');
        const source = view.sources.find((s) => s.id === 'pm-mail')!;
        expect(source.state).toBe('unreadable');
        expect(source.detail).toContain('registry-worker.json');
        expect(source.detail).toContain(
          fault === 'EACCES' ? 'could not be read' : 'not valid JSON'
        );
        expect(source.detail).toContain('since 10:01 AM');
        const row = view.threads.rows.find((t) => t.name === 'registry-worker')!;
        expect(row.doing).toBe('Waiting for review');
        expect(row.stale).toBe(true);
        expect(row.problem).toContain('registry-worker.json');
        expect(row.problem).toContain('Showing its reading from 10:00 AM.');
        expect(row.registrySource).toMatchObject({
          id: 'pm-mail',
          state: 'unreadable',
          asOf: new Date(START).toISOString(),
          ageSeconds: 60,
        });
        // The healthy file's row is current, read at this scan.
        const other = view.threads.rows.find((t) => t.name === 'other-worker')!;
        expect(other.doing).toBe('Healthy neighbour, read again');
        expect(other.problem).toBeNull();
        expect(other.registrySource).toMatchObject({
          state: 'ok',
          asOf: new Date(clock.now).toISOString(),
          ageSeconds: 0,
        });
        expect(view.headline.counts.staleSources).toBe(before.headline.counts.staleSources + 1);
      } finally {
        fs.chmodSync(file, 0o600);
      }

      // Recovery: the next good read clears the problem.
      clock.now += 30_000;
      registryFile(registry, 'registry-worker', 'Back again', clock.now);
      await service.scanRegistry();
      const after = service.view();
      expect(after.sources.find((s) => s.id === 'pm-mail')?.state).toBe('ok');
      const row = after.threads.rows.find((t) => t.name === 'registry-worker')!;
      expect(row.doing).toBe('Back again');
      expect(row.problem).toBeNull();
      expect(row.stale).toBe(false);
      expect(row.registrySource?.state).toBe('ok');
    });
  }

  it('a registry file that never read well names its problem without inventing a row', async () => {
    const registry = dirs.make();
    fs.writeFileSync(path.join(registry, 'newcomer.json'), '{');
    const { service } = setup({ registryDir: registry });
    await service.scanRegistry();
    const view = service.view();
    const source = view.sources.find((s) => s.id === 'pm-mail')!;
    expect(source.state).toBe('unreadable');
    expect(source.detail).toContain('newcomer.json');
    expect(view.threads.rows.some((t) => t.name === 'newcomer')).toBe(false);
  });

  it('a zero-exit browser-check with one failed host names it and keeps its counts cached', async () => {
    const { service, clock, fail } = setup();
    await warm(service);
    const before = service.view();
    expect(before.machines.rows.find((r) => r.id === 'nas1')?.browsers).toEqual({
      running: 6,
      max: 12,
    });

    fail.nas1 = 'error';
    clock.now += 60_000;
    await service.collect();
    const view = service.view();
    const browsers = view.sources.find((s) => s.id === 'browsers')!;
    expect(browsers.state).toBe('error');
    expect(browsers.detail).toContain('Nas1');
    expect(browsers.detail).toContain('backend unavailable');
    expect(browsers.detail).toContain('since 10:01 AM');
    // CPU and memory are their own reading and stay current.
    expect(view.sources.find((s) => s.id === 'machines')?.state).toBe('ok');
    const nas1 = view.machines.rows.find((r) => r.id === 'nas1')!;
    expect(nas1.state).toBe('ok');
    expect(nas1.cpu?.busyPct).toBe(12);
    // The last good counts stay, labelled with their own age and the host's problem.
    expect(nas1.browsers).toEqual({ running: 6, max: 12 });
    expect(nas1.recordings).toEqual({ running: 1, max: 5 });
    expect(nas1.slotsSource).toMatchObject({
      id: 'browsers',
      state: 'error',
      asOf: new Date(START).toISOString(),
      ageSeconds: 60,
    });
    expect(nas1.slotsSource.detail).toContain('backend unavailable');
    expect(nas1.slotsSource.detail).toContain('Showing its reading from 10:00 AM.');
    // The healthy host is current.
    const vm = view.machines.rows.find((r) => r.id === 'vm')!;
    expect(vm.slotsSource).toMatchObject({
      state: 'ok',
      asOf: new Date(clock.now).toISOString(),
      ageSeconds: 0,
    });

    fail.nas1 = 'ok';
    clock.now += 60_000;
    await service.collect();
    const healed = service.view();
    expect(healed.sources.find((s) => s.id === 'browsers')?.state).toBe('ok');
    expect(healed.machines.rows.find((r) => r.id === 'nas1')?.slotsSource.state).toBe('ok');
  });

  it('a host that failed before its first good reading shows unknown counts with the reason', async () => {
    const { service, fail } = setup();
    fail.nas1 = 'error';
    await warm(service);
    const view = service.view();
    const nas1 = view.machines.rows.find((r) => r.id === 'nas1')!;
    expect(nas1.browsers).toEqual({ running: null, max: null });
    expect(nas1.recordings).toEqual({ running: null, max: null });
    expect(nas1.gpuPct).toBeNull();
    expect(nas1.slotsSource).toMatchObject({ state: 'error', asOf: null, ageSeconds: null });
    expect(nas1.slotsSource.detail).toContain('backend unavailable');
    expect(nas1.slotsSource.detail).toContain('No reading yet.');
  });

  it('a host browser-check does not list is not shown as a current reading', async () => {
    const { service, fail } = setup();
    fail.nas1 = 'absent';
    await warm(service);
    const view = service.view();
    expect(view.sources.find((s) => s.id === 'browsers')?.state).toBe('ok');
    const nas1 = view.machines.rows.find((r) => r.id === 'nas1')!;
    expect(nas1.browsers).toBeNull();
    expect(nas1.slotsSource).toMatchObject({ state: 'missing', asOf: null, ageSeconds: null });
    expect(view.machines.rows.find((r) => r.id === 'vm')?.slotsSource.state).toBe('ok');
  });
  it('a Simlock that is starting keeps its last reading cached instead of an empty fleet', async () => {
    const { service, clock, fail } = setup();
    await warm(service);
    expect(service.view().simlock.leases).toHaveLength(1);
    fail.simlock = 'starting';
    clock.now += 60_000;
    await service.collect();
    const view = service.view();
    const source = view.sources.find((s) => s.id === 'simlock')!;
    expect(source.state).toBe('error');
    expect(source.detail).toContain('starting');
    expect(source.detail).toContain('Showing the reading from 10:00 AM.');
    expect(view.simlock.leases).toHaveLength(1);
    expect(view.simlock.capacity).toHaveLength(1);
  });

  it('a disconnected Simlock worker names itself and its leases read as last known', async () => {
    const { service, clock, fail } = setup();
    fail.simlock = 'worker';
    clock.now += 60_000;
    await warm(service);
    const view = service.view();
    const source = view.sources.find((s) => s.id === 'simlock')!;
    expect(source.state).toBe('error');
    expect(source.detail).toContain('Mac: disconnected');
    expect(source.detail).toContain('since 10:00 AM');
    expect(view.simlock.leases[0].lastKnown).toBe(true);
  });
});

function codexAccount(sampledAt: number, usedPercent: number, resetAt: number) {
  return {
    id: 'codex:review',
    provider: 'codex',
    providerLabel: 'Codex',
    label: 'Review fixture',
    email: null,
    plan: 'Pro',
    platform: 'ubuntu',
    source: 'Codex usage on Ubuntu',
    status: 'ok',
    message: null,
    fetchedAt: new Date(sampledAt).toISOString(),
    sampledAt: new Date(sampledAt).toISOString(),
    isActive: true,
    capabilities: { codexProfile: 'review', claudeProfileId: null, claudePlatforms: [] },
    windows: [
      {
        key: 'five_hour',
        label: '5-hour',
        kind: 'rate_limit',
        usedPercent,
        remainingPercent: 100 - usedPercent,
        resetAt: new Date(resetAt).toISOString(),
        windowMinutes: 300,
        used: null,
        limit: null,
        unit: null,
      },
    ],
  };
}

describe('usage resets between samples', () => {
  it('a reading whose reset passes before the next sample becomes history, its number kept', async () => {
    const { service, clock } = setup();
    const reset = START + 30_000;
    service.recordUsage({
      accounts: [codexAccount(START, 100, reset)],
      history: null,
      sampledAt: START,
    } as never);
    const before = service.view();
    expect(before.usage.accounts[0]).toMatchObject({
      usedPct: 100,
      atLimit: true,
      stale: false,
      resetPassed: false,
    });
    expect(before.usage.warnings).toHaveLength(1);

    clock.now = START + 60_000;
    const view = service.view();
    expect(view.sources.find((s) => s.id === 'usage')?.state).toBe('ok');
    const row = view.usage.accounts[0];
    expect(row.usedPct).toBe(100);
    expect(row.resetsAt).toBe(new Date(reset).toISOString());
    expect(row.resetPassed).toBe(true);
    expect(row.stale).toBe(true);
    expect(row.atLimit).toBe(false);
    expect(row.projectedFullAt).toBeNull();
    expect(row.runsOutBeforeReset).toBeNull();
    expect(view.usage.warnings).toEqual([]);
  });

  it('a reading taken after the reset is current', async () => {
    const { service, clock } = setup();
    const reset = START + 30_000;
    clock.now = START + 60_000;
    service.recordUsage({
      accounts: [codexAccount(clock.now, 100, reset)],
      history: null,
      sampledAt: clock.now,
    } as never);
    clock.now += 30_000;
    const row = service.view().usage.accounts[0];
    expect(row).toMatchObject({ usedPct: 100, atLimit: true, stale: false, resetPassed: false });

    const next = clock.now;
    service.recordUsage({
      accounts: [codexAccount(next, 4, next + 5 * 3_600_000)],
      history: null,
      sampledAt: next,
    } as never);
    const fresh = service.view();
    expect(fresh.usage.accounts[0]).toMatchObject({
      usedPct: 4,
      atLimit: false,
      stale: false,
      resetPassed: false,
    });
    expect(fresh.usage.warnings).toEqual([]);
  });
});
