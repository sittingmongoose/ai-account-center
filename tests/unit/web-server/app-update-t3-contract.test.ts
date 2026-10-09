import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  EXPECTED_RESULTS,
  MESSAGES,
  normalizeAppUpdateRow,
  UPDATE_APP_LABELS,
} from '../../../src/web-server/services/app-update-contract';
import { AppUpdateService } from '../../../src/web-server/services/app-update-service';

const temporary: string[] = [];
afterEach(() => {
  for (const directory of temporary.splice(0))
    fs.rmSync(directory, { recursive: true, force: true });
});
const version = '0.0.46-nightly.20261007.2774';
const target = { kind: 'systemd', service: 't3code.service', delaySeconds: 30 };
function row() {
  return {
    appId: 't3-code',
    status: 'updated',
    messageCode: 't3_restart_scheduled',
    previousVersion: '0.0.46-nightly.20261006.2735',
    version,
    manager: 'native',
    updateAttempted: true,
    restartedProcesses: 0,
    restartTargets: [target],
  };
}

describe('T3 Code update contract', () => {
  it('includes desktop and server in one fixed app per host and keeps exact nightly versions', () => {
    expect(UPDATE_APP_LABELS['t3-code']).toBe('T3 Code');
    expect(EXPECTED_RESULTS).toBe(40);
    expect(normalizeAppUpdateRow(row(), 't3-code', 'ubuntu')).toMatchObject({
      appLabel: 'T3 Code',
      status: 'updated',
      version,
      restartedProcesses: 0,
      message: MESSAGES.t3_restart_scheduled,
      restartTargets: [target],
    });
  });

  it('accepts only the fixed delayed Linux T3 service target', () => {
    // Ubuntu and Nas1 are both Linux computers: the same systemd target, nothing else.
    for (const platform of ['ubuntu', 'nas1'] as const) {
      for (const restartTargets of [
        [],
        [{ ...target, service: 'ccs-dashboard.service' }],
        [{ ...target, delaySeconds: 0 }],
        [{ ...target, service: '../t3code.service' }],
      ]) {
        expect(
          normalizeAppUpdateRow({ ...row(), restartTargets }, 't3-code', platform).message
        ).toBe(MESSAGES.helper_invalid);
      }
      expect(normalizeAppUpdateRow(row(), 'omp', platform).message).toBe(MESSAGES.helper_invalid);
    }
    expect(normalizeAppUpdateRow(row(), 't3-code', 'mac').message).toBe(MESSAGES.helper_invalid);
    expect(normalizeAppUpdateRow(row(), 't3-code', 'windows').message).toBe(
      MESSAGES.helper_invalid
    );
  });

  it('accepts the Nas1 systemd restart exactly like the Ubuntu one', () => {
    const nas1 = normalizeAppUpdateRow(row(), 't3-code', 'nas1');
    expect(nas1).toMatchObject({
      platform: 'nas1',
      appLabel: 'T3 Code',
      status: 'updated',
      version,
      message: MESSAGES.t3_restart_scheduled,
      restartTargets: [target],
    });
    expect({ ...nas1, platform: 'ubuntu' }).toEqual(
      normalizeAppUpdateRow(row(), 't3-code', 'ubuntu')
    );
  });

  it('accepts a T3 desktop restart only for Mac or Windows', () => {
    const desktop = {
      ...row(),
      messageCode: 't3_updated',
      manager: 'official-download',
      restartTargets: [{ kind: 'desktop' }],
    };
    expect(normalizeAppUpdateRow(desktop, 't3-code', 'windows').restartTargets).toEqual([
      { kind: 'desktop' },
    ]);
    expect(normalizeAppUpdateRow(desktop, 't3-code', 'mac').restartTargets).toEqual([
      { kind: 'desktop' },
    ]);
    expect(normalizeAppUpdateRow(desktop, 't3-code', 'ubuntu').restartTargets).toEqual([]);
    // Nas1 has no T3 desktop app: the desktop target is dropped, never accepted.
    expect(normalizeAppUpdateRow(desktop, 't3-code', 'nas1').restartTargets).toEqual([]);
    expect(
      normalizeAppUpdateRow({ ...desktop, messageCode: 't3_restart_scheduled' }, 't3-code', 'nas1')
        .message
    ).toBe(MESSAGES.helper_invalid);
    expect(
      normalizeAppUpdateRow({ ...desktop, messageCode: 'updated' }, 'claude-desktop', 'mac')
        .restartTargets
    ).toEqual([]);
  });

  it('restores the deferred restart message and target without scheduling or replaying it', async () => {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-t3-contract-'));
    temporary.push(directory);
    const service = new AppUpdateService({
      ccsDir: directory,
      runHost: async (platform) =>
        JSON.stringify({
          results: Object.keys(UPDATE_APP_LABELS).map((appId) =>
            appId === 't3-code' && (platform === 'ubuntu' || platform === 'nas1')
              ? row()
              : {
                  appId,
                  status: 'current',
                  messageCode: 'current',
                  version: '1.2.3',
                  manager: 'native',
                }
          ),
        }),
    });
    service.start();
    for (let i = 0; i < 30 && service.getStatus().job?.state === 'running'; i++)
      await new Promise((resolve) => setTimeout(resolve, 0));
    const restored = new AppUpdateService({
      ccsDir: directory,
      runHost: async () => {
        throw new Error('No replay');
      },
    });
    for (const platform of ['ubuntu', 'nas1']) {
      expect(
        restored
          .getStatus()
          .job?.results.find((result) => result.appId === 't3-code' && result.platform === platform)
      ).toMatchObject({
        message: MESSAGES.t3_restart_scheduled,
        restartTargets: [target],
        version,
      });
    }
  });
});
