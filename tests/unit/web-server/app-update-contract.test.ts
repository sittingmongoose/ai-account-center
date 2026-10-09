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

  it('accepts store_no_newer only as a current Windows row for Codex', () => {
    const noNewer = {
      appId: 'codex-desktop',
      status: 'current',
      messageCode: 'store_no_newer',
      previousVersion: '26.1002.7124.0',
      version: '26.1002.7124.0',
      manager: 'msix',
      updateAttempted: false,
      restartedProcesses: 0,
    };
    expect(normalizeAppUpdateRow(noNewer, 'codex-desktop', 'windows')).toMatchObject({
      status: 'current',
      manager: 'msix',
      message: MESSAGES.store_no_newer,
    });
    // Only Codex on Windows names the Store check: every other computer and app is refused.
    for (const platform of PLATFORMS.filter((value) => value !== 'windows'))
      expect(normalizeAppUpdateRow(noNewer, 'codex-desktop', platform).message).toBe(
        MESSAGES.helper_invalid
      );
    expect(normalizeAppUpdateRow(noNewer, 'claude-desktop', 'windows').message).toBe(
      MESSAGES.helper_invalid
    );
    for (const status of ['failed', 'action_required', 'restart_failed', 'unknown'])
      expect(
        normalizeAppUpdateRow({ ...noNewer, status }, 'codex-desktop', 'windows').message
      ).toBe(MESSAGES.helper_invalid);
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

  it('marks a skipped adapter part inUse only for a strict true, and keeps the flag on any status', () => {
    const adapters = (status: string, messageCode: string, parts: unknown) =>
      normalizeAppUpdateRow(
        {
          appId: 't3-acp-adapters',
          status,
          messageCode,
          previousVersion: null,
          version: null,
          manager: 'npm',
          updateAttempted: true,
          restartedProcesses: 0,
          parts,
        },
        't3-acp-adapters',
        'windows'
      );
    const held = { name: 'muse-acp', previousVersion: '0.10.0', version: '0.10.0', inUse: true };
    const moved = { name: 'zcode-acp-server', previousVersion: '0.65.0', version: '0.65.1' };
    // Only a boolean true keeps the flag; any other value is dropped with the rest of the part.
    const loose = adapters('action_required', 'in_use', [
      { ...held, inUse: 'true' },
      { ...moved, inUse: 1 },
    ]);
    expect(loose.parts).toEqual([
      { name: 'muse-acp', previousVersion: '0.10.0', version: '0.10.0' },
      { name: 'zcode-acp-server', previousVersion: '0.65.0', version: '0.65.1' },
    ]);
    expect(loose.parts!.some((part) => 'inUse' in part)).toBe(false);
    const named = adapters('action_required', 'in_use', [held, moved]);
    expect(named.parts).toEqual([held, moved]);
    expect(named.status).toBe('action_required');
    expect(named.message).toBe(
      'muse-acp is in use (open, or running in a T3 session), so it stayed at its installed version; zcode-acp-server updated; run Update apps again once it is closed.'
    );
    // A held flag on another status is kept as data; the row's own words are its failure words.
    const failed = adapters('failed', 'update_failed', [held]);
    expect(failed.parts).toEqual([held]);
    expect(failed.message).toBe(MESSAGES.update_failed);
  });

  it('names each held adapter in the in-use words, so no row claims nothing changed when one did', () => {
    const adapters = (parts: unknown[]) =>
      normalizeAppUpdateRow(
        {
          appId: 't3-acp-adapters',
          status: 'action_required',
          messageCode: 'in_use',
          previousVersion: null,
          version: null,
          manager: 'npm',
          updateAttempted: true,
          restartedProcesses: 0,
          parts,
        },
        't3-acp-adapters',
        'windows'
      ).message;
    const held = { name: 'muse-acp', previousVersion: '0.10.0', version: '0.10.0', inUse: true };
    const still = { name: 'zcode-acp-server', previousVersion: '0.65.1', version: '0.65.1' };
    const moved = { name: 'zcode-acp-server', previousVersion: '0.65.0', version: '0.65.1' };
    const heldZcode = {
      name: 'zcode-acp-server',
      previousVersion: '0.65.1',
      version: '0.65.1',
      inUse: true,
    };
    expect(adapters([held, still])).toBe(
      'muse-acp is in use (open, or running in a T3 session), so it stayed at its installed version; run Update apps again once it is closed.'
    );
    expect(adapters([held, moved])).toBe(
      'muse-acp is in use (open, or running in a T3 session), so it stayed at its installed version; zcode-acp-server updated; run Update apps again once it is closed.'
    );
    expect(adapters([held, heldZcode])).toBe(
      'muse-acp and zcode-acp-server are in use (open, or running in T3 sessions), so they stayed at their installed versions; run Update apps again once they are closed.'
    );
    // Without a held part the words stay generic, as rows saved before parts held read back.
    expect(adapters([])).toBe(
      'T3 ACP adapters is in use (open, or running in a T3 session), so nothing changed; run Update apps again once it is closed.'
    );
    // The generic in-use words stay for every other app, which only ever holds the whole app.
    expect(messageFor('in_use', 'zcode')).toBe(
      'ZCode is in use (open, or running in a T3 session), so nothing changed; run Update apps again once it is closed.'
    );
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

  it('says a CLI update left running T3 sessions alone, only on an updated CLI row', () => {
    const kept = {
      status: 'updated',
      messageCode: 't3_sessions_kept',
      previousVersion: '2.1.294',
      version: '2.1.295',
      manager: 'native',
      updateAttempted: true,
      restartedProcesses: 1,
      restartTargets: [
        { kind: 'tmux', server: 'ccs-updates-0123456789ab', session: 'ccs-updated-claude-code-1' },
      ],
    };
    const cli: UpdateAppId[] = ['antigravity-cli', 'muse-code', 'omp', 'codex-cli', 'claude-code'];
    for (const platform of PLATFORMS)
      for (const appId of cli) {
        const row = normalizeAppUpdateRow({ ...kept, appId }, appId, platform);
        expect(row).toMatchObject({ status: 'updated', version: '2.1.295', restartedProcesses: 1 });
        expect(row.message).toBe(MESSAGES.t3_sessions_kept);
        expect(row.message).toContain('keep the previous version until T3 starts them again');
      }
    // A failed, current or in-use row never claims it updated around T3 sessions.
    for (const status of ['current', 'failed', 'restart_failed', 'action_required', 'skipped'])
      expect(
        normalizeAppUpdateRow({ ...kept, appId: 'claude-code', status }, 'claude-code', 'ubuntu')
          .message
      ).toBe(MESSAGES.helper_invalid);
    // Desktop apps, ZCode, the adapters and T3 itself have their own flows and words.
    for (const appId of [
      'codex-desktop',
      'claude-desktop',
      'zcode',
      't3-acp-adapters',
      't3-code',
    ] as const)
      expect(normalizeAppUpdateRow({ ...kept, appId }, appId, 'mac').message).toBe(
        MESSAGES.helper_invalid
      );
  });

  it('says a CLI update left background sessions running; older T3-only rows still restore', () => {
    const row = (messageCode: string, appId: UpdateAppId = 'claude-code', status = 'updated') =>
      normalizeAppUpdateRow(
        {
          appId,
          status,
          messageCode,
          previousVersion: '2.1.294',
          version: '2.1.295',
          manager: 'native',
          updateAttempted: true,
          restartedProcesses: 0,
        },
        appId,
        'windows'
      );
    const kept = row('background_sessions_kept');
    expect(kept).toMatchObject({ status: 'updated', version: '2.1.295', restartedProcesses: 0 });
    expect(kept.message).toBe(MESSAGES.background_sessions_kept);
    expect(kept.message).toContain('Background sessions (T3 and other runs without a terminal)');
    expect(kept.message).toContain('keep the previous version until they are started again');
    // Only an updated CLI row may say so, exactly as for the older T3-only code.
    for (const status of ['current', 'failed', 'restart_failed', 'action_required', 'skipped'])
      expect(row('background_sessions_kept', 'claude-code', status).message).toBe(
        MESSAGES.helper_invalid
      );
    for (const appId of ['codex-desktop', 'zcode', 't3-acp-adapters', 't3-code'] as const)
      expect(row('background_sessions_kept', appId).message).toBe(MESSAGES.helper_invalid);
    // A job saved before the generalisation keeps its own words; both restore as themselves.
    const legacy = row('t3_sessions_kept');
    expect(legacy.message).toBe(MESSAGES.t3_sessions_kept);
    const root = directory();
    saveJob(root, { results: [kept, legacy], expectedResults: 40 });
    const job = new AppUpdateService({ ccsDir: root, runHost: async () => payload() }).getStatus()
      .job!;
    expect(job.results).toEqual([kept, legacy]);
  });

  it('names the CLI when a T3 session keeps Windows npm Codex from updating', () => {
    const row = normalizeAppUpdateRow(
      {
        appId: 'codex-cli',
        status: 'action_required',
        messageCode: 'in_use',
        previousVersion: '0.153.4',
        version: '0.153.4',
        manager: 'npm',
        updateAttempted: false,
        restartedProcesses: 0,
      },
      'codex-cli',
      'windows'
    );
    expect(row).toMatchObject({
      status: 'action_required',
      updateAttempted: false,
      forcedStops: 0,
    });
    expect(row.message).toBe(messageFor('in_use', 'codex-cli'));
    expect(row.message).toContain('Codex CLI is in use (open, or running in a T3 session)');
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

describe('Update apps contract for held ACP adapter parts', () => {
  it('restores an adapters row with a held part from disk with the same words and flag', () => {
    const root = directory();
    const held = normalizeAppUpdateRow(
      {
        appId: 't3-acp-adapters',
        status: 'action_required',
        messageCode: 'in_use',
        previousVersion: null,
        version: null,
        manager: 'npm',
        updateAttempted: true,
        restartedProcesses: 0,
        parts: [
          { name: 'muse-acp', previousVersion: '0.10.0', version: '0.10.0', inUse: true },
          { name: 'zcode-acp-server', previousVersion: '0.65.0', version: '0.65.1' },
        ],
      },
      't3-acp-adapters',
      'windows'
    );
    expect(held.message).toContain('muse-acp is in use');
    saveJob(root, { results: [held], expectedResults: 40 });
    const job = new AppUpdateService({ ccsDir: root, runHost: async () => payload() }).getStatus()
      .job!;
    // Had the saved words not mapped back, the row would restore as helper_invalid.
    expect(job.results).toEqual([held]);
    expect(job.results[0]!.status).toBe('action_required');
    expect(job.results[0]!.parts![0]).toEqual({
      name: 'muse-acp',
      previousVersion: '0.10.0',
      version: '0.10.0',
      inUse: true,
    });
  });
});
