import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import * as childProcess from 'child_process';
import { createHash } from 'crypto';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { isSafeUsageSshAlias } from '../../../src/web-server/services/additional-usage-transport';
import {
  ANALYTICS_HELPER_SHA256,
  analyticsHelperCommand,
  fixedAnalyticsRemoteAliases,
  resolveAnalyticsRemoteHosts,
} from '../../../src/web-server/services/analytics-remote-transport';
import { loadAnalyticsRemoteSources } from '../../../src/web-server/services/analytics-remote-sources';
import { NAS1_SSH_ALIAS } from '../../../src/web-server/services/dashboard-hosts';

// Nothing here reaches a real host: the aliases are resolved in a temporary CCS home and the only
// process call the transport makes is replaced before it runs.
const MIN_DATE = Date.parse('2026-09-01T00:00:00Z');
const originalCcsHome = process.env.CCS_HOME;
let ccsHome: string;
let cache: string;

beforeEach(() => {
  ccsHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-remote-nas1-'));
  cache = path.join(ccsHome, 'cache');
  process.env.CCS_HOME = ccsHome;
});
afterEach(() => {
  mock.restore();
  if (originalCcsHome === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = originalCcsHome;
  fs.rmSync(ccsHome, { recursive: true, force: true });
});

function writeLauncherManifest(profiles: unknown[]): void {
  fs.mkdirSync(path.join(ccsHome, '.ccs'), { recursive: true });
  fs.writeFileSync(
    path.join(ccsHome, '.ccs', 'claude-desktop-profiles.json'),
    JSON.stringify({ version: 1, profiles })
  );
}

describe('analytics remote host aliases', () => {
  it('gives Nas1 its fixed alias with no Claude desktop launcher at all', async () => {
    const expected = { mac: null, windows: null, nas1: 'nas1-agent' };
    expect(await resolveAnalyticsRemoteHosts()).toEqual(expected);
    expect(fixedAnalyticsRemoteAliases()).toEqual(expected);
    expect(NAS1_SSH_ALIAS).toBe('nas1-agent');
  });

  it('keeps taking the Mac and Windows aliases from the first launcher that names one', async () => {
    writeLauncherManifest([
      { id: 'one', email: 'one@example.test', mac: { launcherName: 'One', sshHost: 'mac-first' } },
      {
        id: 'two',
        email: 'two@example.test',
        mac: { launcherName: 'Two', sshHost: 'mac-second' },
        windows: { launcherName: 'Two', sshHost: 'win-first' },
      },
    ]);
    expect(await resolveAnalyticsRemoteHosts()).toEqual({
      mac: 'mac-first',
      windows: 'win-first',
      nas1: 'nas1-agent',
    });
  });

  it('still gives Nas1 its alias when the launcher list is unreadable or names an unsafe alias', async () => {
    fs.mkdirSync(path.join(ccsHome, '.ccs'), { recursive: true });
    fs.writeFileSync(path.join(ccsHome, '.ccs', 'claude-desktop-profiles.json'), 'not json{');
    expect(await resolveAnalyticsRemoteHosts()).toEqual({
      mac: null,
      windows: null,
      nas1: 'nas1-agent',
    });
    writeLauncherManifest([
      { id: 'one', email: 'one@example.test', mac: { launcherName: 'One', sshHost: 'bad;alias' } },
    ]);
    expect(await resolveAnalyticsRemoteHosts()).toEqual({
      mac: null,
      windows: null,
      nas1: 'nas1-agent',
    });
  });

  it('passes the same ssh alias check as every other host', async () => {
    expect(isSafeUsageSshAlias((await resolveAnalyticsRemoteHosts()).nas1)).toBe(true);
  });
});

const FORBIDDEN = [
  'activate',
  'switch',
  'key_store',
  'claude_usage',
  'auth.json',
  '.credentials.json',
];
const EMPTY_ANSWER = JSON.stringify({
  version: 1,
  truncated: false,
  kinds: {},
  rows: [],
  srows: [],
});

interface SshCall {
  file: string;
  args: string[];
  input: string;
}

/** Replaces every way this process could start another one, recording ssh and refusing the rest. */
function interceptProcesses(): { ssh: SshCall[]; other: string[] } {
  const seen = { ssh: [] as SshCall[], other: [] as string[] };
  spyOn(childProcess, 'execFile').mockImplementation(((
    file: string,
    args: string[],
    _options: unknown,
    callback: (error: Error | null, stdout: string, stderr: string) => void
  ) => {
    const call: SshCall = { file, args: [...args], input: '' };
    seen.ssh.push(call);
    return {
      stdin: {
        on: () => undefined,
        end: (data?: Buffer) => {
          call.input = data ? data.toString('utf8') : '';
          setImmediate(() => callback(null, EMPTY_ANSWER, ''));
        },
      },
    };
  }) as never);
  for (const name of ['spawn', 'spawnSync', 'exec', 'execSync', 'execFileSync', 'fork'] as const) {
    spyOn(childProcess, name).mockImplementation((() => {
      seen.other.push(name);
      throw new Error(`unexpected ${name}`);
    }) as never);
  }
  return seen;
}

describe('the only ssh command built for Nas1 by the analytics scan', () => {
  it('is the read-only analytics helper, sent to exactly nas1-agent', async () => {
    const seen = interceptProcesses();
    // The real alias resolver and the real transport: only the process start itself is replaced.
    // No launcher names the Mac or Windows here, so Nas1 is the one host with an alias.
    const { states } = await loadAnalyticsRemoteSources(MIN_DATE, { cacheDir: cache });
    expect(seen.other).toEqual([]);
    expect(seen.ssh.length).toBeGreaterThan(0);
    expect(states.filter((entry) => entry.host === 'nas1')).toHaveLength(6);
    for (const call of seen.ssh) {
      expect(call.file).toBe('ssh');
      const separator = call.args.indexOf('--');
      expect(separator).toBeGreaterThan(0);
      // After the options come exactly the destination and the one remote command.
      const [destination, command, ...rest] = call.args.slice(separator + 1);
      expect(destination).toBe('nas1-agent');
      expect(rest).toEqual([]);
      expect(command).toBe(analyticsHelperCommand('nas1'));
      expect(command).toStartWith("/usr/bin/python3 -c 'import sys,json,io;");
      // The options are the fixed non-interactive set, never forwarding, a proxy or a local command.
      const options = call.args.slice(0, separator);
      expect(options[0]).toBe('-T');
      const pairs = options.slice(1);
      expect(pairs.length % 2).toBe(0);
      for (let index = 0; index < pairs.length; index += 2) {
        expect(pairs[index]).toBe('-o');
        expect(pairs[index + 1]).toMatch(
          /^(BatchMode|ConnectTimeout|ConnectionAttempts|ServerAliveInterval|ServerAliveCountMax)=\w+$/
        );
      }
      const argv = call.args.join(' ').toLowerCase();
      for (const word of FORBIDDEN) expect(argv).not.toContain(word);
      // The helper arrives on stdin and is the pinned, read-only one: no other code ever runs.
      const payload = JSON.parse(call.input) as {
        helperSource: string;
        request: Record<string, unknown>;
      };
      expect(Object.keys(payload).sort()).toEqual(['helperSource', 'request']);
      expect(
        createHash('sha256').update(Buffer.from(payload.helperSource, 'utf8')).digest('hex')
      ).toBe(ANALYTICS_HELPER_SHA256);
      expect(payload.request.immutableSqlite).toBe(true);
      expect(Object.keys(payload.request).sort()).toEqual([
        'deadlineMs',
        'extraRoots',
        'fingerprints',
        'immutableSqlite',
        'kinds',
        'minDateMs',
      ]);
    }
  });

  it('reaches every other host only through the same helper command and its own alias', async () => {
    writeLauncherManifest([
      {
        id: 'one',
        email: 'one@example.test',
        mac: { launcherName: 'One', sshHost: 'mac-alias' },
        windows: { launcherName: 'One', sshHost: 'win-alias' },
      },
    ]);
    const seen = interceptProcesses();
    await loadAnalyticsRemoteSources(MIN_DATE, { cacheDir: cache });
    expect(seen.other).toEqual([]);
    const destinations = new Map<string, string>();
    for (const call of seen.ssh) {
      const separator = call.args.indexOf('--');
      destinations.set(call.args[separator + 1], call.args[separator + 2]);
    }
    expect([...destinations.keys()].sort()).toEqual(['mac-alias', 'nas1-agent', 'win-alias']);
    // Nas1 never takes a launcher's alias, and gets the POSIX command, not the Windows one.
    expect(destinations.get('nas1-agent')).toBe(analyticsHelperCommand('nas1'));
    expect(destinations.get('mac-alias')).toBe(analyticsHelperCommand('mac'));
    expect(destinations.get('win-alias')).toBe(analyticsHelperCommand('windows'));
  });
});
