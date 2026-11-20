/**
 * Update all, 2026-11-20: on the Linux computers T3's new server starts only after that
 * computer's part of the job, so the Antigravity ACP row said it would be checked "on the next
 * Update apps" and nothing checked it again. The job now waits for that restart and runs the
 * check itself, replacing the deferred row with the real result.
 */
import { describe, expect, it } from 'bun:test';
import {
  AppUpdateService,
  appUpdateInvocation,
  UPDATE_APP_LABELS,
  type HostRunControl,
  type UpdatePlatform,
} from '../../../src/web-server/services/app-update-service';
import { HOST_OS } from '../../../src/web-server/services/dashboard-hosts';
import { MESSAGES } from '../../../src/web-server/services/app-update-contract';
import { isLegacyMessage } from '../../../src/web-server/services/app-update-messages';

const NEW_T3 = '0.1.12-nightly.20261120.4994';
const OLD_T3 = '0.1.11-nightly.20261119.4941';
const OLD_WORDS =
  'T3 was updated in this run and its server restarts after the job, so Antigravity ACP is checked against the new T3 on the next Update apps.';

/** One computer's run as the helper reports it: T3 updated, and on Linux the ACP row deferred. */
function run(platform: UpdatePlatform): string {
  const linux = HOST_OS[platform] === 'linux';
  return JSON.stringify({
    results: Object.keys(UPDATE_APP_LABELS).map((appId) => {
      if (appId === 't3-code')
        return {
          appId,
          status: 'updated',
          messageCode: linux ? 't3_restart_scheduled' : 't3_updated',
          previousVersion: OLD_T3,
          version: NEW_T3,
          manager: 'native',
          updateAttempted: true,
          restartedProcesses: 0,
          restartTargets: linux
            ? [{ kind: 'systemd', service: 't3code.service', delaySeconds: 30 }]
            : [{ kind: 'desktop' }],
        };
      if (appId === 'antigravity-acp' && linux)
        return {
          appId,
          status: 'action_required',
          messageCode: 't3_restart_pending',
          previousVersion: '1.5.12',
          version: '1.5.12',
          manager: 'official-download',
          updateAttempted: false,
          restartedProcesses: 0,
          diagnostics: { cause: 't3_restart_pending', reason: 'deferred_server_restart' },
        };
      return {
        appId,
        status: 'current',
        messageCode: 'current',
        previousVersion: '1.5.13',
        version: '1.5.13',
        manager: appId === 'antigravity-acp' ? 'official-download' : 'native',
        updateAttempted: true,
        restartedProcesses: 0,
      };
    }),
  });
}

const FOLLOW_UP_ROW = JSON.stringify({
  results: [
    {
      appId: 'antigravity-acp',
      status: 'updated',
      messageCode: 'antigravity_acp_updated',
      previousVersion: '1.5.12',
      version: '1.5.13',
      manager: 'official-download',
      updateAttempted: true,
      restartedProcesses: 0,
      diagnostics: { cause: 'none' },
    },
  ],
});

async function finish(service: AppUpdateService) {
  for (let i = 0; i < 100 && service.getStatus().job?.state === 'running'; i++)
    await new Promise((resolve) => setTimeout(resolve, 0));
}

function acpRows(service: AppUpdateService) {
  return Object.fromEntries(
    service
      .getStatus()
      .job!.results.filter((row) => row.appId === 'antigravity-acp')
      .map((row) => [row.platform, row])
  );
}

describe('Antigravity ACP after a Linux computer restarts T3', () => {
  it('the deferred row says the check follows by itself, not on the next Update apps', () => {
    expect(MESSAGES.t3_restart_pending).not.toContain('next Update apps');
    expect(MESSAGES.t3_restart_pending).toContain('by itself');
    // Jobs saved with the old words still restore as this code.
    expect(isLegacyMessage('t3_restart_pending', OLD_WORDS)).toBe(true);
  });

  it('runs the follow-up on every Linux computer once its part is done and records the real row', async () => {
    const followUps: Array<{ platform: UpdatePlatform; hostState: string; t3Version: string }> = [];
    let service!: AppUpdateService;
    service = new AppUpdateService({
      persist: false,
      runHost: async (platform: UpdatePlatform, control: HostRunControl) => {
        if (!control.followUp) return run(platform);
        // The detached restart worker restarts T3 only once this computer reads done.
        followUps.push({
          platform,
          hostState: service.getStatus().job!.hosts![platform].state,
          t3Version: control.followUp.t3Version,
        });
        return FOLLOW_UP_ROW;
      },
    });
    service.start();
    await finish(service);
    const job = service.getStatus().job!;
    const linux = (Object.keys(HOST_OS) as UpdatePlatform[]).filter(
      (host) => HOST_OS[host] === 'linux' && job.hosts?.[host]
    );
    expect(linux.length).toBeGreaterThan(0);
    expect(followUps.map((call) => call.platform).sort()).toEqual([...linux].sort());
    expect(followUps.every((call) => call.hostState === 'done')).toBe(true);
    expect(followUps.every((call) => call.t3Version === NEW_T3)).toBe(true);
    const rows = acpRows(service);
    for (const host of linux) {
      expect(rows[host]).toMatchObject({
        status: 'updated',
        previousVersion: '1.5.12',
        version: '1.5.13',
        message: MESSAGES.antigravity_acp_updated,
      });
    }
    // Mac and Windows reopen T3 within the run: no follow-up, their rows stand.
    for (const host of Object.keys(rows) as UpdatePlatform[])
      if (HOST_OS[host] !== 'linux') expect(rows[host].status).toBe('current');
    expect(job.results.some((row) => row.message === MESSAGES.t3_restart_pending)).toBe(false);
    expect(job.state).toBe('completed');
  });

  it('a follow-up that cannot finish never leaves the deferred words behind', async () => {
    const service = new AppUpdateService({
      persist: false,
      runHost: async (platform: UpdatePlatform, control: HostRunControl) => {
        if (!control.followUp) return run(platform);
        throw new Error('PRIVATE_SENTINEL');
      },
    });
    service.start();
    await finish(service);
    const job = service.getStatus().job!;
    for (const row of Object.values(acpRows(service)))
      if (HOST_OS[row.platform] === 'linux')
        expect(row).toMatchObject({ status: 'unknown', message: MESSAGES.host_unknown });
    expect(JSON.stringify(job)).not.toContain('PRIVATE_SENTINEL');
    expect(job.results.some((row) => row.message === MESSAGES.t3_restart_pending)).toBe(false);
  });

  it('a follow-up past its deadline reads timed out', async () => {
    const service = new AppUpdateService({
      persist: false,
      followUpDeadlineMs: 5,
      runHost: async (platform: UpdatePlatform, control: HostRunControl) => {
        if (!control.followUp) return run(platform);
        return new Promise<string>(() => {});
      },
    });
    service.start();
    for (let i = 0; i < 50 && service.getStatus().job?.state === 'running'; i++)
      await new Promise((resolve) => setTimeout(resolve, 5));
    for (const row of Object.values(acpRows(service)))
      if (HOST_OS[row.platform] === 'linux')
        expect(row).toMatchObject({ status: 'unknown', message: MESSAGES.host_timeout });
  });

  it('a cancelled job skips the follow-up', async () => {
    let followUps = 0;
    let service!: AppUpdateService;
    service = new AppUpdateService({
      persist: false,
      runHost: async (platform: UpdatePlatform, control: HostRunControl) => {
        if (control.followUp) {
          followUps++;
          return FOLLOW_UP_ROW;
        }
        service.cancel();
        return run(platform);
      },
    });
    service.start();
    await finish(service);
    expect(followUps).toBe(0);
    for (const row of Object.values(acpRows(service)))
      if (HOST_OS[row.platform] === 'linux')
        expect(row).toMatchObject({ status: 'skipped', message: MESSAGES.skipped_cancelled });
  });

  it('invokes the helper follow-up only on Linux, with a checked T3 version or any', async () => {
    const local = await appUpdateInvocation('ubuntu', '', '/fixture/state', { t3Version: NEW_T3 });
    expect(local.args.slice(1)).toEqual([
      '--apply',
      '--platform',
      'ubuntu',
      '--state-dir',
      '/fixture/state',
      '--acp-after-t3-restart',
      NEW_T3,
    ]);
    // The follow-up never schedules another restart: no --dashboard-job.
    expect(local.args).not.toContain('--dashboard-job');
    const nas1 = await appUpdateInvocation('nas1', '', undefined, { t3Version: NEW_T3 });
    expect(nas1.args.at(-1)).toContain(
      `--apply --platform ubuntu --acp-after-t3-restart '${NEW_T3}'`
    );
    const unsafe = await appUpdateInvocation('nas1', '', undefined, { t3Version: "1'; rm -rf ~" });
    expect(unsafe.args.at(-1)).toContain("--acp-after-t3-restart 'any'");
    await expect(
      appUpdateInvocation('windows', '', undefined, { t3Version: NEW_T3 })
    ).rejects.toThrow();
    await expect(
      appUpdateInvocation('mac', '', undefined, { t3Version: NEW_T3 })
    ).rejects.toThrow();
  });
});
