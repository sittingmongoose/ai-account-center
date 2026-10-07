import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  AppUpdateBusyError,
  AppUpdateService,
  antigravityReviewedArgument,
  appUpdateInvocation,
  normalizeAppUpdateResults,
  parseDeployedChecksums,
  UPDATE_APP_LABELS,
  type UpdatePlatform,
} from '../../../src/web-server/services/app-update-service';
import { APP_UPDATE_SSH_HOSTS } from '../../../src/web-server/services/app-update-hosts';

const directories: string[] = [];
afterEach(() => {
  for (const value of directories.splice(0)) fs.rmSync(value, { recursive: true, force: true });
});
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
async function finish(service: AppUpdateService) {
  for (let i = 0; i < 30 && service.getStatus().job?.state === 'running'; i++)
    await new Promise((resolve) => setTimeout(resolve, 0));
}
function directory() {
  const value = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-updater-test-'));
  directories.push(value);
  return value;
}

describe('fixed app update service', () => {
  it('status reads never trigger an update', () => {
    let calls = 0;
    const service = new AppUpdateService({
      persist: false,
      runHost: async () => {
        calls++;
        return payload();
      },
    });
    expect(service.getStatus()).toEqual({ job: null });
    expect(service.getStatus()).toEqual({ job: null });
    expect(calls).toBe(0);
  });
  it('coalesces one asynchronous job and continues other hosts after a failure', async () => {
    const calls: UpdatePlatform[] = [];
    let release!: (value: string) => void;
    const blocked = new Promise<string>((resolve) => {
      release = resolve;
    });
    const service = new AppUpdateService({
      persist: false,
      runHost: async (host) => {
        calls.push(host);
        if (host === 'ubuntu') return blocked;
        if (host === 'mac') throw new Error('PRIVATE_SENTINEL');
        return payload();
      },
    });
    const started = service.start();
    expect(started.job.state).toBe('running');
    expect(calls).toEqual(['ubuntu']);
    expect(() => service.start()).toThrow(AppUpdateBusyError);
    // Mac and Windows never wait for the blocked Ubuntu run.
    for (let i = 0; i < 10; i++) await new Promise((resolve) => setTimeout(resolve, 0));
    expect([...calls].sort()).toEqual(['mac', 'ubuntu', 'windows']);
    expect(
      service.getStatus().job!.results.filter((row) => row.platform !== 'ubuntu')
    ).toHaveLength(14);
    release(payload());
    await finish(service);
    const job = service.getStatus().job!;
    expect(calls).toHaveLength(3);
    expect(job.results).toHaveLength(21);
    expect(job.state).toBe('failed');
    expect(job.activePlatform).toBeNull();
    expect(JSON.stringify(job)).not.toContain('PRIVATE_SENTINEL');
    const clone = service.getStatus().job!;
    clone.results.length = 0;
    expect(service.getStatus().job!.results).toHaveLength(21);
  });
  it('executes only the three fixed host-local helpers', () => {
    const local = appUpdateInvocation('ubuntu');
    expect(local.binary).toBe('/usr/bin/python3');
    expect(local.args.slice(1, 4)).toEqual(['--apply', '--platform', 'ubuntu']);
    const mac = appUpdateInvocation('mac');
    expect(mac.binary).toBe('ssh');
    expect(mac.args.slice(-2, -1)).toEqual([APP_UPDATE_SSH_HOSTS.mac]);
    expect(mac.args.at(-1)).toContain('$HOME/.ccs/app-updates/app_updates.py');
    const windows = appUpdateInvocation('windows');
    expect(windows.args.slice(-2, -1)).toEqual([APP_UPDATE_SSH_HOSTS.windows]);
    const encoded = windows.args.at(-1)!.split(' ').at(-1)!;
    const script = Buffer.from(encoded, 'base64').toString('utf16le');
    expect(script).toContain('--apply --platform windows');
    expect(script).not.toContain('Invoke-Expression');
  });
  it('hands every host the packaged Antigravity reviewed versions, quoted', () => {
    const reviewed = antigravityReviewedArgument();
    expect(reviewed.split(',')).toContain('1.3.0');
    expect(reviewed).toMatch(/^\d+\.\d+\.\d+(,\d+\.\d+\.\d+)*$/);
    expect(appUpdateInvocation('ubuntu').args.slice(-2)).toEqual(['--agy-reviewed', reviewed]);
    expect(appUpdateInvocation('mac').args.at(-1)).toMatch(
      new RegExp(`--apply --platform mac --agy-reviewed '${reviewed.replace(/\./g, '\\.')}'$`)
    );
    const decode = (args: string[]) =>
      Buffer.from(args.at(-1)!.split(' ').at(-1)!, 'base64').toString('utf16le');
    // PowerShell would split an unquoted comma list into separate arguments.
    expect(decode(appUpdateInvocation('windows').args)).toContain(
      `& python.exe $helper --apply --platform windows --agy-reviewed '${reviewed}'`
    );
    // An unusable release file sends no list: every helper then holds the update.
    const dir = directory();
    fs.writeFileSync(path.join(dir, 'release.json'), '{"reviewedNatives": "broken"}');
    expect(antigravityReviewedArgument(path.join(dir, 'release.json'))).toBe('');
    expect(appUpdateInvocation('ubuntu', '').args).not.toContain('--agy-reviewed');
    expect(appUpdateInvocation('mac', "1.3.0'; rm -rf ~; '").args.at(-1)).not.toContain('rm -rf');
    expect(decode(appUpdateInvocation('windows', '').args)).not.toContain('--agy-reviewed');
  });
  it('accepts a held Antigravity row with the build it held and keeps it after a restore', () => {
    const rows = JSON.parse(payload()).results;
    rows[0] = {
      ...rows[0],
      status: 'held',
      messageCode: 'held_for_review',
      previousVersion: '1.2.16',
      version: '1.2.16',
      heldVersion: '1.3.0',
      updateAttempted: false,
    };
    rows[1] = { ...rows[1], status: 'held', messageCode: 'held_for_review', heldVersion: '1.3.0' };
    const value = normalizeAppUpdateResults(JSON.stringify({ results: rows }), 'ubuntu');
    expect(value[0]).toMatchObject({
      appId: 'antigravity-cli',
      status: 'held',
      version: '1.2.16',
      heldVersion: '1.3.0',
      updateAttempted: false,
    });
    expect(value[0].message).toContain('waiting for a switching review');
    // Only the Antigravity CLI is ever held.
    expect(value[1].status).toBe('failed');
    expect(value[1].heldVersion).toBeUndefined();
    const unchecked = normalizeAppUpdateResults(
      JSON.stringify({
        results: [{ ...rows[0], messageCode: 'held_unchecked', heldVersion: '1.3.0' }],
      }),
      'mac'
    )[0];
    expect(unchecked.status).toBe('held');
    expect(unchecked.heldVersion).toBeUndefined();
    for (const bad of [
      { ...rows[0], messageCode: 'current' },
      { ...rows[0], status: 'current', messageCode: 'held_for_review' },
      { ...rows[0], status: 'current', messageCode: 'updated_unreviewed' },
    ])
      expect(
        normalizeAppUpdateResults(JSON.stringify({ results: [bad] }), 'windows')[0].status
      ).toBe('failed');
    const hostile = normalizeAppUpdateResults(
      JSON.stringify({ results: [{ ...rows[0], heldVersion: '1.3.0; id' }] }),
      'ubuntu'
    )[0];
    expect(hostile.status).toBe('held');
    expect(JSON.stringify(hostile)).not.toContain('; id');
    const unreviewed = normalizeAppUpdateResults(
      JSON.stringify({
        results: [
          {
            ...rows[0],
            status: 'updated',
            messageCode: 'updated_unreviewed',
            version: '1.3.1',
            heldVersion: '1.3.1',
          },
        ],
      }),
      'ubuntu'
    )[0];
    expect(unreviewed).toMatchObject({ status: 'updated', version: '1.3.1' });
    expect(unreviewed.message).toContain('switching is paused');
    expect(unreviewed.heldVersion).toBeUndefined();
  });
  it('completes a job with a held row and restores the held build from disk', async () => {
    const root = directory();
    const rows = JSON.parse(payload()).results;
    rows[0] = { ...rows[0], status: 'held', messageCode: 'held_for_review', heldVersion: '1.3.0' };
    const service = new AppUpdateService({
      ccsDir: root,
      runHost: async () => JSON.stringify({ results: rows }),
    });
    service.start();
    await finish(service);
    expect(service.getStatus().job!.state).toBe('completed');
    const restored = new AppUpdateService({ ccsDir: root, runHost: async () => payload() });
    const held = restored.getStatus().job!.results.filter((row) => row.status === 'held');
    expect(held).toHaveLength(3);
    expect(held.every((row) => row.heldVersion === '1.3.0')).toBe(true);
  });
  it('restores a quit-first desktop row from disk with its own words', async () => {
    const root = directory();
    const rows = JSON.parse(payload()).results;
    const index = rows.findIndex((row: { appId: string }) => row.appId === 'codex-desktop');
    rows[index] = {
      ...rows[index],
      status: 'action_required',
      messageCode: 'quit_first',
      updateAttempted: false,
    };
    const service = new AppUpdateService({
      ccsDir: root,
      runHost: async () => JSON.stringify({ results: rows }),
    });
    service.start();
    await finish(service);
    const restored = new AppUpdateService({ ccsDir: root, runHost: async () => payload() });
    const quit = restored.getStatus().job!.results.filter((row) => row.appId === 'codex-desktop');
    expect(quit).toHaveLength(3);
    for (const row of quit) {
      expect(row.status).toBe('action_required');
      expect(row.message).toBe(
        'Quit Codex Desktop to finish its update, then run Update apps again.'
      );
    }
  });
  it('normalizes only safe whitelist metadata and never full helper responses', () => {
    const rows = JSON.parse(payload()).results;
    rows[0] = {
      ...rows[0],
      status: 'updated',
      messageCode: 'updated',
      secret: 'PRIVATE_SENTINEL',
      message: 'PRIVATE_SENTINEL',
      forcedStops: 2,
      restartedProcesses: 1,
      restartTargets: [
        {
          kind: 'tmux',
          server: 'ccs-updates-0123456789ab',
          session: 'ccs-updated-antigravity-cli-1',
          env: 'PRIVATE_SENTINEL',
        },
        { kind: 'tmux', server: '$(unsafe)', session: 'invalid' },
      ],
    };
    const result = normalizeAppUpdateResults(
      JSON.stringify({ results: rows, token: 'PRIVATE_SENTINEL' }),
      'mac'
    );
    expect(result[0].platform).toBe('mac');
    expect(result[0].forcedStops).toBe(2);
    expect(result[0].restartTargets).toHaveLength(1);
    expect(JSON.stringify(result)).not.toContain('PRIVATE_SENTINEL');
    expect(JSON.stringify(result)).not.toContain('unsafe');
  });
  it.each(['not-json', '{"results":[]}', 'x'.repeat(65537)])(
    'returns fixed errors for malformed helper output',
    (raw) => {
      const rows = normalizeAppUpdateResults(raw, 'ubuntu');
      expect(rows).toHaveLength(7);
      expect(rows.every((row) => row.status === 'failed')).toBe(true);
    }
  );
  it('rejects duplicate identities and unbounded versions', () => {
    const rows = JSON.parse(payload()).results;
    rows.push(rows[0]);
    rows[1].version = 'SECRET_TOKEN';
    rows[1].status = 'updated';
    const value = normalizeAppUpdateResults(JSON.stringify({ results: rows }), 'ubuntu');
    expect(value[0].status).toBe('failed');
    expect(value[1].status).toBe('failed');
    expect(JSON.stringify(value)).not.toContain('SECRET_TOKEN');
  });
  it('accepts action-required rows and completes the job without failures', async () => {
    const rows = JSON.parse(payload()).results;
    rows[0] = { ...rows[0], status: 'action_required', messageCode: 'quit_first' };
    rows[1] = {
      ...rows[1],
      status: 'action_required',
      messageCode: 'quit_first',
      updateAttempted: false,
      previousVersion: null,
      version: null,
    };
    rows[2] = { ...rows[2], status: 'action_required', messageCode: 'check_in_app' };
    const normalized = normalizeAppUpdateResults(JSON.stringify({ results: rows }), 'mac');
    expect(normalized[0].status).toBe('action_required');
    expect(normalized[0].message).toBe(
      `Quit ${normalized[0].appLabel} to finish its update, then run Update apps again.`
    );
    expect(normalized[1].message).toBe(
      `Quit ${normalized[1].appLabel} to finish its update, then run Update apps again.`
    );
    expect(normalized[2].message).toBe(
      'The download was blocked; open the app to check for updates.'
    );
    const service = new AppUpdateService({
      persist: false,
      runHost: async () => JSON.stringify({ results: rows }),
    });
    service.start();
    await finish(service);
    expect(service.getStatus().job!.state).toBe('completed');
  });
  it('rejects action-required rows with unknown message codes', () => {
    const rows = JSON.parse(payload()).results;
    rows[0] = { ...rows[0], status: 'action_required', messageCode: 'bogus' };
    const value = normalizeAppUpdateResults(JSON.stringify({ results: rows }), 'ubuntu');
    expect(value[0].status).toBe('failed');
    expect(JSON.stringify(value)).not.toContain('bogus');
  });
  it('uses an owner-only cross-process lock and restores safe completed status', async () => {
    const root = directory();
    let release!: (value: string) => void;
    const blocked = new Promise<string>((resolve) => {
      release = resolve;
    });
    const first = new AppUpdateService({
      ccsDir: root,
      runHost: async (host) => (host === 'ubuntu' ? blocked : payload()),
    });
    first.start();
    const second = new AppUpdateService({ ccsDir: root, runHost: async () => payload() });
    expect(() => second.start()).toThrow(AppUpdateBusyError);
    expect(fs.statSync(path.join(root, 'app-updates/dashboard-update.lock')).mode & 0o077).toBe(0);
    release(payload());
    await finish(first);
    expect(second.getStatus().job!.state).toBe('completed');
    expect(second.getStatus().job!.results).toHaveLength(21);
    expect(fs.existsSync(path.join(root, 'app-updates/dashboard-update.lock'))).toBe(false);
  });
  it('refuses a cancel from a process that does not own the running job', async () => {
    const root = directory();
    let release!: (value: string) => void;
    const blocked = new Promise<string>((resolve) => {
      release = resolve;
    });
    const owner = new AppUpdateService({
      ccsDir: root,
      runHost: async (host) => (host === 'ubuntu' ? blocked : payload()),
    });
    owner.start();
    const other = new AppUpdateService({ ccsDir: root, runHost: async () => payload() });
    const refused = other.cancel();
    expect(refused.cancelling).toBe(false);
    expect(refused.notOwner).toBe(true);
    expect(refused.job?.state).toBe('running');
    expect(refused.job?.cancelRequested).toBe(false);
    // Nothing was written over the owner's job file.
    const saved = JSON.parse(
      fs.readFileSync(path.join(root, 'app-updates/dashboard-job.json'), 'utf8')
    );
    expect(saved.job.cancelRequested).toBe(false);
    // The owner itself still cancels and honours it.
    expect(owner.cancel().cancelling).toBe(true);
    release(payload());
    await finish(owner);
    expect(owner.getStatus().job!.results.filter((row) => row.status === 'skipped')).toHaveLength(
      14
    );
  });
  it('marks an interrupted persisted job failed without replaying it', () => {
    const root = directory();
    const sub = path.join(root, 'app-updates');
    fs.mkdirSync(sub);
    fs.writeFileSync(
      path.join(sub, 'dashboard-job.json'),
      JSON.stringify({
        job: {
          id: '11111111-1111-4111-8111-111111111111',
          state: 'running',
          startedAt: '2026-10-01T00:00:00Z',
          results: [],
        },
      })
    );
    let calls = 0;
    const service = new AppUpdateService({
      ccsDir: root,
      runHost: async () => {
        calls++;
        return payload();
      },
    });
    expect(service.getStatus().job!.state).toBe('failed');
    expect(calls).toBe(0);
  });
  it('syncs deployed helpers before each remote run and survives a sync failure', async () => {
    const events: string[] = [];
    const service = new AppUpdateService({
      persist: false,
      sync: async (platform) => {
        events.push(`sync:${platform}`);
        if (platform === 'mac') throw new Error('PRIVATE_SENTINEL');
      },
      runHost: async (platform) => {
        events.push(`run:${platform}`);
        return payload();
      },
    });
    service.start();
    await finish(service);
    // Hosts run side by side; each remote host still syncs before it runs.
    expect(events).toHaveLength(5);
    expect(events.indexOf('sync:mac')).toBeLessThan(events.indexOf('run:mac'));
    expect(events.indexOf('sync:windows')).toBeLessThan(events.indexOf('run:windows'));
    expect(events.filter((event) => event.startsWith('sync:')).sort()).toEqual([
      'sync:mac',
      'sync:windows',
    ]);
    expect(events).not.toContain('sync:ubuntu');
    expect(service.getStatus().job!.state).toBe('completed');
    expect(JSON.stringify(service.getStatus().job)).not.toContain('PRIVATE_SENTINEL');
  });
  it('parses deployed helper checksum lines from both host shells', () => {
    const hash = 'a'.repeat(64);
    const other = 'B'.repeat(64);
    const parsed = parseDeployedChecksums(
      [
        `${hash}  /Users/x/.ccs/app-updates/app_updates.py`,
        `${other}  app_update_common.py`,
        'shasum: /Users/x/.ccs/app-updates/app_update_pipe.py: No such file or directory',
        'not-a-hash  app_updates.py',
      ].join('\n')
    );
    expect(parsed['app_updates.py']).toBe(hash);
    expect(parsed['app_update_common.py']).toBe('b'.repeat(64));
    expect(Object.keys(parsed)).toHaveLength(2);
  });
});
