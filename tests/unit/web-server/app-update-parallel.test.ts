/**
 * Update apps must never hang and never serialise one computer behind another.
 * Fake hosts and fake helper processes only: nothing here reaches a real
 * computer, installer or app.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import { spawnSync } from 'child_process';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  AppUpdateService,
  MAC_EXTRACT,
  UPDATE_APP_LABELS,
  type HostRunControl,
  type UpdateAppId,
  type UpdatePlatform,
} from '../../../src/web-server/services/app-update-service';
import { runHelperProcess } from '../../../src/web-server/services/app-update-hosts';
import {
  MESSAGES,
  normalizeAppUpdateResults,
} from '../../../src/web-server/services/app-update-contract';

const APPS = Object.keys(UPDATE_APP_LABELS) as UpdateAppId[];
const directories: string[] = [];
afterEach(() => {
  for (const value of directories.splice(0)) fs.rmSync(value, { recursive: true, force: true });
});

function row(appId: UpdateAppId, status = 'current', messageCode = status) {
  return {
    appId,
    status,
    messageCode,
    previousVersion: '1.2.3',
    version: '1.2.3',
    manager: 'native',
    updateAttempted: status !== 'skipped',
    restartedProcesses: 0,
  };
}
const payload = (rows = APPS.map((appId) => row(appId))) => JSON.stringify({ results: rows });
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
async function finish(service: AppUpdateService, limitMs = 3000) {
  const end = Date.now() + limitMs;
  while (service.getStatus().job?.state === 'running' && Date.now() < end) await sleep(2);
}

describe('Update apps runs every computer at once', () => {
  it('finishes in about the slowest host, not the sum of all three', async () => {
    const delays: Record<UpdatePlatform, number> = { ubuntu: 600, mac: 400, windows: 200 };
    const started: Record<string, number> = {};
    const service = new AppUpdateService({
      persist: false,
      runHost: async (platform) => {
        started[platform] = Date.now();
        await sleep(delays[platform]);
        return payload();
      },
    });
    const t0 = Date.now();
    service.start();
    await sleep(50);
    const mid = service.getStatus().job!;
    expect(mid.hosts).not.toBeNull();
    expect(Object.values(mid.hosts!).map((host) => host.state)).toEqual([
      'running',
      'running',
      'running',
    ]);
    await finish(service);
    const elapsed = Date.now() - t0;
    const job = service.getStatus().job!;
    expect(job.state).toBe('completed');
    expect(job.results).toHaveLength(21);
    // Serial would be >= 1200 ms; parallel is the slowest host plus scheduling.
    expect(elapsed).toBeGreaterThanOrEqual(595);
    expect(elapsed).toBeLessThan(1000);
    expect(Math.max(...Object.values(started)) - Math.min(...Object.values(started))).toBeLessThan(
      50
    );
    expect(Object.values(job.hosts!).every((host) => host.state === 'done')).toBe(true);
    expect(job.activePlatform).toBeNull();
  });

  it('a hanging host times out into per-app rows while the others finish', async () => {
    let aborted = 0;
    const service = new AppUpdateService({
      persist: false,
      hostDeadlineMs: 120,
      runHost: (platform, control) => {
        if (platform !== 'ubuntu') return sleep(20).then(() => payload());
        control.setAbort(() => aborted++);
        // One app reports, then the next probe hangs forever.
        control.onEvent({ event: 'app', appId: 'antigravity-cli', phase: 'updating' });
        control.onEvent({ event: 'result', result: row('antigravity-cli', 'updated', 'updated') });
        control.onEvent({ event: 'app', appId: 'muse-code', phase: 'checking' });
        return new Promise<string>(() => {});
      },
    });
    const t0 = Date.now();
    service.start();
    await sleep(60);
    const running = service.getStatus().job!;
    expect(running.state).toBe('running');
    expect(running.hosts!.mac.state).toBe('done');
    expect(running.hosts!.windows.state).toBe('done');
    expect(running.hosts!.ubuntu).toEqual({
      state: 'running',
      currentApp: 'muse-code',
      phase: 'checking',
      phaseSince: expect.any(String),
    });
    expect(running.activePlatform).toBe('ubuntu');
    await finish(service);
    expect(Date.now() - t0).toBeLessThan(1000);
    const job = service.getStatus().job!;
    expect(aborted).toBe(1);
    expect(job.results).toHaveLength(21);
    const ubuntu = job.results.filter((value) => value.platform === 'ubuntu');
    expect(ubuntu.find((value) => value.appId === 'antigravity-cli')!.status).toBe('updated');
    const timedOut = ubuntu.filter((value) => value.appId !== 'antigravity-cli');
    expect(timedOut).toHaveLength(6);
    expect(timedOut.every((value) => value.status === 'unknown')).toBe(true);
    expect(timedOut.every((value) => value.message === MESSAGES.host_timeout)).toBe(true);
    expect(timedOut.every((value) => value.updateAttempted === false)).toBe(true);
    expect(job.results.filter((value) => value.platform !== 'ubuntu')).toHaveLength(14);
    expect(job.state).toBe('failed');
  });

  it('a slow helper sync is bounded by the same host deadline', async () => {
    const service = new AppUpdateService({
      persist: false,
      hostDeadlineMs: 80,
      sync: (platform) => (platform === 'mac' ? new Promise<void>(() => {}) : Promise.resolve()),
      runHost: async () => payload(),
    });
    service.start();
    await finish(service);
    const mac = service.getStatus().job!.results.filter((value) => value.platform === 'mac');
    expect(mac).toHaveLength(7);
    expect(mac.every((value) => value.message === MESSAGES.host_timeout)).toBe(true);
  });

  it('shows a desktop download live with the time its phase began', async () => {
    let clock = Date.parse('2026-10-06T15:09:16.000Z');
    let finishWindows!: () => void;
    const gate = new Promise<void>((resolve) => {
      finishWindows = resolve;
    });
    let control!: HostRunControl;
    const service = new AppUpdateService({
      persist: false,
      now: () => clock,
      runHost: async (platform, value) => {
        if (platform !== 'windows') return payload();
        control = value;
        value.onEvent({ event: 'app', appId: 'codex-desktop', phase: 'updating' });
        await gate;
        return payload();
      },
    });
    service.start();
    await sleep(20);
    clock += 2000;
    control.onEvent({ event: 'app', appId: 'codex-desktop', phase: 'downloading' });
    const downloading = service.getStatus().job!.hosts!.windows;
    expect(downloading.currentApp).toBe('codex-desktop');
    expect(downloading.phase).toBe('downloading');
    expect(downloading.phaseSince).toBe('2026-10-06T15:09:18.000Z');
    // The same phase again keeps its start, so the elapsed time keeps counting.
    clock += 5000;
    control.onEvent({ event: 'app', appId: 'codex-desktop', phase: 'downloading' });
    expect(service.getStatus().job!.hosts!.windows.phaseSince).toBe('2026-10-06T15:09:18.000Z');
    control.onEvent({ event: 'app', appId: 'codex-desktop', phase: 'quitting' });
    expect(service.getStatus().job!.hosts!.windows.phase).toBe('downloading');
    finishWindows();
    await finish(service);
    expect(service.getStatus().job!.hosts!.windows).toEqual({
      state: 'done',
      currentApp: null,
      phase: null,
    });
  });

  it('names the app in its quit-first row', () => {
    const rows = JSON.parse(payload()).results;
    const index = rows.findIndex((value: { appId: string }) => value.appId === 'codex-desktop');
    rows[index] = { ...rows[index], status: 'action_required', messageCode: 'quit_first' };
    const normalized = normalizeAppUpdateResults(JSON.stringify({ results: rows }), 'windows');
    expect(normalized.find((value) => value.appId === 'codex-desktop')!.message).toBe(
      'Quit Codex Desktop to finish its update, then run Update apps again.'
    );
  });

  it('streams each app row as it lands and merges the final document once', async () => {
    let finishUbuntu!: () => void;
    const gate = new Promise<void>((resolve) => {
      finishUbuntu = resolve;
    });
    const service = new AppUpdateService({
      persist: false,
      runHost: async (platform, control) => {
        if (platform !== 'ubuntu') return payload();
        control.onEvent({ event: 'app', appId: null, phase: 'checking' });
        control.onEvent({ event: 'result', result: row('omp') });
        control.onEvent({ event: 'result', result: row('omp', 'failed', 'update_failed') });
        control.onEvent({ event: 'result', result: { appId: 'not-an-app', status: 'updated' } });
        control.onEvent({ event: 'app', appId: 'codex-cli', phase: 'updating' });
        control.onEvent({ event: 'app', appId: '$(evil)', phase: 'updating' });
        await gate;
        return payload();
      },
    });
    service.start();
    await sleep(20);
    const mid = service.getStatus().job!;
    const ubuntu = mid.results.filter((value) => value.platform === 'ubuntu');
    expect(ubuntu.map((value) => [value.appId, value.status])).toEqual([['omp', 'current']]);
    expect(mid.hosts!.ubuntu.currentApp).toBe('codex-cli');
    expect(mid.hosts!.ubuntu.phase).toBe('updating');
    expect(JSON.stringify(mid)).not.toContain('evil');
    expect(JSON.stringify(mid)).not.toContain('not-an-app');
    finishUbuntu();
    await finish(service);
    const job = service.getStatus().job!;
    expect(job.results).toHaveLength(21);
    expect(job.results.filter((value) => value.platform === 'ubuntu')).toHaveLength(7);
    expect(job.state).toBe('completed');
  });

  it('keeps streamed rows when the host drops mid-run and marks only the rest unknown', async () => {
    const service = new AppUpdateService({
      persist: false,
      runHost: async (platform, control) => {
        if (platform !== 'windows') return payload();
        control.onEvent({ event: 'result', result: row('codex-cli', 'updated', 'updated') });
        control.onEvent({ event: 'result', result: row('claude-code') });
        throw new Error('PRIVATE_SENTINEL');
      },
    });
    service.start();
    await finish(service);
    const windows = service
      .getStatus()
      .job!.results.filter((value) => value.platform === 'windows');
    expect(windows).toHaveLength(7);
    expect(windows.find((value) => value.appId === 'codex-cli')!.status).toBe('updated');
    expect(windows.find((value) => value.appId === 'claude-code')!.status).toBe('current');
    const rest = windows.filter((value) => !['codex-cli', 'claude-code'].includes(value.appId));
    expect(rest.every((value) => value.message === MESSAGES.host_unknown)).toBe(true);
    expect(JSON.stringify(service.getStatus())).not.toContain('PRIVATE_SENTINEL');
  });

  it('forwards one cancel to every running host; each skips the apps it has not started', async () => {
    const handlers: Record<string, number> = {};
    const releases: Array<() => void> = [];
    const service = new AppUpdateService({
      persist: false,
      runHost: (platform, control) =>
        new Promise<string>((resolve) => {
          let cancelled = false;
          control.setCancel(() => {
            handlers[platform] = (handlers[platform] ?? 0) + 1;
            cancelled = true;
          });
          releases.push(() =>
            resolve(
              payload(
                APPS.map((appId, index) =>
                  // The app running at the cancel finishes; the rest are skipped.
                  cancelled && index > 0 ? row(appId, 'skipped', 'skipped_cancelled') : row(appId)
                )
              )
            )
          );
        }),
    });
    service.start();
    await sleep(10);
    expect(service.cancel().cancelling).toBe(true);
    expect(service.cancel().cancelling).toBe(true);
    expect(handlers).toEqual({ ubuntu: 1, mac: 1, windows: 1 });
    for (const release of releases) release();
    await finish(service);
    const job = service.getStatus().job!;
    expect(job.results).toHaveLength(21);
    expect(job.results.filter((value) => value.status === 'skipped')).toHaveLength(18);
    expect(job.results.filter((value) => value.status === 'current')).toHaveLength(3);
    expect(job.cancelRequested).toBe(true);
    expect(job.state).toBe('completed');
  });

  it('a host still syncing when the cancel lands never starts its helper', async () => {
    const runs: UpdatePlatform[] = [];
    let releaseSync!: () => void;
    const syncing = new Promise<void>((resolve) => {
      releaseSync = resolve;
    });
    const service = new AppUpdateService({
      persist: false,
      sync: () => syncing,
      runHost: async (platform) => {
        runs.push(platform);
        return payload();
      },
    });
    service.start();
    service.cancel();
    releaseSync();
    await finish(service);
    expect(runs).toEqual(['ubuntu']);
    const remote = service.getStatus().job!.results.filter((value) => value.platform !== 'ubuntu');
    expect(remote.every((value) => value.status === 'skipped')).toBe(true);
  });

  it('restores saved host progress and drops anything outside the fixed values', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-parallel-restore-'));
    directories.push(root);
    fs.mkdirSync(path.join(root, 'app-updates'));
    const save = (hosts: unknown) =>
      fs.writeFileSync(
        path.join(root, 'app-updates', 'dashboard-job.json'),
        JSON.stringify({
          job: {
            id: '11111111-1111-4111-8111-111111111111',
            state: 'completed',
            startedAt: '2026-10-06T00:00:00Z',
            finishedAt: '2026-10-06T00:05:00Z',
            activePlatform: null,
            hosts,
            results: [],
          },
        })
      );
    const done = { state: 'done', currentApp: null, phase: null };
    save({ ubuntu: done, mac: done, windows: { ...done, currentApp: 'omp', phase: 'updating' } });
    const restored = new AppUpdateService({ ccsDir: root }).getStatus().job!;
    expect(restored.hosts!.windows).toEqual({
      state: 'done',
      currentApp: 'omp',
      phase: 'updating',
    });
    save({ ubuntu: done, mac: done, windows: { ...done, currentApp: 'rm -rf' } });
    expect(new AppUpdateService({ ccsDir: root }).getStatus().job!.hosts).toBeNull();
    save(undefined);
    expect(new AppUpdateService({ ccsDir: root }).getStatus().job!.hosts).toBeNull();
  });
});

describe('fake helper processes', () => {
  // A stand-in helper: streams progress, then waits for a cancel line on stdin.
  const helper = `
    const out = (value) => process.stdout.write(JSON.stringify(value) + '\\n');
    out({ event: 'app', appId: 'omp', phase: 'updating' });
    out({ event: 'result', result: { appId: 'omp', status: 'current', messageCode: 'current' } });
    process.stdout.write('not json\\n');
    let input = '';
    process.stdin.on('data', (chunk) => {
      input += chunk;
      if (input.includes('cancel\\n')) {
        out({ results: [{ appId: 'omp', status: 'current', messageCode: 'current' },
          { appId: 'codex-cli', status: 'skipped', messageCode: 'skipped_cancelled' }] });
        process.exit(0);
      }
    });
  `;

  it('relays progress lines and a cancel over stdin to a real child process', async () => {
    const events: Array<Record<string, unknown>> = [];
    let cancel: () => void = () => {};
    const control: HostRunControl = {
      onEvent: (event) => {
        events.push(event);
        if (event.event === 'result') cancel();
      },
      setCancel: (handler) => {
        cancel = handler;
      },
      setAbort: () => {},
    };
    const output = await runHelperProcess(process.execPath, ['-e', helper], process.env, control);
    expect(events.map((event) => event.event)).toEqual(['app', 'result']);
    expect(JSON.parse(output).results[1].status).toBe('skipped');
  });

  it('a helper that never answers is stopped by its abort and rejects', async () => {
    let abort: () => void = () => {};
    const control: HostRunControl = {
      onEvent: () => {},
      setCancel: () => {},
      setAbort: (handler) => {
        abort = handler;
      },
    };
    const pending = runHelperProcess(
      process.execPath,
      ['-e', 'setInterval(() => {}, 1000)'],
      process.env,
      control
    );
    const t0 = Date.now();
    setTimeout(() => abort(), 50);
    await expect(pending).rejects.toThrow('App update host failed.');
    expect(Date.now() - t0).toBeLessThan(3000);
  });
});

describe('the real Python helper with fake apps', () => {
  it('streams live rows and stops starting apps after a cancel on stdin', async () => {
    const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-helper-stream-'));
    directories.push(home);
    // Every app is a fixture: detection, readiness and updates are replaced, so
    // nothing on this computer is probed, stopped or updated.
    const driver = path.join(home, 'driver.py');
    fs.writeFileSync(
      driver,
      [
        'import pathlib, sys, time',
        `sys.path.insert(0, ${JSON.stringify(path.resolve(__dirname, '../../../scripts/app-updates'))})`,
        'import app_updates as u, app_update_common as c',
        "u.detect = lambda platform: {k: c.Install(k, platform, pathlib.Path('/fixture/' + k), '1.0.0') for k in c.APP_LABELS}",
        'u.check_readiness = lambda install: None',
        'def fake(install, deadline):',
        '    time.sleep(0.3)',
        "    return c.result(install.app_id, install.platform, 'current', '1.0.0', '1.0.0', 'native', attempted=True)",
        'u.update_cli = fake',
        'u.update_desktop = fake',
        "sys.argv = ['app_updates.py', '--apply', '--platform', 'ubuntu']",
        'u.main()',
      ].join('\n')
    );
    const events: Array<Record<string, unknown>> = [];
    let cancel: () => void = () => {};
    const control: HostRunControl = {
      onEvent: (event) => {
        events.push(event);
        if (event.event === 'result' && events.filter((e) => e.event === 'result').length === 1)
          cancel();
      },
      setCancel: (handler) => {
        cancel = handler;
      },
      setAbort: () => {},
    };
    const t0 = Date.now();
    const output = await runHelperProcess(
      'python3',
      [driver],
      { ...process.env, HOME: home, AAC_UPDATE_PROGRESS: '1' },
      control
    );
    const rows = JSON.parse(output).results as Array<{ status: string; messageCode: string }>;
    expect(rows).toHaveLength(7);
    expect(rows[0].status).toBe('current');
    const skippedRows = rows.filter((value) => value.status === 'skipped');
    // The app already started when the cancel landed may finish; nothing after it starts.
    expect(skippedRows.length).toBeGreaterThanOrEqual(5);
    expect(skippedRows.every((value) => value.messageCode === 'skipped_cancelled')).toBe(true);
    expect(rows.every((value) => ['current', 'skipped'].includes(value.status))).toBe(true);
    expect(Date.now() - t0).toBeLessThan(2000);
    expect(events[0]).toEqual({ event: 'app', appId: null, phase: 'checking' });
    expect(events.filter((event) => event.event === 'result')).toHaveLength(7);
  });
});

describe('Mac helper sync', () => {
  it('uses only tools that exist on macOS', () => {
    expect(MAC_EXTRACT).not.toContain('/usr/bin/chmod');
    const tools = MAC_EXTRACT.match(/\/(?:usr\/)?bin\/[a-z]+/g);
    expect(tools).toEqual(['/bin/mkdir', '/bin/chmod', '/usr/bin/tar']);
  });

  it('really extracts a helper archive into a private directory', () => {
    const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-mac-extract-'));
    const source = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-mac-source-'));
    directories.push(home, source);
    fs.writeFileSync(path.join(source, 'app_updates.py'), 'print("fixture")\n');
    const archive = spawnSync('tar', ['-c', '-f', '-', '-C', source, 'app_updates.py']);
    expect(archive.status).toBe(0);
    const extracted = spawnSync('/bin/sh', ['-c', MAC_EXTRACT], {
      input: archive.stdout,
      env: { ...process.env, HOME: home },
    });
    expect(extracted.status).toBe(0);
    const target = path.join(home, '.ccs', 'app-updates');
    expect(fs.readFileSync(path.join(target, 'app_updates.py'), 'utf8')).toBe('print("fixture")\n');
    expect(fs.statSync(target).mode & 0o777).toBe(0o700);
  });
});
