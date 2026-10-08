/**
 * Nas1 is the fourth Update apps computer: a second Ubuntu box reached over its
 * fixed ssh alias, running the same helper with --platform ubuntu. Every command
 * below is built from fixed strings. Nothing here reaches a real computer: the
 * one process test puts a fake ssh alone on a private PATH.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import { createHash } from 'crypto';
import { spawn, spawnSync } from 'child_process';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  APP_UPDATE_SSH_HOSTS,
  appUpdateInvocation,
  helperExtract,
  helperHashQuery,
  parseDeployedChecksums,
  runHost,
  syncRemoteHelpers,
} from '../../../src/web-server/services/app-update-hosts';
import {
  EXPECTED_RESULTS,
  PLATFORMS,
  UPDATE_APP_LABELS,
  normalizeAppUpdateResults,
} from '../../../src/web-server/services/app-update-contract';
import {
  AppUpdateService,
  POSIX_EXTRACT,
} from '../../../src/web-server/services/app-update-service';
import { NAS1_SSH_ALIAS } from '../../../src/web-server/services/dashboard-hosts';

const directories: string[] = [];
afterEach(() => {
  for (const value of directories.splice(0)) fs.rmSync(value, { recursive: true, force: true });
});
function directory(prefix = 'aac-nas1-updates-') {
  const value = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  directories.push(value);
  return value;
}
function payload() {
  return JSON.stringify({
    results: Object.keys(UPDATE_APP_LABELS).map((appId) => ({
      appId,
      status: 'current',
      messageCode: 'current',
      previousVersion: '1.2.3',
      version: '1.2.3',
      manager: 'native',
      updateAttempted: true,
      restartedProcesses: 0,
    })),
  });
}
const sha256 = (text: string) => createHash('sha256').update(text).digest('hex');

describe('Nas1 as the fourth update computer', () => {
  it('adds one fixed platform: four computers times eight apps', () => {
    expect(PLATFORMS).toEqual(['ubuntu', 'mac', 'windows', 'nas1']);
    expect(EXPECTED_RESULTS).toBe(32);
    const rows = normalizeAppUpdateResults(payload(), 'nas1');
    expect(rows).toHaveLength(8);
    expect(rows.every((row) => row.platform === 'nas1' && row.status === 'current')).toBe(true);
  });

  it('runs the helper over the fixed alias as a second Ubuntu with no local job flags', () => {
    const nas1 = appUpdateInvocation('nas1', '1.3.0,1.3.1', '/never/passed/to/a/remote/host');
    expect(nas1.binary).toBe('ssh');
    // The same ssh options as the other remote computers.
    expect(nas1.args.slice(0, -2)).toEqual(
      appUpdateInvocation('mac', '1.3.0,1.3.1').args.slice(0, -2)
    );
    expect(nas1.args.slice(-3)).toEqual([
      '--',
      'nas1-agent',
      `AAC_UPDATE_PROGRESS=1 /usr/bin/python3 "$HOME/.ccs/app-updates/app_updates.py" --apply --platform ubuntu --agy-reviewed '1.3.0,1.3.1'`,
    ]);
    // The state directory and dashboard job belong to this computer's own run.
    const text = nas1.args.join(' ');
    for (const local of ['--state-dir', '--dashboard-job', '/never/passed', 'powershell'])
      expect(text).not.toContain(local);
    expect(appUpdateInvocation('nas1', '').args.at(-1)).toBe(
      'AAC_UPDATE_PROGRESS=1 /usr/bin/python3 "$HOME/.ccs/app-updates/app_updates.py" --apply --platform ubuntu'
    );
  });

  it('picks each remote alias by lookup, never by falling through to another host', () => {
    expect(APP_UPDATE_SSH_HOSTS.nas1).toBe(NAS1_SSH_ALIAS);
    expect(Object.keys(APP_UPDATE_SSH_HOSTS).sort()).toEqual(['mac', 'nas1', 'windows']);
    expect(Object.isFrozen(APP_UPDATE_SSH_HOSTS)).toBe(true);
    const destination = (platform: 'mac' | 'windows' | 'nas1') =>
      appUpdateInvocation(platform).args.at(-2);
    expect(destination('mac')).toBe(APP_UPDATE_SSH_HOSTS.mac);
    expect(destination('windows')).toBe(APP_UPDATE_SSH_HOSTS.windows);
    expect(destination('nas1')).toBe('nas1-agent');
    expect(new Set(Object.values(APP_UPDATE_SSH_HOSTS)).size).toBe(3);
  });

  it('keeps the Mac on --platform mac and Windows on its PowerShell command', () => {
    expect(appUpdateInvocation('mac', '').args.at(-1)).toBe(
      'AAC_UPDATE_PROGRESS=1 /usr/bin/python3 "$HOME/.ccs/app-updates/app_updates.py" --apply --platform mac'
    );
    const encoded = appUpdateInvocation('windows', '').args.at(-1)!;
    expect(encoded).toMatch(/^powershell\.exe -NoProfile -NonInteractive -EncodedCommand /);
    const script = Buffer.from(encoded.split(' ').at(-1)!, 'base64').toString('utf16le');
    expect(script).toContain('& python.exe $helper --apply --platform windows');
  });

  it('asks Nas1 for checksums with sha256sum and the Mac with shasum', () => {
    const nas1 = helperHashQuery('nas1');
    expect(nas1.startsWith('/usr/bin/sha256sum "$HOME/.ccs/app-updates/app_updates.py" ')).toBe(
      true
    );
    expect(nas1.endsWith(' 2>/dev/null; exit 0')).toBe(true);
    expect(nas1).not.toContain('shasum');
    const mac = helperHashQuery('mac');
    expect(mac.startsWith('/usr/bin/shasum -a 256 "$HOME/.ccs/app-updates/app_updates.py" ')).toBe(
      true
    );
    // Both POSIX hosts list the same helper files.
    expect(nas1.replace('/usr/bin/sha256sum', '')).toBe(mac.replace('/usr/bin/shasum -a 256', ''));
    expect(helperHashQuery('windows')).toMatch(
      /^powershell\.exe -NoProfile -NonInteractive -EncodedCommand [A-Za-z0-9+/=]+$/
    );
  });

  // The real sha256sum is only present where the tests run on Linux.
  (fs.existsSync('/usr/bin/sha256sum') ? it : it.skip)(
    'the Nas1 query prints checksums the sync can parse, for the helpers that exist',
    () => {
      const home = directory();
      const target = path.join(home, '.ccs', 'app-updates');
      fs.mkdirSync(target, { recursive: true });
      fs.writeFileSync(path.join(target, 'app_updates.py'), 'print("one")\n');
      fs.writeFileSync(path.join(target, 'app_update_common.py'), 'print("two")\n');
      const ran = spawnSync('/bin/sh', ['-c', helperHashQuery('nas1')], {
        env: { ...process.env, HOME: home },
        encoding: 'utf8',
      });
      // A missing helper adds no line and never fails the query.
      expect(ran.status).toBe(0);
      expect(ran.stderr).toBe('');
      expect(parseDeployedChecksums(ran.stdout)).toEqual({
        'app_updates.py': sha256('print("one")\n'),
        'app_update_common.py': sha256('print("two")\n'),
      });
    }
  );

  it('extracts on Nas1 and the Mac with the same POSIX command, on Windows with PowerShell', () => {
    expect(helperExtract('nas1')).toBe(POSIX_EXTRACT);
    expect(helperExtract('mac')).toBe(POSIX_EXTRACT);
    expect(helperExtract('windows')).toMatch(/^powershell\.exe -NoProfile /);
    expect(helperExtract('windows')).not.toBe(POSIX_EXTRACT);
  });

  it('never syncs the local Ubuntu computer', async () => {
    await expect(syncRemoteHelpers('ubuntu')).resolves.toBeUndefined();
  });
});

describe('a job saved before Nas1 existed', () => {
  const done = { state: 'done', currentApp: null, phase: null };

  it('restores its 24 results and every host it saved, with Nas1 done and empty', async () => {
    const root = directory();
    const saved = path.join(root, 'app-updates');
    fs.mkdirSync(saved);
    const rows = (['ubuntu', 'mac', 'windows'] as const).flatMap((platform) =>
      normalizeAppUpdateResults(payload(), platform)
    );
    expect(rows).toHaveLength(24);
    fs.writeFileSync(
      path.join(saved, 'dashboard-job.json'),
      JSON.stringify({
        job: {
          id: '11111111-1111-4111-8111-111111111111',
          state: 'completed',
          startedAt: '2026-10-07T00:00:00.000Z',
          finishedAt: '2026-10-07T00:05:00.000Z',
          // The previous package wrote exactly these three computers.
          activePlatform: null,
          hosts: { ubuntu: done, mac: done, windows: { ...done, currentApp: 'omp' } },
          cancelRequested: false,
          results: rows,
          expectedResults: 24,
        },
      })
    );
    const service = new AppUpdateService({ ccsDir: root, runHost: async () => payload() });
    const job = service.getStatus().job!;
    expect(job.state).toBe('completed');
    expect(job.results).toHaveLength(24);
    for (const platform of ['ubuntu', 'mac', 'windows'])
      expect(job.results.filter((row) => row.platform === platform)).toHaveLength(8);
    expect(job.results.some((row) => row.platform === 'nas1')).toBe(false);
    expect(job.hosts).toEqual({
      ubuntu: done,
      mac: done,
      windows: { ...done, currentApp: 'omp' },
      nas1: done,
    });
    // The old job keeps its own total, and never blocks the next four-computer run.
    expect(job.expectedResults).toBe(24);
    service.start();
    for (let i = 0; i < 30 && service.getStatus().job?.state === 'running'; i++)
      await new Promise((resolve) => setTimeout(resolve, 0));
    const next = service.getStatus().job!;
    expect(next.state).toBe('completed');
    expect(next.results).toHaveLength(32);
    expect(next.expectedResults).toBe(32);
    expect(Object.keys(next.hosts!)).toEqual(['ubuntu', 'mac', 'windows', 'nas1']);
  });

  it('still drops a malformed saved host instead of guessing', () => {
    const root = directory();
    const saved = path.join(root, 'app-updates');
    fs.mkdirSync(saved);
    fs.writeFileSync(
      path.join(saved, 'dashboard-job.json'),
      JSON.stringify({
        job: {
          id: '11111111-1111-4111-8111-111111111111',
          state: 'completed',
          startedAt: '2026-10-07T00:00:00.000Z',
          hosts: { ubuntu: done, mac: { ...done, state: 'finished' }, windows: done },
          results: [],
        },
      })
    );
    const job = new AppUpdateService({ ccsDir: root }).getStatus().job!;
    expect(job.state).toBe('completed');
    expect(job.hosts).toBeNull();
  });
});

describe('no account switching reaches Nas1', () => {
  // Words that would mean the server asked Nas1 to switch, sign in or read an account file.
  const FORBIDDEN = [
    'activate',
    'switch',
    'key_store',
    'claude_usage',
    'auth.json',
    '.credentials.json',
  ];
  const UPDATE_COMMAND =
    /^AAC_UPDATE_PROGRESS=1 \/usr\/bin\/python3 "\$HOME\/\.ccs\/app-updates\/app_updates\.py" --apply --platform ubuntu( --agy-reviewed '[0-9A-Za-z_.,-]{1,4096}')?$/;
  const HASH_QUERY =
    /^\/usr\/bin\/sha256sum( "\$HOME\/\.ccs\/app-updates\/[a-z0-9_]+\.(?:py|cjs)")+ 2>\/dev\/null; exit 0$/;

  /**
   * Asks `ssh` to identify itself the way runHost starts it (async spawn with an
   * env copy). The fake answers "sandbox"; a real ssh would reject the unknown
   * option and exit before it connects to anything.
   */
  function whichSsh(): Promise<string> {
    return new Promise((resolve) => {
      const child = spawn('ssh', ['--aac-probe'], {
        env: { ...process.env },
        stdio: ['ignore', 'pipe', 'ignore'],
      });
      let out = '';
      child.stdout?.on('data', (chunk) => (out += chunk));
      child.on('error', () => resolve('not found'));
      child.on('close', () => resolve(out.trim()));
    });
  }

  /**
   * Runs the real runHost for Nas1 with a fake ssh as the only program on PATH,
   * so no real ssh can be reached, and returns the argv that fake received.
   */
  async function sshArgvOfRealRun(): Promise<string[]> {
    const sandbox = directory('aac-nas1-fake-ssh-');
    const bin = path.join(sandbox, 'bin');
    const log = path.join(sandbox, 'argv.log');
    fs.mkdirSync(bin);
    fs.writeFileSync(
      path.join(bin, 'ssh'),
      [
        '#!/bin/sh',
        `if [ "$1" = '--aac-probe' ]; then printf 'sandbox\\n'; exit 0; fi`,
        `printf '%s\\n' "$@" > '${log}'`,
        `printf '%s\\n' '{"results": []}'`,
        '',
      ].join('\n'),
      { mode: 0o755 }
    );
    const original = process.env.PATH;
    process.env.PATH = bin;
    try {
      // Fails here, before runHost starts, if this runtime ignores the private PATH.
      expect(await whichSsh()).toBe('sandbox');
      expect(await runHost('nas1')).toBe('{"results": []}');
    } finally {
      if (original === undefined) delete process.env.PATH;
      else process.env.PATH = original;
    }
    return fs.readFileSync(log, 'utf8').split('\n').slice(0, -1);
  }

  it('builds only fixed commands for the fixed alias, none that touch accounts', async () => {
    const spawned = await sshArgvOfRealRun();
    // What the real run spawned is exactly the invocation the server builds.
    expect(spawned).toEqual(appUpdateInvocation('nas1').args);

    // Every ssh argv the server can build for Nas1 from the update code: the
    // update run (as built, without a review list, and as spawned), the hash
    // query and the extract. Add the analytics helper command here once that
    // chunk is merged.
    const commands: Array<{ name: string; argv: string[]; fixed: (command: string) => boolean }> = [
      {
        name: 'update run',
        argv: appUpdateInvocation('nas1', '1.3.0,1.3.1').args,
        fixed: (command) => UPDATE_COMMAND.test(command),
      },
      {
        name: 'update run without a review list',
        argv: appUpdateInvocation('nas1', '').args,
        fixed: (command) => UPDATE_COMMAND.test(command),
      },
      {
        name: 'update run as spawned',
        argv: spawned,
        fixed: (command) => UPDATE_COMMAND.test(command),
      },
      {
        name: 'hash query',
        argv: ['--', APP_UPDATE_SSH_HOSTS.nas1, helperHashQuery('nas1')],
        fixed: (command) => HASH_QUERY.test(command),
      },
      {
        name: 'extract',
        argv: ['--', APP_UPDATE_SSH_HOSTS.nas1, helperExtract('nas1')],
        fixed: (command) => command === POSIX_EXTRACT,
      },
    ];
    const problems: string[] = [];
    for (const { name, argv, fixed } of commands) {
      if (argv.at(-3) !== '--' || argv.at(-2) !== 'nas1-agent')
        problems.push(`${name}: destination`);
      if (!fixed(argv.at(-1)!)) problems.push(`${name}: not one of the fixed commands`);
      for (const word of FORBIDDEN)
        if (argv.join(' ').toLowerCase().includes(word)) problems.push(`${name}: ${word}`);
    }
    expect(problems).toEqual([]);
    expect(commands).toHaveLength(5);
  });
});
