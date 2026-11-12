import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  EXPECTED_RESULTS,
  LEGACY_STORAGE_APPS,
  MESSAGES,
  UPDATE_APP_LABELS,
  normalizeAppUpdateRow,
} from '../../../src/web-server/services/app-update-contract';
import { AppUpdateService } from '../../../src/web-server/services/app-update-service';
import { LegacyAppUpdateReader } from '../../fixtures/legacy-computer-readers';

const roots: string[] = [];
afterEach(() => {
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});
const rows = Object.keys(UPDATE_APP_LABELS).map((appId) => ({
  appId,
  status: 'current',
  messageCode: 'current',
  previousVersion: '1.3.0',
  version: '1.3.0',
  manager: 'native',
  updateAttempted: false,
  restartedProcesses: 0,
}));

describe('Cursor CLI and T3 Antigravity ACP update rows', () => {
  it('reports unknown install state and unconfirmed revoke only as failed ACP rows', () => {
    for (const messageCode of [
      't3_install_state_unknown',
      't3_session_revoke_failed',
      't3_session_issue_unknown',
    ] as const) {
      const row = { ...rows[0], status: 'failed', messageCode };
      expect(normalizeAppUpdateRow(row, 'antigravity-acp', 'windows').message).toBe(
        MESSAGES[messageCode]
      );
      expect(normalizeAppUpdateRow(row, 'cursor-cli', 'windows').message).toBe(
        MESSAGES.helper_invalid
      );
      expect(
        normalizeAppUpdateRow({ ...row, status: 'updated' }, 'antigravity-acp', 'windows').message
      ).toBe(MESSAGES.helper_invalid);
    }
  });

  it('keeps legacy staged rows actionable by rerunning Update apps', () => {
    const staged = {
      ...rows[0],
      status: 'action_required',
      updateAttempted: true,
      messageCode: 'antigravity_acp_staged',
    };
    for (const platform of ['ubuntu', 'mac', 'windows', 'nas1', 'alexis'] as const) {
      expect(normalizeAppUpdateRow(staged, 'antigravity-acp', platform)).toMatchObject({
        status: 'action_required',
        version: '1.3.0',
        message: MESSAGES.antigravity_acp_staged,
      });
    }
    expect(normalizeAppUpdateRow(staged, 'cursor-cli', 'windows').message).toBe(
      MESSAGES.helper_invalid
    );
    expect(
      normalizeAppUpdateRow({ ...staged, status: 'updated' }, 'antigravity-acp', 'windows').message
    ).toBe(MESSAGES.helper_invalid);
  });

  it('reports newer kept and legacy activation failure only for ACP', () => {
    for (const [status, messageCode] of [
      ['current', 'antigravity_acp_newer_kept'],
      ['failed', 'antigravity_acp_activation_unsettled'],
    ] as const) {
      const row = { ...rows[0], status, messageCode };
      expect(normalizeAppUpdateRow(row, 'antigravity-acp', 'windows').message).toBe(
        MESSAGES[messageCode]
      );
      expect(normalizeAppUpdateRow(row, 'cursor-cli', 'windows').message).toBe(
        MESSAGES.helper_invalid
      );
    }
    expect(MESSAGES.antigravity_acp_staged).toContain('Run Update apps again');
  });

  it('reports T3 API errors only on ACP with the correct failure status', () => {
    for (const code of Object.keys(MESSAGES).filter(
      (code) =>
        code === 't3_unreachable' ||
        code === 't3_auth_failed' ||
        code === 't3_install_timeout' ||
        code.startsWith('t3_install_failed:')
    )) {
      const status = ['t3_unreachable', 't3_auth_failed'].includes(code)
        ? 'action_required'
        : 'failed';
      const row = { ...rows[0], status, messageCode: code };
      expect(normalizeAppUpdateRow(row, 'antigravity-acp', 'windows').message).toBe(MESSAGES[code]);
      expect(normalizeAppUpdateRow(row, 'cursor-cli', 'windows').message).toBe(
        MESSAGES.helper_invalid
      );
      expect(
        normalizeAppUpdateRow({ ...row, status: 'updated' }, 'antigravity-acp', 'windows').message
      ).toBe(MESSAGES.helper_invalid);
    }
    expect(MESSAGES.antigravity_acp_updated).toContain('T3 installed and activated');
    expect(MESSAGES.t3_unreachable).toContain('AAC did not write');
  });

  it('expects twelve apps on five computers and keeps the rollback app set frozen', () => {
    expect(EXPECTED_RESULTS).toBe(60);
    expect(Object.keys(UPDATE_APP_LABELS)).toHaveLength(12);
    expect(LEGACY_STORAGE_APPS).toHaveLength(10);
    expect(LEGACY_STORAGE_APPS).not.toContain('cursor-cli');
    expect(LEGACY_STORAGE_APPS).not.toContain('antigravity-acp');
  });

  it('keeps the new update words and versions without inventing a process restart', () => {
    for (const appId of ['cursor-cli', 'antigravity-acp'] as const) {
      const code = appId === 'cursor-cli' ? 'cursor_updated' : 'antigravity_acp_updated';
      for (const platform of ['ubuntu', 'mac', 'windows', 'nas1', 'alexis'] as const) {
        const row = normalizeAppUpdateRow(
          {
            appId,
            status: 'updated',
            previousVersion: '1.3.0',
            version: '1.4.5',
            manager: 'native',
            updateAttempted: true,
            restartedProcesses: 0,
            messageCode: code,
          },
          appId,
          platform
        );
        expect(row).toMatchObject({
          status: 'updated',
          previousVersion: '1.3.0',
          version: '1.4.5',
          restartedProcesses: 0,
        });
        expect(row.message).toBe(MESSAGES[code]);
      }
    }
  });

  it('permits staging only for Windows Cursor and GUI failures only for the two Mac clients', () => {
    const staged = {
      ...rows[0],
      status: 'action_required',
      updateAttempted: true,
      messageCode: 'cursor_update_staged',
    };
    for (const platform of ['windows', 'alexis'] as const)
      expect(normalizeAppUpdateRow(staged, 'cursor-cli', platform).message).toBe(
        MESSAGES.cursor_update_staged
      );
    expect(normalizeAppUpdateRow(staged, 'cursor-cli', 'mac').message).toBe(
      MESSAGES.helper_invalid
    );
    expect(normalizeAppUpdateRow(staged, 'omp', 'windows').message).toBe(MESSAGES.helper_invalid);
    for (const code of [
      'mac_keychain_locked',
      'mac_gui_unavailable',
      'mac_gui_cleanup_failed',
    ] as const) {
      const failed = { ...rows[0], status: 'failed', messageCode: code };
      for (const appId of ['muse-code', 'cursor-cli'] as const)
        expect(normalizeAppUpdateRow(failed, appId, 'mac').message).toBe(MESSAGES[code]);
      expect(normalizeAppUpdateRow(failed, 'muse-code', 'windows').message).toBe(
        MESSAGES.helper_invalid
      );
      expect(normalizeAppUpdateRow(failed, 'omp', 'mac').message).toBe(MESSAGES.helper_invalid);
    }
  });

  it('restores a running new-app phase in the new reader while the actual old reader accepts its legacy view', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-new-update-rollback-'));
    roots.push(root);
    let release!: () => void;
    const blocked = new Promise<void>((resolve) => {
      release = resolve;
    });
    const service = new AppUpdateService({
      ccsDir: root,
      runHost: async (_host, control) => {
        for (const row of rows) control.onEvent({ event: 'result', result: row });
        control.onEvent({ event: 'app', appId: 'antigravity-acp', phase: 'downloading' });
        await blocked;
        return JSON.stringify({ results: rows });
      },
    });
    service.start();
    try {
      for (let i = 0; i < 50 && service.getStatus().job!.results.length < 60; i++)
        await new Promise((resolve) => setTimeout(resolve, 0));
      const next = new AppUpdateService({ ccsDir: root }).getStatus().job!;
      expect(next.expectedResults).toBe(60);
      expect(next.results).toHaveLength(60);
      expect(next.hosts!.mac.currentApp).toBe('antigravity-acp');
      const old = new LegacyAppUpdateReader(root).getStatus().job!;
      expect(old.expectedResults).toBe(40);
      expect(old.results).toHaveLength(40);
      expect(
        Object.values(old.hosts!).every((host) => host.currentApp === null && host.phase === null)
      ).toBe(true);
      expect(old.results.every((row) => LEGACY_STORAGE_APPS.includes(row.appId))).toBe(true);
      expect(
        fs.readFileSync(path.join(root, 'app-updates/dashboard-job.json'), 'utf8')
      ).not.toContain('antigravity-acp');
    } finally {
      release();
      for (let i = 0; i < 50 && service.getStatus().job!.state === 'running'; i++)
        await new Promise((resolve) => setTimeout(resolve, 0));
    }
    expect(service.getStatus().job!.state).toBe('completed');
    expect(new LegacyAppUpdateReader(root).getStatus().job!.results).toHaveLength(40);
  });
});
