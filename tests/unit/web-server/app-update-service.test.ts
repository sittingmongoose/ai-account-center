import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  AppUpdateBusyError,
  AppUpdateService,
  appUpdateInvocation,
  normalizeAppUpdateResults,
  UPDATE_APP_LABELS,
  type UpdatePlatform,
} from '../../../src/web-server/services/app-update-service';

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
    release(payload());
    await finish(service);
    const job = service.getStatus().job!;
    expect(calls).toEqual(['ubuntu', 'mac', 'windows']);
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
    expect(local.args.slice(-3)).toEqual(['--apply', '--platform', 'ubuntu']);
    const mac = appUpdateInvocation('mac');
    expect(mac.binary).toBe('ssh');
    expect(mac.args.slice(-2, -1)).toEqual(['jared-mac']);
    expect(mac.args.at(-1)).toContain('$HOME/.ccs/app-updates/app_updates.py');
    const windows = appUpdateInvocation('windows');
    expect(windows.args.slice(-2, -1)).toEqual(['jared-windows']);
    const encoded = windows.args.at(-1)!.split(' ').at(-1)!;
    const script = Buffer.from(encoded, 'base64').toString('utf16le');
    expect(script).toContain('--apply --platform windows');
    expect(script).not.toContain('Invoke-Expression');
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
    expect(normalized[0].message).toBe('Quit the app, then run Update apps again.');
    expect(normalized[1].message).toBe('Quit the app, then run Update apps again.');
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
});
