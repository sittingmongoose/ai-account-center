import { afterEach, describe, expect, it, spyOn } from 'bun:test';
import fs from 'fs';
import path from 'path';
import {
  busyBetween,
  machineProbeCommand,
  parseBrowserSlots,
  parseMachineProbe,
  parseRegistryEntry,
  parseSimlockStatus,
  PmMailRegistryReader,
  readLocalMachine,
  readRemoteMachine,
  registryMachine,
  sessionUrl,
  type CommandRunner,
} from '../../../../src/web-server/status/status-collectors';
import { PERSISTENT_SHARE_ERROR } from '../../../../src/web-server/status/agent-reports-reader';
import { tempDirs } from './status-test-helpers';

const dirs = tempDirs();
afterEach(() => dirs.cleanup());

describe('status collectors', () => {
  it('reads browser-check phones, display_problem and snapshot without turning null into zero', () => {
    const state = { caps: { max_browsers: 4 }, browsers: 1, recordings: 0, sessions: [] };
    const rows = parseBrowserSlots({
      rows: [
        {
          machine: { name: 'mac', label: 'MacBook' },
          state: {
            ...state,
            phones: { ios: { running: 0, max: 2 }, android: { running: 1, max: null } },
          },
        },
        {
          machine: { name: 'vm', label: 'Ubuntu VM' },
          state: { ...state, phones: null, display_problem: 'X desktop :0 refuses this account' },
        },
        {
          machine: { name: 'nas1', label: 'Nas1' },
          state: { ...state, snapshot: 'backend busy (x): counted from its session records' },
        },
        { machine: { name: 'windows', label: 'Windows PC' }, state },
      ],
    });
    const by = Object.fromEntries(rows.map((row) => [row.machine, row]));
    expect(by.mac.phones).toEqual({
      ios: { running: 0, max: 2 },
      android: { running: 1, max: null },
    });
    expect(by.mac.notes).toEqual([]);
    expect(by.vm.phones).toBeNull();
    expect(by.vm.notes).toEqual([
      'browsers cannot start here: X desktop :0 refuses this account',
      'reported no phone count (phones: null), so phones here are unknown',
    ]);
    expect(by.nas1.notes).toEqual(['backend busy (x): counted from its session records']);
    // A field browser-check left out is not a null report.
    expect(by.windows.phones).toBeUndefined();
    expect(by.windows.notes).toEqual([]);
  });

  it('shows browser sessions as host and path only, and keeps caps per machine', () => {
    const rows = parseBrowserSlots({
      rows: [
        {
          machine: { name: 'vm', label: 'Ubuntu VM' },
          state: {
            caps: { max_browsers: 12, max_recordings: 5 },
            browsers: 2,
            recordings: 1,
            gpu_util: 39,
            mem_gb: 15.44,
            sessions: [
              {
                id: 'abc123',
                url: 'http://10.0.0.5:8963/head.html?cb=1&token=secret#x',
                owner: 'pid 5',
                recording: true,
                age_s: 466,
              },
              {
                id: 'def456',
                url: 'file:///tmp/run/page%20one.html?x=1',
                owner: 'thread:abc',
                age_s: 10,
              },
            ],
          },
        },
        { machine: { name: 'laptop' }, state: {} },
      ],
    });
    expect(rows).toHaveLength(1);
    expect(rows[0].browsers).toEqual({ running: 2, max: 12 });
    expect(rows[0].recordings).toEqual({ running: 1, max: 5 });
    expect(rows[0].memGb).toBe(15.4);
    expect(rows[0].sessions.map((s) => s.url)).toEqual([
      '10.0.0.5:8963/head.html',
      '/tmp/run/page one.html',
    ]);
    expect(sessionUrl('javascript:alert(1)')).toBeNull();
    expect(() => parseBrowserSlots({})).toThrow('unknown shape');
  });

  it('reads Simlock capacity and leases without lease ids or tokens', () => {
    const status = parseSimlockStatus({
      devices: [{ id: 'dev_1', spec: { platform: 'ios', model: 'iPhone Duo' } }],
      leases: [
        {
          id: 'lease-secret-id',
          deviceId: 'dev_1',
          requesterId: 'agent-7',
          ownerId: 'owner-x',
          grantedAt: Date.parse('2026-11-06T16:00:00Z'),
          ttlDeadline: Date.parse('2026-11-06T17:00:00Z'),
          token: 'abc',
        },
      ],
      capacity: {
        ios: { running: 1, maxRunning: 1, reserved: 0, overLimit: false },
        android: { running: 0, maxRunning: 1, reserved: 1, overLimit: true },
      },
    });
    expect(status.capacity).toEqual([
      { platform: 'ios', running: 1, max: 1, reserved: 0, overLimit: false },
      { platform: 'android', running: 0, max: 1, reserved: 1, overLimit: true },
    ]);
    expect(status.leases).toEqual([
      {
        device: 'dev_1',
        deviceLabel: 'iPhone Duo',
        platform: 'ios',
        holder: 'agent-7',
        since: '2026-11-06T16:00:00.000Z',
        expiresAt: '2026-11-06T17:00:00.000Z',
        lastKnown: false,
      },
    ]);
    expect(status.problems).toEqual([]);
    expect(JSON.stringify(status)).not.toContain('lease-secret-id');
  });

  it('keeps a host browser-check could not read as an error with unknown counts', () => {
    const rows = parseBrowserSlots({
      rows: [
        {
          machine: { name: 'nas1', label: 'Nas1' },
          state: { error: 'session list failed: exit 1: backend unavailable' },
          free: null,
        },
        {
          machine: { name: 'mac', label: 'Mac' },
          state: { error: 'token=sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789' },
        },
      ],
    });
    expect(rows[0]).toMatchObject({
      machine: 'nas1',
      browsers: { running: null, max: null },
      recordings: { running: null, max: null },
      gpuPct: null,
      sessions: [],
      error: 'session list failed: exit 1: backend unavailable',
    });
    // A tool text that looks like a secret is never shown; the failure still is.
    expect(rows[1].error).toBe('browser-check could not read this machine');
  });

  it('refuses a Simlock answer that has not read its devices yet, or whose daemon failed', () => {
    expect(() =>
      parseSimlockStatus({ daemon: { health: 'starting', mode: 'worker' }, host: {} })
    ).toThrow('starting');
    expect(() =>
      parseSimlockStatus({
        daemon: { health: 'failed', mode: 'worker' },
        capacity: {},
        devices: [],
        leases: [],
      })
    ).toThrow('failed');
    expect(() => parseSimlockStatus({ daemon: { health: 'running' }, host: {} })).toThrow(
      'without'
    );
  });

  it('names a disconnected Simlock worker and marks its leases as last known', () => {
    const status = parseSimlockStatus({
      daemon: { health: 'running', mode: 'gateway' },
      capacity: { ios: { running: 0, maxRunning: 1, reserved: 0, overLimit: false } },
      devices: [
        { id: 'dev_1', workerId: 'mac', spec: { platform: 'ios', model: 'iPhone Duo' } },
        { id: 'dev_2', workerId: 'vm', spec: { platform: 'android', model: 'Pixel' } },
      ],
      leases: [
        { deviceId: 'dev_1', workerId: 'mac', requesterId: 'agent-1', grantedAt: 1 },
        { deviceId: 'dev_2', workerId: 'vm', requesterId: 'agent-2', grantedAt: 1 },
      ],
      workers: [
        {
          id: 'mac',
          label: 'Mac',
          connection: 'disconnected',
          drained: false,
          lastSeenAt: Date.parse('2026-11-06T16:00:00Z'),
        },
        { id: 'vm', label: 'VM', connection: 'connected', drained: false, lastSeenAt: 2 },
      ],
    });
    expect(status.problems).toEqual([
      {
        item: 'Mac',
        reason: 'disconnected; its devices and leases are the last known',
        lastSeenAt: '2026-11-06T16:00:00.000Z',
      },
    ]);
    expect(status.leases.map((l) => [l.device, l.lastKnown])).toEqual([
      ['dev_1', true],
      ['dev_2', false],
    ]);
  });

  it('parses each host probe and works out the busy percent between readings', () => {
    const linux = parseMachineProbe(
      'linux',
      'cores=6\nload=7.48\nmemTotalKb=20210000\nmemAvailKb=13245000\ncpu=100 0 50 800 50 0 0 0\ndiskTotalKb=812506724\ndiskFreeKb=528836920\nmount=/\n'
    );
    expect(linux?.cores).toBe(6);
    expect(linux?.memAvailableBytes).toBe(13245000 * 1024);
    expect(linux?.cpuTimes).toEqual({ busy: 150, total: 1000 });
    expect(busyBetween({ busy: 150, total: 1000 }, { busy: 250, total: 1200 })).toBe(50);
    const mac = parseMachineProbe(
      'mac',
      'cores=8\nload=2.84\nmemTotalB=17179869184\npageSize=16384\npagesAvail=100000\npagesAvail=50000\ncpuPct=32\ndiskTotalKb=482766932\ndiskFreeKb=18378524\nmount=/System/Volumes/Data\n'
    );
    expect(mac?.memAvailableBytes).toBe(150000 * 16384);
    expect(mac?.busyPct).toBe(4);
    expect(mac?.disk?.mount).toBe('/System/Volumes/Data');
    const windows = parseMachineProbe(
      'windows',
      'cores=32\r\ncpuPct=40\r\nmemTotalKb=33463884\r\nmemAvailKb=10291080\r\ndiskTotalB=1999342923776\r\ndiskFreeB=218592583680\r\nmount=C:\r\n'
    );
    expect(windows?.load1).toBeNull();
    expect(windows?.busyPct).toBe(40);
    expect(windows?.disk).toEqual({
      mount: 'C:',
      freeBytes: 218592583680,
      totalBytes: 1999342923776,
    });
    expect(parseMachineProbe('linux', 'garbage')).toBeNull();
    // An answer without its disk lines is a partial reading: named, never "no disk".
    const noDisk = parseMachineProbe(
      'linux',
      'cores=6\nmemTotalKb=20210000\nmemAvailKb=13245000\n'
    );
    expect(noDisk?.disk).toBeNull();
    expect(noDisk?.diskError).toBe('The machine did not report its system disk.');
    expect(linux?.diskError).toBeUndefined();
    expect(machineProbeCommand('windows')).toStartWith(
      'powershell.exe -NoProfile -NonInteractive -EncodedCommand '
    );
  });

  it('reads a remote machine with one ssh command and reports a silent host in fixed words', async () => {
    const calls: string[][] = [];
    const run: CommandRunner = async (file, args) => {
      calls.push([file, ...args]);
      return {
        code: 255,
        stdout: 'Permission denied for user secret',
        timedOut: false,
        missing: false,
      };
    };
    await expect(readRemoteMachine('nas1', run)).rejects.toThrow('ssh failed.');
    expect(calls[0][0]).toBe('ssh');
    expect(calls[0]).toContain('BatchMode=yes');
    expect(calls[0]).toContain('nas1-agent');
  });

  it('maps registry entries onto machines and reads the folder without writing', async () => {
    expect(registryMachine('ubuntuserver', '')).toBe('vm');
    expect(registryMachine('ubuntu-agent-nas1', '')).toBe('nas1');
    expect(registryMachine('Someones-MacBook-Air.local', '')).toBe('mac');
    expect(registryMachine('GamingPC', '')).toBe('windows');
    expect(registryMachine(null, '/Users/x/work')).toBe('mac');
    expect(registryMachine(null, 'C:\\Users\\x')).toBe('windows');
    expect(parseRegistryEntry({ name: '../bad' })).toBeNull();
    expect(parseRegistryEntry({ name: 'ok', about: 'mail me at a@b.example' })?.about).toBeNull();
    const dir = dirs.make();
    fs.writeFileSync(
      path.join(dir, 'lead.json'),
      JSON.stringify({
        name: 'lead',
        kind: 'claude',
        about: 'Working',
        last_seen: '2026-11-06T16:00:00Z',
        machine: 'ubuntuserver',
      })
    );
    fs.writeFileSync(path.join(dir, 'broken.json'), '{');
    const reader = new PmMailRegistryReader(() => dir);
    const first = await reader.scan(1_000);
    expect(first.threads).toEqual([
      {
        name: 'lead',
        kind: 'claude',
        machine: 'vm',
        about: 'Working',
        lastSeen: '2026-11-06T16:00:00.000Z',
        readAt: 1_000,
        problem: null,
      },
    ]);
    // The broken file is named, not silently dropped, and tried again each scan.
    expect(first.problems).toEqual([
      { item: 'broken.json', reason: 'is not valid JSON', since: 1_000 },
    ]);
    const second = await reader.scan(2_000);
    expect(second.changed).toBe(false);
    expect(second.problems[0].since).toBe(1_000);
    expect(fs.readdirSync(dir).sort()).toEqual(['broken.json', 'lead.json']);
    await expect(new PmMailRegistryReader(() => path.join(dir, 'nope')).scan()).rejects.toThrow(
      'missing'
    );
  });

  it('a local disk read that fails leaves the rest of the reading and names the disk', async () => {
    const sample = await readLocalMachine({
      statfs: async () => {
        throw Object.assign(new Error('EIO'), { code: 'EIO' });
      },
    });
    expect(sample.disk).toBeNull();
    expect(sample.diskError).toBe('The system disk could not be read.');
    expect(sample.memTotalBytes).toBeGreaterThan(0);
    const good = await readLocalMachine();
    expect(good.disk?.mount).toBe('/');
    expect(good.diskError).toBeUndefined();
  });

  it('keeps a registry row through one ENOENT race and names a listed file gone twice in a row', async () => {
    const dir = dirs.make();
    const file = path.join(dir, 'lead.json');
    fs.writeFileSync(file, JSON.stringify({ name: 'lead', about: 'Working' }));
    const reader = new PmMailRegistryReader(() => dir);
    expect((await reader.scan(1_000)).threads).toHaveLength(1);
    const realStat = fs.promises.stat;
    const stat = spyOn(fs.promises, 'stat').mockImplementation((async (
      target: fs.PathLike,
      ...rest: unknown[]
    ) => {
      if (String(target) === file) throw Object.assign(new Error('gone'), { code: 'ENOENT' });
      return (realStat as (...args: unknown[]) => Promise<fs.Stats>)(target, ...rest);
    }) as typeof fs.promises.stat);
    try {
      // Listed, then gone at the stat: pm-mail's rename-over. The row stays, nothing is said.
      let scan = await reader.scan(2_000);
      expect(scan.threads.map((thread) => [thread.name, thread.readAt, thread.problem])).toEqual([
        ['lead', 1_000, null],
      ]);
      expect(scan.problems).toEqual([]);
      // Listed and gone again: a named problem since the first failure, on the row too.
      scan = await reader.scan(3_000);
      expect(scan.threads[0].problem).toEqual({
        item: 'lead.json',
        reason: PERSISTENT_SHARE_ERROR,
        since: 2_000,
      });
      expect(scan.problems).toHaveLength(1);
    } finally {
      stat.mockRestore();
    }
    const scan = await reader.scan(4_000);
    expect(scan.threads[0].problem).toBeNull();
    expect(scan.problems).toEqual([]);
    // Removed for real: gone from the listing, gone from the page.
    fs.rmSync(file);
    expect((await reader.scan(5_000)).threads).toEqual([]);
  });
});
