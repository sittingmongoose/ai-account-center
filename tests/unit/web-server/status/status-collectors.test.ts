import { afterEach, describe, expect, it, spyOn } from 'bun:test';
import { execFileSync } from 'child_process';
import fs from 'fs';
import path from 'path';
import {
  browserMachine,
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
  simlockWorkerMachines,
  type CommandRunner,
} from '../../../../src/web-server/status/status-collectors';
import { PERSISTENT_SHARE_ERROR } from '../../../../src/web-server/status/agent-reports-reader';
import { machineWarnings } from '../../../../src/web-server/status/status-view-panels';
import { tempDirs } from './status-test-helpers';

const dirs = tempDirs();
afterEach(() => dirs.cleanup());

describe('status collectors', () => {
  it('names browser-check rows by the registry: gtx1070ti is the 1070ti, Odroid by name', () => {
    expect(browserMachine({ name: 'odroid', hostnames: ['Odroid'] })).toBe('odroid');
    expect(browserMachine({ name: 'gtx1070ti', hostnames: ['1070ti'] })).toBe('1070ti');
    expect(browserMachine({ name: 'vm', hostnames: ['ubuntuserver'] })).toBe('vm');
    expect(browserMachine({ name: 'nas1', hostnames: ['ubuntu-agent-nas1'] })).toBe('nas1');
    // Never a partial match, and an unregistered machine is left out.
    expect(browserMachine({ name: 'gtx1070', hostnames: ['gtx1070'] })).toBeNull();
    expect(browserMachine({ name: 'future', hostnames: ['future'] })).toBeNull();
    const rows = parseBrowserSlots({
      rows: [
        {
          machine: { name: 'odroid', label: 'Odroid H2+', hostnames: ['Odroid'] },
          state: { error: 'no backend' },
        },
        {
          machine: { name: 'gtx1070ti', label: '1070ti (GTX 1070 Ti)', hostnames: ['1070ti'] },
          state: { error: 'no backend' },
        },
        { machine: { name: '1070ti', label: 'again' }, state: { error: 'twice' } },
        { machine: { name: 'future', label: 'Future' }, state: { error: 'no backend' } },
      ],
    });
    expect(rows.map((r) => [r.machine, r.error])).toEqual([
      ['odroid', 'no backend'],
      ['1070ti', 'no backend'],
    ]);
  });

  it("reads the Simlock gateway's workers as machines, connected or not", () => {
    expect(
      simlockWorkerMachines({
        workers: [
          { label: 'nas1', connection: 'connected' },
          { label: 'vm', connection: 'connected' },
          { label: 'mac', connection: 'disconnected' },
          { label: '1070ti', connection: 'connected' },
          { label: 'somewhere-else', connection: 'connected' },
          { id: 'no-label' },
        ],
      })
    ).toEqual(['nas1', 'vm', 'mac', '1070ti']);
    expect(simlockWorkerMachines({ capacity: {} })).toBeNull();
    expect(simlockWorkerMachines(null)).toBeNull();
  });

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

  it("adds the fleet's phone caps from the workers that can run each platform, never a drained one", () => {
    // The real gateway on 2026-11-20: every worker reports iPhone and Android rows, the gateway summed 6 iPhones.
    const worker = (
      label: string,
      catalog: string,
      ios: number,
      android: number,
      drained = false
    ) => ({
      id: label,
      label,
      connection: 'connected',
      drained,
      catalog: { [catalog]: [] },
      capacity: {
        ios: { running: 0, maxRunning: ios, reserved: 0, overLimit: false, limit: ios },
        android: {
          running: label === 'vm' ? 1 : 0,
          maxRunning: android,
          reserved: 0,
          overLimit: false,
        },
        global: {
          running: label === 'vm' ? 1 : 0,
          maxRunning: Math.max(ios, android),
          reserved: 0,
          overLimit: false,
        },
      },
    });
    const status = parseSimlockStatus({
      daemon: { health: 'running', mode: 'gateway' },
      capacity: {
        ios: { running: 0, maxRunning: 6, reserved: 0, overLimit: false },
        android: { running: 1, maxRunning: 5, reserved: 0, overLimit: false },
        global: { running: 1, maxRunning: 7, reserved: 0, overLimit: false },
      },
      devices: [],
      leases: [],
      workers: [
        worker('nas1', 'android', 1, 1),
        worker('vm', 'android', 1, 1),
        worker('mac', 'ios', 2, 1),
        worker('1070ti', 'android', 2, 2, true),
      ],
    });
    expect(status.capacity).toEqual([
      { platform: 'ios', running: 0, max: 2, reserved: 0, overLimit: false },
      { platform: 'android', running: 1, max: 2, reserved: 0, overLimit: false },
      { platform: 'global', running: 1, max: 4, reserved: 0, overLimit: false },
    ]);
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
      'cores=8\nload=2.84\nmemTotalB=17179869184\nmemAvailPct=67\ncpuPct=32\ndiskTotalKb=482766932\ndiskFreeKb=18378524\nmount=/System/Volumes/Data\n'
    );
    expect(mac?.memAvailableBytes).toBe(11_510_512_353.28);
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
    expect(noDisk?.memoryError).toBeUndefined();
    for (const machine of [linux, mac, windows]) expect(machine?.memoryError).toBeUndefined();
    const noMemory = parseMachineProbe(
      'windows',
      'cores=32\r\ndiskTotalB=100\r\ndiskFreeB=50\r\nmount=C:\r\n'
    );
    expect(noMemory?.memAvailableBytes).toBeNull();
    expect(noMemory?.memoryError).toBe('The machine did not report its memory.');
    expect(noMemory?.diskError).toBeUndefined();
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
    expect(calls[0][0]).toMatch(/(^|[\/])ssh(\.exe)?$/);
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

  it('a local disk or CPU read that fails leaves the rest of the reading and names the part', async () => {
    const eio = () => Object.assign(new Error('EIO'), { code: 'EIO' });
    const statfs = async () => ({ bavail: 10, bsize: 4096, blocks: 100 }) as fs.StatsFs;
    const proc = (fail: string | null) => async (file: string) => {
      if (file === fail) throw eio();
      return file === '/proc/stat'
        ? 'cpu  100 0 50 800 50 0 0 0\n'
        : 'MemTotal: 1000 kB\nMemAvailable: 500 kB\n';
    };
    const noDisk = await readLocalMachine({
      statfs: async () => {
        throw eio();
      },
      readFile: proc(null),
      platform: 'linux',
    });
    expect(noDisk.disk).toBeNull();
    expect(noDisk.diskError).toBe('The system disk could not be read.');
    expect(noDisk.memAvailableBytes).toBe(500 * 1024);
    expect(noDisk.cpuError).toBeUndefined();
    const noCpu = await readLocalMachine({
      statfs,
      readFile: proc('/proc/stat'),
      platform: 'linux',
    });
    expect(noCpu.cpuTimes).toBeNull();
    expect(noCpu.cpuError).toBe('The CPU counters could not be read.');
    expect(noCpu.disk?.mount).toBe('/');
    expect(noCpu.diskError).toBeUndefined();
    // A failed /proc/meminfo keeps the OS figure and the CPU counters.
    const noMeminfo = await readLocalMachine({
      statfs,
      readFile: proc('/proc/meminfo'),
      platform: 'linux',
    });
    expect(noMeminfo.cpuTimes).toEqual({ busy: 150, total: 1000 });
    expect(noMeminfo.cpuError).toBeUndefined();
    // A dashboard hosted on the Mac uses the same available-memory reading.
    const macMemory =
      (stdout: string): CommandRunner =>
      async (file, args) => {
        expect(file).toBe('/bin/sh');
        expect(args[1]).toContain('kern.memorystatus_level');
        return { code: 0, stdout, timedOut: false, missing: false };
      };
    const mac = await readLocalMachine({
      statfs,
      readFile: proc('/proc/stat'),
      platform: 'darwin',
      run: macMemory('memTotalB=17179869184\nmemAvailPct=67\n'),
    });
    expect(mac.cpuError).toBeUndefined();
    expect(mac.memAvailableBytes).toBe(11_510_512_353.28);
    expect(mac.memTotalBytes).toBe(16 * 1024 ** 3);
    const unknownMac = await readLocalMachine({
      statfs,
      platform: 'darwin',
      run: macMemory('memTotalB=17179869184\nmemAvailPct=\n'),
    });
    expect(unknownMac.memAvailableBytes).toBeNull();
    expect(unknownMac.memoryError).toBe('The machine did not report its memory.');
  });

  it('a host probe whose CPU command fails reports no CPU, never 0% busy', () => {
    const mac = machineProbeCommand('mac');
    expect(mac).toContain('c=$(ps -A -o %cpu=) && p=$(');
    // The Mac probe as a shell runs it, with ps failing: no cpuPct line at all.
    const script = mac
      .replace(/"\$\(sysctl -n hw\.ncpu\)"/, '8')
      .replace(/"\$\(sysctl -n vm\.loadavg \| awk '\{print \$2\}'\)"/, '1.5')
      .replace('sysctl -n hw.memsize 2>/dev/null', "printf '17179869184\\n'")
      .replace('sysctl -n kern.memorystatus_level', 'printf 67')
      .replace('ps -A -o %cpu=', 'false')
      .replace(
        /df -Pk "\$d"/,
        "printf 'Filesystem 1024-blocks Used Available Capacity Mounted\\n/dev/x 100 50 50 50%% /\\n'; true"
      );
    const out = execFileSync('bash', ['-c', script], { encoding: 'utf8' });
    // The Mac probe with `sysctl -n hw.ncpu` failing: an empty core count, so no busy figure.
    const noCores = execFileSync(
      'bash',
      [
        '-c',
        script
          .replace("cores=%s\\n' 8", 'cores=%s\\n\' "$(false)"')
          .replace('c=$(false)', "c=$(printf '320\\n')"),
      ],
      { encoding: 'utf8' }
    );
    expect(noCores).toContain('cpuPct=320');
    expect(noCores).toMatch(/^cores=$/m);
    expect(parseMachineProbe('mac', noCores)?.busyPct).toBeNull();
    expect(parseMachineProbe('mac', noCores)?.cpuError).toBe('The machine did not report its CPU.');
    // ps that succeeds but prints nothing, or nothing numeric: no figure (awk would print 0).
    for (const output of ["printf ''", "printf 'x\\n-\\n'"]) {
      const empty = execFileSync('bash', ['-c', script.replace('c=$(false)', `c=$(${output})`)], {
        encoding: 'utf8',
      });
      expect({ output, line: empty.includes('cpuPct=') }).toEqual({ output, line: false });
      expect(parseMachineProbe('mac', empty)?.cpuError).toBe('The machine did not report its CPU.');
    }
    // Control: the same script with a working ps sums its lines.
    const good = execFileSync(
      'bash',
      ['-c', script.replace('c=$(false)', "c=$(printf '10\\n20\\n')")],
      {
        encoding: 'utf8',
      }
    );
    expect(good).toContain('cpuPct=30');
    expect(parseMachineProbe('mac', good)?.cpuError).toBeUndefined();
    expect(out).not.toContain('cpuPct=');
    const sample = parseMachineProbe('mac', out);
    expect(sample?.busyPct).toBeNull();
    expect(sample?.cpuError).toBe('The machine did not report its CPU.');
    expect(sample?.memoryError).toBeUndefined();
    // Windows prints each processor's load; the server averages them (PowerShell's Measure-Object
    // would count a null LoadPercentage as 0). "cpuLoad=" + $null is the bare "cpuLoad=".
    const windowsScript = Buffer.from(
      machineProbeCommand('windows').split(' ').pop()!,
      'base64'
    ).toString('utf16le');
    expect(windowsScript).toContain('foreach ($p in $cpu) { "cpuLoad=" + $p.LoadPercentage }');
    expect(windowsScript).not.toContain('Measure-Object LoadPercentage');
    const base = 'cores=8\r\nmemTotalKb=2\r\nmemAvailKb=1\r\ndiskTotalB=10\r\ndiskFreeB=5\r\n';
    const winGood = parseMachineProbe('windows', `${base}cpuLoad=30\r\ncpuLoad=50\r\n`);
    expect(winGood?.busyPct).toBe(40);
    expect(winGood?.cpuError).toBeUndefined();
    const winIdle = parseMachineProbe('windows', `${base}cpuLoad=0\r\n`);
    expect(winIdle?.busyPct).toBe(0);
    expect(winIdle?.cpuError).toBeUndefined();
    for (const lines of ['cpuLoad=\r\ncpuLoad=50\r\n', 'cpuLoad=\r\n', '']) {
      const unknown = parseMachineProbe('windows', `${base}${lines}`);
      expect({ lines, busy: unknown?.busyPct, error: unknown?.cpuError }).toEqual({
        lines,
        busy: null,
        error: 'The machine did not report its CPU.',
      });
    }
    // Windows prints raw values: a missing one is empty, never a cast 0 (checked on the PC).
    expect(windowsScript).not.toContain('[int64]');
    expect(windowsScript).not.toContain('Measure-Object');
    // One coreCount per processor: any without a value makes the total unknown, never a short sum.
    const mem = 'memTotalKb=2\r\nmemAvailKb=1\r\ndiskTotalB=10\r\ndiskFreeB=5\r\ncpuLoad=40\r\n';
    expect(parseMachineProbe('windows', `coreCount=4\r\ncoreCount=4\r\n${mem}`)?.cores).toBe(8);
    expect(parseMachineProbe('windows', `coreCount=\r\ncoreCount=4\r\n${mem}`)?.cores).toBeNull();
    expect(parseMachineProbe('windows', `coreCount=0\r\n${mem}`)?.cores).toBeNull();
    const winEmpty = parseMachineProbe(
      'windows',
      'cores=0\r\ncpuLoad=10\r\nmemTotalKb=2\r\nmemAvailKb=\r\ndiskTotalB=10\r\ndiskFreeB=\r\nmount=C:\r\n'
    );
    expect(winEmpty?.cores).toBeNull();
    expect(winEmpty?.memoryError).toBe('The machine did not report its memory.');
    expect(winEmpty?.diskError).toBe('The machine did not report its system disk.');
    expect(winEmpty?.disk).toBeNull();
    // A Mac answer without its core count cannot divide the summed ps: no figure, named.
    const macNoCores = parseMachineProbe(
      'mac',
      'cores=\nmemTotalB=17179869184\nmemAvailPct=67\ncpuPct=320\ndiskTotalKb=10\ndiskFreeKb=5\n'
    );
    expect(macNoCores?.busyPct).toBeNull();
    expect(macNoCores?.cpuError).toBe('The machine did not report its CPU.');
    const linux = parseMachineProbe(
      'linux',
      'cores=6\nmemTotalKb=2\nmemAvailKb=1\ndiskTotalKb=10\ndiskFreeKb=5\n'
    );
    expect(linux?.cpuError).toBe('The machine did not report its CPU.');
  });

  it('reports recorded Mac available memory and ignores old page counters', () => {
    // November 18 capture: 16 GiB, 16 KiB pages, sysctl and memory_pressure both at 67%.
    // The old vm_stat probe summed free, inactive, speculative and purgeable pages.
    const oldBytes = (6344 + 318459 + 37964 + 6127) * 16384;
    expect(oldBytes / 1024 ** 3).toBeCloseTo(5.63);
    const measured = parseMachineProbe(
      'mac',
      'memTotalB=17179869184\nmemAvailPct=67\npageSize=16384\npagesAvail=6344\npagesAvail=318459\npagesAvail=37964\npagesAvail=6127\n'
    );
    expect(measured?.memTotalBytes).toBe(16 * 1024 ** 3);
    expect(measured?.memAvailableBytes).toBe(11_510_512_353.28);
    expect(measured?.memoryError).toBeUndefined();
    expect(machineWarnings(measured)).toEqual([]);

    const missing = parseMachineProbe(
      'mac',
      'memTotalB=17179869184\nmemAvailPct=\npageSize=16384\npagesAvail=6344\npagesAvail=318459\npagesAvail=37964\npagesAvail=6127\n'
    );
    expect(missing?.memAvailableBytes).toBeNull();
    expect(missing?.memoryError).toBe('The machine did not report its memory.');
    expect(machineWarnings(missing)).toEqual([]);

    const zero = parseMachineProbe('mac', 'memTotalB=17179869184\nmemAvailPct=0\n');
    expect(zero?.memAvailableBytes).toBe(0);
    expect(machineWarnings(zero)).toEqual(['Memory 0.0 GB available, under the 6 GB floor']);
    expect(
      parseMachineProbe('mac', 'memTotalB=17179869184\nmemAvailPct=101\n')?.memAvailableBytes
    ).toBeNull();
    expect(parseMachineProbe('mac', 'cores=8\nmemAvailPct=67\n')?.memAvailableBytes).toBeNull();
  });

  it('runs the same Mac shell probe in TypeScript and Rust and requires clean successful memory sources', () => {
    const probe = machineProbeCommand('mac');
    const rust = path.resolve(import.meta.dir, '../../../../crates/aac-agent/src/probes/mac.txt');
    expect(fs.readFileSync(rust, 'utf8')).toBe(probe + '\n');
    const run = (level: string, pressure: string, memsize = String.raw`printf '17179869184\n'`) => {
      const commands = String.raw`
        sysctl() {
          case "$2" in
            hw.ncpu) printf '8\n' ;;
            vm.loadavg) printf '{ 1.50 1.50 1.50 }\n' ;;
            hw.memsize) ${memsize} ;;
            kern.memorystatus_level) ${level} ;;
          esac
        }
        memory_pressure() { ${pressure}; }
        ps() { printf '32\n'; }
        df() { printf 'Filesystem 1024-blocks Used Available Capacity Mounted\n/dev/x 100 50 50 50%% /\n'; }
      `;
      const out = execFileSync('/bin/sh', ['-c', commands + probe], { encoding: 'utf8' });
      return parseMachineProbe('mac', out);
    };
    const recordedPressure = String.raw`printf 'Pages free: 6344 \nPages purgeable: 6127 \nPages inactive: 318459 \nPages speculative: 37964 \n\nSystem-wide memory free percentage: 67%%\n'`;
    expect(
      run('printf 67', 'printf "System-wide memory free percentage: 70%%\\n"')?.memAvailableBytes
    ).toBe(11_510_512_353.28);
    expect(run('printf "0\\n"', recordedPressure)?.memAvailableBytes).toBe(0);
    expect(run('return 1', recordedPressure)?.memAvailableBytes).toBe(11_510_512_353.28);
    const malformed = ['printf "N/A\\n"', 'printf "   \\n"', 'printf "101\\n"'];
    expect(malformed.map((level) => run(level, recordedPressure)?.memAvailableBytes)).toEqual([
      11_510_512_353.28, 11_510_512_353.28, 11_510_512_353.28,
    ]);
    const missing = run('return 1', 'return 127');
    expect(missing?.memAvailableBytes).toBeNull();
    expect(missing?.memoryError).toBe('The machine did not report its memory.');
    const unusable = run(malformed[0], 'printf "System-wide memory free percentage: 101%%\\n"');
    expect(unusable?.memAvailableBytes).toBeNull();
    expect(unusable?.memoryError).toBe('The machine did not report its memory.');
    // M1: hw.memsize must exit 0 with exactly one clean decimal; CPU and disk still stand.
    const badTotals = [
      String.raw`printf '17179869184\n'; return 1`,
      String.raw`printf '17179869184\ninvalid\n'`,
      String.raw`printf 'bad\nmemTotalB=2147483648\n'`,
      String.raw`printf ''`,
      String.raw`printf 'N/A\n'`,
    ];
    for (const memsize of badTotals) {
      const sample = run('printf 67', 'return 1', memsize);
      expect({ memsize, avail: sample?.memAvailableBytes }).toEqual({ memsize, avail: null });
      expect(sample?.memoryError).toBe('The machine did not report its memory.');
      expect(sample?.busyPct).toBe(4);
      expect(sample?.disk?.totalBytes).toBe(100 * 1024);
    }
    // M2: memory_pressure must exit 0 with one unconflicted percentage line.
    const badPressures = [
      String.raw`printf 'System-wide memory free percentage: 67%%\n'; return 1`,
      String.raw`printf 'System-wide memory free percentage: 67%%\nSystem-wide memory free percentage: 40%%\n'`,
      String.raw`printf ''`,
      String.raw`printf 'System-wide memory free percentage: N/A%%\n'`,
    ];
    for (const pressure of badPressures) {
      const sample = run('return 1', pressure);
      expect({ pressure, avail: sample?.memAvailableBytes }).toEqual({ pressure, avail: null });
      expect(sample?.memoryError).toBe('The machine did not report its memory.');
      expect(sample?.busyPct).toBe(4);
      expect(sample?.disk?.totalBytes).toBe(100 * 1024);
    }
    // A CRLF right after the percent sign keeps the valid fallback; agreeing duplicates are one value.
    for (const pressure of [
      String.raw`printf 'System-wide memory free percentage: 67%%\r\n'`,
      String.raw`printf 'System-wide memory free percentage: 67%% \r\n'`,
      String.raw`printf 'System-wide memory free percentage: 67%%\nSystem-wide memory free percentage: 67%%\n'`,
    ]) {
      expect(run('return 1', pressure)?.memAvailableBytes).toBe(11_510_512_353.28);
    }
  });

  it('rejects malformed and oversized Mac totals while retaining the other readings', () => {
    expect(parseMachineProbe('mac', 'memTotalB=1e308\nmemAvailPct=100\n')).toBeNull();
    const malformed = parseMachineProbe('mac', 'cores=8\nmemTotalB=1e308\nmemAvailPct=100\n');
    expect(malformed?.cores).toBe(8);
    expect(malformed?.memTotalBytes).toBeNull();
    expect(malformed?.memAvailableBytes).toBeNull();
    expect(malformed?.memoryError).toBe('The machine did not report its memory.');
    const oversized = parseMachineProbe(
      'mac',
      'cores=8\nmemTotalB=18446744073709551615\nmemAvailPct=67\n'
    );
    expect(oversized?.memAvailableBytes).toBeNull();
    expect(oversized?.memoryError).toBe('The machine did not report its memory.');
  });

  it('returns the total itself at 100% and names memory unavailable on empty wire fields', () => {
    // At the safe-integer limit the product total*pct/100 rounds one byte past the total.
    const limit = parseMachineProbe('mac', 'memTotalB=9007199254740985\nmemAvailPct=100\n');
    expect(limit?.memAvailableBytes).toBe(9007199254740985);
    expect(limit?.memTotalBytes).toBe(9007199254740985);
    expect(limit?.memoryError).toBeUndefined();
    const withCores = parseMachineProbe(
      'mac',
      'cores=8\nmemTotalB=9007199254740985\nmemAvailPct=100\n'
    );
    expect(withCores?.memAvailableBytes).toBe(9007199254740985);
    expect(withCores?.cores).toBe(8);
    const whole = parseMachineProbe('mac', 'memTotalB=17179869184\nmemAvailPct=100\n');
    expect(whole?.memAvailableBytes).toBe(17179869184);
    expect(whole?.memoryError).toBeUndefined();
    // The strict probe's failure shapes: an empty field, never an invented reading.
    expect(parseMachineProbe('mac', 'memTotalB=\nmemAvailPct=67\n')).toBeNull();
    const partial = parseMachineProbe(
      'mac',
      'cores=8\nmemTotalB=\nmemAvailPct=67\ncpuPct=32\ndiskTotalKb=10\ndiskFreeKb=5\n'
    );
    expect(partial?.memTotalBytes).toBeNull();
    expect(partial?.memAvailableBytes).toBeNull();
    expect(partial?.memoryError).toBe('The machine did not report its memory.');
    expect(partial?.busyPct).toBe(4);
    expect(partial?.disk?.totalBytes).toBe(10 * 1024);
    const noPct = parseMachineProbe(
      'mac',
      'cores=8\nmemTotalB=17179869184\nmemAvailPct=\ncpuPct=32\ndiskTotalKb=10\ndiskFreeKb=5\n'
    );
    expect(noPct?.memAvailableBytes).toBeNull();
    expect(noPct?.memoryError).toBe('The machine did not report its memory.');
    expect(noPct?.busyPct).toBe(4);
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
