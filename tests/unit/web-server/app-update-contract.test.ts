/**
 * The Update apps row contract for ZCode, the T3 ACP adapters and in-use rows.
 * Fixed rows and fake helpers only: nothing here reaches a real computer or app.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  AppUpdateService,
  normalizeAppUpdateResults,
  UPDATE_APP_LABELS,
  type UpdateAppId,
} from '../../../src/web-server/services/app-update-service';
import {
  EXPECTED_RESULTS,
  MESSAGES,
  messageFor,
  normalizeAppUpdateRow,
  PLATFORMS,
  rowMessage,
} from '../../../src/web-server/services/app-update-contract';

const directories: string[] = [];
afterEach(() => {
  for (const value of directories.splice(0)) fs.rmSync(value, { recursive: true, force: true });
});
function directory() {
  const value = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-updater-contract-'));
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
async function finish(service: AppUpdateService) {
  for (let i = 0; i < 30 && service.getStatus().job?.state === 'running'; i++)
    await new Promise((resolve) => setTimeout(resolve, 0));
}
/** A saved job file; `hosts: null` is what a job saved before parallel hosts looks like. */
function saveJob(root: string, job: { results: unknown[]; expectedResults: number | null }) {
  const saved = path.join(root, 'app-updates');
  fs.mkdirSync(saved, { recursive: true });
  fs.writeFileSync(
    path.join(saved, 'dashboard-job.json'),
    JSON.stringify({
      job: {
        id: '11111111-1111-4111-8111-111111111111',
        state: 'completed',
        startedAt: '2026-10-07T00:00:00.000Z',
        finishedAt: '2026-10-07T00:05:00.000Z',
        activePlatform: null,
        hosts: null,
        cancelRequested: false,
        ...job,
      },
    })
  );
}
const IN_USE_WORDS =
  'is in use (open, or running in a T3 session), so nothing changed; run Update apps again once it is closed.';

describe('Update apps contract for ZCode, T3 ACP adapters and in-use rows', () => {
  it('lists ten apps in a fixed order and expects forty rows across four computers', () => {
    expect(Object.keys(UPDATE_APP_LABELS)).toEqual([
      'antigravity-cli',
      'muse-code',
      'omp',
      'codex-cli',
      'codex-desktop',
      'claude-code',
      'claude-desktop',
      'zcode',
      't3-acp-adapters',
      't3-code',
    ]);
    expect(UPDATE_APP_LABELS.zcode).toBe('ZCode');
    expect(UPDATE_APP_LABELS['t3-acp-adapters']).toBe('T3 ACP adapters');
    expect(PLATFORMS).toEqual(['ubuntu', 'mac', 'windows', 'nas1']);
    expect(EXPECTED_RESULTS).toBe(40);
    for (const platform of PLATFORMS) {
      const rows = normalizeAppUpdateResults(payload(), platform);
      expect(rows.map((row) => row.appId)).toEqual(Object.keys(UPDATE_APP_LABELS));
      expect(rows.every((row) => row.status === 'current')).toBe(true);
    }
  });

  it('accepts in-use only as action-required, and names the app that is open', () => {
    const inUse = {
      appId: 'zcode',
      status: 'action_required',
      messageCode: 'in_use',
      previousVersion: '0.65.1',
      version: '0.65.1',
      manager: 'official-download',
      updateAttempted: false,
      restartedProcesses: 0,
    };
    for (const platform of PLATFORMS)
      expect(normalizeAppUpdateRow(inUse, 'zcode', platform)).toMatchObject({
        status: 'action_required',
        message: `ZCode ${IN_USE_WORDS}`,
      });
    expect(messageFor('in_use', 'claude-code')).toBe(`Claude Code ${IN_USE_WORDS}`);
    for (const status of ['current', 'updated', 'failed', 'held', 'skipped', 'unknown'])
      expect(normalizeAppUpdateRow({ ...inUse, status }, 'zcode', 'windows').message).toBe(
        MESSAGES.helper_invalid
      );
  });

  it('keeps parts only on the adapters row, one per known name, with safe versions', () => {
    const adapters = {
      appId: 't3-acp-adapters',
      status: 'current',
      messageCode: 'current',
      previousVersion: null,
      version: null,
      manager: 'npm',
      updateAttempted: false,
      restartedProcesses: 0,
      parts: [
        { name: 'muse-acp', previousVersion: '0.10.0', version: '0.10.1', env: 'PRIVATE_SENTINEL' },
        { name: 'muse-acp', previousVersion: '9.9.9', version: '9.9.9' },
        { name: 'zcode-acp-server', previousVersion: '0.65.0', version: 'SECRET; rm -rf' },
        { name: 'zcode-acp-server', previousVersion: '0.65.1', version: '0.65.1' },
        { name: 'other-tool', previousVersion: '1.0.0', version: '1.0.1' },
        'PRIVATE_SENTINEL',
      ],
    };
    const row = normalizeAppUpdateRow(adapters, 't3-acp-adapters', 'mac');
    // Each name is kept once (its first entry), unknown names and extra keys are dropped.
    expect(row.parts).toEqual([
      { name: 'muse-acp', previousVersion: '0.10.0', version: '0.10.1' },
      { name: 'zcode-acp-server', previousVersion: '0.65.0', version: null },
    ]);
    expect(JSON.stringify(row)).not.toContain('PRIVATE_SENTINEL');
    expect(JSON.stringify(row)).not.toContain('SECRET');
    expect(
      normalizeAppUpdateRow({ ...adapters, parts: 'muse-acp' }, 't3-acp-adapters', 'mac').parts
    ).toEqual([]);
    // Only the adapters row carries parts; every other app drops them.
    for (const appId of Object.keys(UPDATE_APP_LABELS) as UpdateAppId[]) {
      if (appId === 't3-acp-adapters') continue;
      const other = normalizeAppUpdateRow({ ...adapters, appId }, appId, 'ubuntu');
      expect(other.status).toBe('current');
      expect('parts' in other).toBe(false);
    }
  });

  it('updates an adapters row only when a part moved, and never without one', () => {
    const update = (
      parts: unknown,
      appId: 't3-acp-adapters' | 'omp' = 't3-acp-adapters',
      version: string | null = null
    ) =>
      normalizeAppUpdateRow(
        {
          appId,
          status: 'updated',
          messageCode: 'updated',
          previousVersion: null,
          version,
          manager: 'npm',
          updateAttempted: true,
          restartedProcesses: 0,
          parts,
        },
        appId,
        'ubuntu'
      );
    const moved = { name: 'muse-acp', previousVersion: '0.10.0', version: '0.10.1' };
    expect(update([moved])).toMatchObject({
      status: 'updated',
      version: null,
      message: MESSAGES.updated,
      parts: [moved],
    });
    // A part with no earlier version counts as moved too.
    expect(
      update([{ name: 'zcode-acp-server', previousVersion: null, version: '0.65.1' }]).status
    ).toBe('updated');
    // Nothing moved, the moved version is unsafe, or there are no parts: refused, even with a
    // top-level version, since an adapters row never names one of its own.
    expect(update([{ ...moved, previousVersion: '0.10.1' }]).message).toBe(MESSAGES.helper_invalid);
    expect(update([{ ...moved, version: 'bad version' }]).message).toBe(MESSAGES.helper_invalid);
    expect(update([]).message).toBe(MESSAGES.helper_invalid);
    expect(update([], 't3-acp-adapters', '0.10.1').message).toBe(MESSAGES.helper_invalid);
    // Every other app still needs a top-level version, whatever its parts say.
    expect(update([moved], 'omp').message).toBe(MESSAGES.helper_invalid);
    expect(update([moved], 'omp', '0.10.1').status).toBe('updated');
  });

  it('restores a saved 32-row job from before ZCode and the adapters existed', async () => {
    const root = directory();
    const before = [
      'antigravity-cli',
      'muse-code',
      'omp',
      'codex-cli',
      'codex-desktop',
      'claude-code',
      'claude-desktop',
      't3-code',
    ];
    const rows = PLATFORMS.flatMap((platform) =>
      normalizeAppUpdateResults(payload(), platform).filter((row) => before.includes(row.appId))
    );
    expect(rows).toHaveLength(32);
    saveJob(root, { results: rows, expectedResults: 32 });
    const service = new AppUpdateService({ ccsDir: root, runHost: async () => payload() });
    const restored = service.getStatus().job!;
    expect(restored.state).toBe('completed');
    expect(restored.results).toHaveLength(32);
    expect(restored.expectedResults).toBe(32);
    // The old job keeps its own total; the next run covers the full forty rows.
    service.start();
    await finish(service);
    expect(service.getStatus().job!.results).toHaveLength(40);
    expect(service.getStatus().job!.expectedResults).toBe(40);
  });

  it('restores in-use and adapter-part rows from disk with their own words and parts', () => {
    const root = directory();
    const inUse = normalizeAppUpdateRow(
      {
        appId: 'zcode',
        status: 'action_required',
        messageCode: 'in_use',
        previousVersion: '0.65.1',
        version: '0.65.1',
        manager: 'official-download',
        updateAttempted: false,
        restartedProcesses: 0,
      },
      'zcode',
      'windows'
    );
    const adapters = normalizeAppUpdateRow(
      {
        appId: 't3-acp-adapters',
        status: 'updated',
        messageCode: 'updated',
        previousVersion: null,
        version: null,
        manager: 'npm',
        updateAttempted: true,
        restartedProcesses: 0,
        parts: [{ name: 'muse-acp', previousVersion: '0.10.0', version: '0.10.1' }],
      },
      't3-acp-adapters',
      'mac'
    );
    saveJob(root, { results: [inUse, adapters], expectedResults: 40 });
    const job = new AppUpdateService({ ccsDir: root, runHost: async () => payload() }).getStatus()
      .job!;
    expect(job.results).toEqual([inUse, adapters]);
  });
});

describe('stray copies beside a managed Codex or Claude install', () => {
  const sentence = (version: string, place = '/usr/local/bin', label = 'Codex CLI') =>
    ` An older ${label} copy (${version}) in ${place} comes first on some PATHs; remove it so it never runs instead.`;
  const row = (overrides: Record<string, unknown> = {}) => ({
    appId: 'codex-cli',
    status: 'current',
    messageCode: 'current',
    previousVersion: '0.162.0',
    version: '0.162.0',
    manager: 'native',
    updateAttempted: false,
    restartedProcesses: 0,
    ...overrides,
  });
  const copy = (version: string | null, shadows = true, location = 'usr-local') => ({
    location,
    version,
    shadows,
  });

  it('keeps only fixed stray fields and names an older shadowing copy in one sentence', () => {
    const value = normalizeAppUpdateRow(
      row({
        strays: [
          {
            location: 'usr-local',
            version: '0.145.0',
            shadows: true,
            path: '/usr/local/bin/codex',
          },
          { location: '/usr/local/bin', version: '0.145.0', shadows: true },
          { location: 'bun', version: 'latest', shadows: false },
          { location: 'other', version: '0.150.0', shadows: 'yes' },
        ],
      }),
      'codex-cli',
      'mac'
    );
    expect(value.strays).toEqual([
      { location: 'usr-local', version: '0.145.0', shadows: true },
      { location: 'bun', version: null, shadows: false },
    ]);
    expect(value.message).toBe(MESSAGES.current + sentence('0.145.0'));
    expect(JSON.stringify(value)).not.toContain('/usr/local/bin/codex');
  });

  it('caps strays at four and leaves a row without strays exactly as before', () => {
    const strays = Array.from({ length: 6 }, (_, index) => copy(`0.1${index}.0`, false, 'other'));
    expect(normalizeAppUpdateRow(row({ strays }), 'codex-cli', 'ubuntu').strays).toEqual(
      strays.slice(0, 4)
    );
    const plain = normalizeAppUpdateRow(row(), 'codex-cli', 'ubuntu');
    expect('strays' in plain).toBe(false);
    expect(plain.message).toBe(MESSAGES.current);
  });

  it('names a copy only when it shadows the managed install and is older than the row version', () => {
    const message = (value: Record<string, unknown>) =>
      normalizeAppUpdateRow(value, 'codex-cli', 'mac').message;
    // A newer copy is listed but never called older.
    expect(message(row({ strays: [copy('0.170.0')] }))).toBe(MESSAGES.current);
    // Behind the managed install on PATH: not shadowing.
    expect(message(row({ strays: [copy('0.145.0', false)] }))).toBe(MESSAGES.current);
    // Without a version it cannot be called older.
    expect(message(row({ strays: [copy(null)] }))).toBe(MESSAGES.current);
    expect(message(row({ strays: [copy('0.162.0')] }))).toBe(MESSAGES.current);
    // Numbers compare as numbers: 0.9.0 is older than 0.10.0.
    expect(
      message(row({ strays: [copy('0.9.0')], previousVersion: '0.10.0', version: '0.10.0' }))
    ).toBe(MESSAGES.current + sentence('0.9.0'));
    // Words without a full stop get one before the sentence: a cancelled row reads "Skipped: cancelled."
    expect(
      message(
        row({ strays: [copy('0.145.0')], status: 'skipped', messageCode: 'skipped_cancelled' })
      )
    ).toBe(MESSAGES.skipped_cancelled + '.' + sentence('0.145.0'));
    // A failed row with no new version is compared against the version it started from.
    expect(
      message(
        row({
          strays: [copy('0.145.0')],
          status: 'failed',
          messageCode: 'update_failed',
          version: null,
        })
      )
    ).toBe(MESSAGES.update_failed + sentence('0.145.0'));
  });

  it('names a Claude Code copy with its own label and a fixed place', () => {
    const value = normalizeAppUpdateRow(
      row({
        appId: 'claude-code',
        version: '2.1.295',
        previousVersion: '2.1.295',
        strays: [copy('2.1.0', true, 'homebrew')],
      }),
      'claude-code',
      'mac'
    );
    expect(value.message).toBe(
      MESSAGES.current + sentence('2.1.0', 'the Homebrew bin folder', 'Claude Code')
    );
  });

  it('never reads strays from another app row', () => {
    const value = normalizeAppUpdateRow(
      row({ appId: 'omp', strays: [copy('0.145.0')] }),
      'omp',
      'mac'
    );
    expect('strays' in value).toBe(false);
    expect(value.message).toBe(MESSAGES.current);
    expect(messageFor('current', 'codex-cli')).toBe(MESSAGES.current);
  });

  it('restores a saved Codex row with its strays and the same words from disk', () => {
    const root = directory();
    const saved = normalizeAppUpdateRow(row({ strays: [copy('0.145.0')] }), 'codex-cli', 'mac');
    expect(rowMessage(JSON.parse(JSON.stringify(saved)), 'codex-cli', 'current')).toBe(
      saved.message
    );
    saveJob(root, { results: [saved], expectedResults: 40 });
    const job = new AppUpdateService({ ccsDir: root, runHost: async () => payload() }).getStatus()
      .job!;
    expect(job.results).toEqual([saved]);
  });
});
