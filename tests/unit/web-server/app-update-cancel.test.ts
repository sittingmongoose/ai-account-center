import { describe, expect, it } from 'bun:test';
import {
  AppUpdateService,
  normalizeAppUpdateResults,
  UPDATE_APP_LABELS,
  type UpdatePlatform,
} from '../../../src/web-server/services/app-update-service';
import { MESSAGES } from '../../../src/web-server/services/app-update-contract';

function payload(status = 'current') {
  return JSON.stringify({
    results: Object.keys(UPDATE_APP_LABELS).map((appId) => ({
      appId,
      status,
      messageCode: status,
      previousVersion: '1.2.3',
      version: '1.2.3',
      manager: 'native',
      updateAttempted: true,
      restartedProcesses: 0,
    })),
  });
}

async function finish(service: AppUpdateService) {
  for (let i = 0; i < 60 && service.getStatus().job?.state === 'running'; i++)
    await new Promise((resolve) => setTimeout(resolve, 0));
}

describe('cancellable app updates', () => {
  it('cancel during the first app finishes it and skips the rest', async () => {
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
        return payload();
      },
    });
    service.start();
    const ack = service.cancel();
    expect(ack.cancelling).toBe(true);
    expect(ack.job?.state).toBe('running');
    expect(ack.job?.cancelRequested).toBe(true);
    release(payload());
    await finish(service);
    const job = service.getStatus().job!;
    expect(calls).toEqual(['ubuntu']);
    const running = job.results.filter((row) => row.platform === 'ubuntu');
    const queued = job.results.filter((row) => row.platform !== 'ubuntu');
    expect(running).toHaveLength(7);
    expect(running.every((row) => row.status === 'current')).toBe(true);
    expect(queued).toHaveLength(14);
    expect(queued.every((row) => row.status === 'skipped')).toBe(true);
    expect(queued.every((row) => row.message === MESSAGES.skipped_cancelled)).toBe(true);
    expect(queued.every((row) => row.updateAttempted === false)).toBe(true);
    expect(job.results).toHaveLength(21);
    expect(job.expectedResults).toBe(21);
    expect(job.state).toBe('completed');
    expect(job.cancelRequested).toBe(true);
    expect(job.activePlatform).toBeNull();
    expect(job.finishedAt).not.toBeNull();
    expect(JSON.stringify(job)).not.toContain('undo');
  });

  it('cancel after the last app changes nothing', async () => {
    const service = new AppUpdateService({ persist: false, runHost: async () => payload() });
    service.start();
    await finish(service);
    const before = service.getStatus().job!;
    expect(before.state).toBe('completed');
    expect(before.cancelRequested).toBe(false);
    const outcome = service.cancel();
    expect(outcome.cancelling).toBe(false);
    expect(outcome.job).toEqual(before);
    expect(service.getStatus().job).toEqual(before);
  });

  it('a double cancel is idempotent', async () => {
    let release!: (value: string) => void;
    const blocked = new Promise<string>((resolve) => {
      release = resolve;
    });
    const service = new AppUpdateService({
      persist: false,
      runHost: async (host) => (host === 'ubuntu' ? blocked : payload()),
    });
    service.start();
    const first = service.cancel();
    const second = service.cancel();
    expect(first.cancelling).toBe(true);
    expect(second).toEqual(first);
    release(payload());
    await finish(service);
    const job = service.getStatus().job!;
    expect(job.results).toHaveLength(21);
    expect(job.results.filter((row) => row.status === 'skipped')).toHaveLength(14);
  });

  it('cancel with no job is a no-op', () => {
    const service = new AppUpdateService({ persist: false, runHost: async () => payload() });
    expect(service.cancel()).toEqual({ job: null, cancelling: false });
  });

  it('readiness unknown is distinct from failed', () => {
    const rows = JSON.parse(payload()).results;
    rows[0] = { appId: rows[0].appId, status: 'unknown', messageCode: 'readiness_unknown' };
    rows[1] = { ...rows[1], status: 'failed', messageCode: 'unsupported' };
    const normalized = normalizeAppUpdateResults(JSON.stringify({ results: rows }), 'mac');
    expect(normalized[0].status).toBe('unknown');
    expect(normalized[0].message).toBe(MESSAGES.readiness_unknown);
    expect(normalized[0].updateAttempted).toBe(false);
    expect(normalized[1].status).toBe('failed');
    expect(normalized[1].message).toBe(MESSAGES.unsupported);
    expect(normalized[0].message).not.toBe(normalized[1].message);
  });

  it('an unreachable host reports unknown and the job cannot complete', async () => {
    const service = new AppUpdateService({
      persist: false,
      runHost: async (host) => {
        if (host === 'windows') throw new Error('PRIVATE_SENTINEL');
        return payload();
      },
    });
    service.start();
    await finish(service);
    const job = service.getStatus().job!;
    const windows = job.results.filter((row) => row.platform === 'windows');
    expect(windows).toHaveLength(7);
    expect(windows.every((row) => row.status === 'unknown')).toBe(true);
    expect(windows.every((row) => row.message === MESSAGES.host_unknown)).toBe(true);
    expect(windows.every((row) => row.updateAttempted === false)).toBe(true);
    expect(job.state).toBe('failed');
    expect(JSON.stringify(job)).not.toContain('PRIVATE_SENTINEL');
  });

  it('cancel and readiness DTO shape', async () => {
    expect(MESSAGES.skipped_cancelled).toBe('Skipped: cancelled');
    let release!: (value: string) => void;
    const blocked = new Promise<string>((resolve) => {
      release = resolve;
    });
    const service = new AppUpdateService({
      persist: false,
      runHost: async (host) => (host === 'ubuntu' ? blocked : payload()),
    });
    expect(service.start().job.cancelRequested).toBe(false);
    const ack = service.cancel();
    expect(typeof ack.cancelling).toBe('boolean');
    expect(ack.job?.cancelRequested).toBe(true);
    release(payload());
    await finish(service);
    const row = service.getStatus().job!.results.find((value) => value.status === 'skipped')!;
    expect(Object.keys(row).sort()).toEqual(
      [
        'appId',
        'appLabel',
        'platform',
        'status',
        'previousVersion',
        'version',
        'manager',
        'message',
        'updateAttempted',
        'restartedProcesses',
        'forcedStops',
        'restartTargets',
      ].sort()
    );
  });
});
