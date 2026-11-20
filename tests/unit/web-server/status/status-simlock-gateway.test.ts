/**
 * Status > Phones reads the fleet gateway (`simlock status --json --gateway`), and only when that does not
 * answer does this computer's worker stand in, named on the panel. Real StatusService, a fake command runner.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import path from 'path';
import { AgentReportsReader } from '../../../../src/web-server/status/agent-reports-reader';
import { StatusAnswers } from '../../../../src/web-server/status/answers';
import type { CommandResult } from '../../../../src/web-server/status/status-collectors';
import { StatusService } from '../../../../src/web-server/status/status-service';
import { tempDirs, threadDoc, writeReport } from './status-test-helpers';

const dirs = tempDirs();
afterEach(() => dirs.cleanup());

const START = Date.parse('2026-11-07T15:00:00.000Z');

const GATEWAY = {
  daemon: { health: 'running', mode: 'gateway' },
  capacity: {
    global: { running: 5, maxRunning: 5, reserved: 0, overLimit: false },
    ios: { running: 4, maxRunning: 4, reserved: 0, overLimit: false, limit: 5 },
    android: { running: 3, maxRunning: 3, reserved: 0, overLimit: false },
  },
  devices: [],
  leases: [],
  workers: [
    { id: 'nas1', label: 'Nas1', connection: 'connected', lastSeenAt: START },
    { id: 'vm', label: 'VM', connection: 'connected', lastSeenAt: START },
    { id: 'mac', label: 'Mac', connection: 'connected', lastSeenAt: START },
  ],
};

const LOCAL = {
  daemon: { health: 'running', mode: 'worker' },
  capacity: {
    ios: { running: 1, maxRunning: 1, reserved: 0, overLimit: false },
    android: { running: 1, maxRunning: 1, reserved: 0, overLimit: false },
    global: { running: 1, maxRunning: 1, reserved: 0, overLimit: false },
  },
  devices: [],
  leases: [],
  host: {},
};

function answer(stdout: unknown): CommandResult {
  return { code: 0, stdout: JSON.stringify(stdout), timedOut: false, missing: false };
}

function refused(): CommandResult {
  return { code: 2, stdout: '', timedOut: false, missing: false };
}

function setup(reply: (args: string[]) => CommandResult) {
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
  const calls: string[][] = [];
  const service = new StatusService({
    reader,
    answers,
    now: () => clock.now,
    timeZone: () => 'America/New_York',
    notify: () => undefined,
    localMachine: () => 'vm',
    refreshSeconds: () => 60,
    run: async (_file: string, args: string[]) => {
      calls.push(args);
      return reply(args);
    },
  } as never);
  return { service, calls };
}

describe('Status > Phones reads Simlock', () => {
  it('asks the fleet gateway first, and draws the overall cap after the platforms', async () => {
    const { service, calls } = setup(() => answer(GATEWAY));
    await service.collectSimlock();
    expect(calls).toEqual([['status', '--json', '--gateway']]);
    const view = service.view();
    expect(view.simlock.capacity.map((c) => [c.platform, c.label, c.overall])).toEqual([
      ['ios', 'iPhone', false],
      ['android', 'Android', false],
      ['global', 'All phones at once', true],
    ]);
    expect(view.simlock.capacity.at(-1)).toMatchObject({ platform: 'global', running: 5, max: 5 });
    expect(view.sources.find((s) => s.id === 'simlock')?.state).toBe('ok');
  });

  it('falls back to this computer worker, and says so, when the gateway does not answer', async () => {
    const { service, calls } = setup((args) =>
      args.includes('--gateway') ? refused() : answer(LOCAL)
    );
    await service.collectSimlock();
    expect(calls).toEqual([
      ['status', '--json', '--gateway'],
      ['status', '--json'],
    ]);
    const view = service.view();
    const source = view.sources.find((s) => s.id === 'simlock');
    expect(source?.state).toBe('error');
    expect(source?.detail).toContain(
      "Read this computer's Simlock worker only; the fleet gateway did not answer"
    );
    // The local worker's own counts, never the gateway's.
    expect(view.simlock.capacity.map((c) => [c.platform, c.running, c.max])).toEqual([
      ['ios', 1, 1],
      ['android', 1, 1],
      ['global', 1, 1],
    ]);
  });

  it('shows a failed read, with no counts, when neither the gateway nor the worker answers', async () => {
    const { service, calls } = setup(() => refused());
    await service.collectSimlock();
    expect(calls).toEqual([
      ['status', '--json', '--gateway'],
      ['status', '--json'],
    ]);
    const view = service.view();
    expect(view.simlock.capacity).toEqual([]);
    expect(view.sources.find((s) => s.id === 'simlock')?.state).toBe('error');
  });
});

describe('Status > Browsers knows which machines run a Simlock worker', () => {
  const slot = (name: string, hostnames: string[]) => ({
    machine: { name, label: name, hostnames },
    state: { caps: { max_browsers: 2 }, browsers: 0, recordings: 0, sessions: [], phones: null },
  });
  const SLOTS = { rows: [slot('odroid', ['Odroid']), slot('gtx1070ti', ['1070ti'])] };
  const WITH_1070TI = {
    ...GATEWAY,
    workers: [
      ...GATEWAY.workers,
      { id: 'w4', label: '1070ti', connection: 'connected', lastSeenAt: START },
    ],
  };
  const browserRows = (service: StatusService) =>
    (service as unknown as { browsers: { data: { machine: string; notes?: string[] }[] } }).browsers
      .data;

  it("takes the gateway's workers: the 1070ti's missing phone count is a problem, Odroid's is not", async () => {
    const { service } = setup((args) =>
      args[0] === 'slots' ? answer(SLOTS) : answer(WITH_1070TI)
    );
    await service.collectSimlock();
    await service.collectBrowsers();
    const rows = Object.fromEntries(browserRows(service).map((r) => [r.machine, r.notes]));
    expect(Object.keys(rows)).toEqual(['odroid', '1070ti']);
    expect(rows.odroid).toEqual([]);
    expect(rows['1070ti']).toEqual([
      'reported no phone count (phones: null), so phones here are unknown',
    ]);
  });

  it('keeps every non-Windows computer as a possible worker until the gateway answers', async () => {
    const { service } = setup((args) => (args[0] === 'slots' ? answer(SLOTS) : refused()));
    await service.collectSimlock();
    await service.collectBrowsers();
    const rows = Object.fromEntries(browserRows(service).map((r) => [r.machine, r.notes]));
    expect(rows.odroid).toHaveLength(1);
    expect(rows['1070ti']).toHaveLength(1);
  });
});
