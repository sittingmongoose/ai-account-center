import { afterEach, beforeEach, expect, test, mock, spyOn, setSystemTime } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import * as transport from '../../../src/web-server/services/claude-desktop-transport';
import * as storage from '../../../src/services/logging/log-storage';
import { openClaudeDesktopProfile } from '../../../src/web-server/services/claude-desktop-open-service';
import { ClaudeOpenOperations } from '../../../src/web-server/services/claude-open-operations';
import { synchronizeClaudeHistoryBeforeOpen } from '../../../src/web-server/services/claude-history-sync-service';
import { listClaudeDesktopProfiles } from '../../../src/web-server/services/claude-desktop-profile-service';

// Synthetic offline fixtures only: the transport, launchers and remote helper
// are mocked, and every path is a disposable temporary CCS directory.
const fx = require('./synthetic-history-fixtures.cjs');
const core = require('../../../scripts/claude-history/history-index-sync.cjs');
const seed = fx.bindings('M');
const privateCanary = 'SYNTHETIC_PRIVATE_LOST_REPLY';

type Platform = 'mac' | 'windows';
interface Scenario {
  id: 'gmail' | 'platyr';
  source: Platform;
  target: Platform;
}
// The two accepted guarded directions: Gmail Windows to Mac, Platyr Mac to Windows.
const GMAIL: Scenario = { id: 'gmail', source: 'windows', target: 'mac' };
const PLATYR: Scenario = { id: 'platyr', source: 'mac', target: 'windows' };

let directory: string, priorDir: string | undefined;
let scenario: Scenario = GMAIL;
let count = 3,
  opened = 0,
  appends = 0,
  cleanups = 0;
let cleanupFailure = false;
let failure:
  | 'none'
  | 'lost'
  | 'clean'
  | 'target-drift'
  | 'collect-fails'
  | 'policy-changes'
  | 'policy-removed-early'
  | 'target-opens'
  | 'slow' = 'none';
let targetRecords: Array<{ name: string; bytes: Buffer; sha256: string }> = [];
let historyCopies: Array<[number | null, number | null]> = [];
let markerStatesDuringAppend: boolean[] = [];
let operations: ClaudeOpenOperations;
let logEvents: string[] = [];

const markerRoot = () => path.join(directory, 'claude-history-pending');
const markerNames = () => (fs.existsSync(markerRoot()) ? fs.readdirSync(markerRoot()) : []);

const snapshot = (platform: Platform) => {
  const records =
    platform === scenario.source
      ? Array.from({ length: count }, (_, index) =>
          fx.envelope(fx.record(seed, scenario.source, {}, index + 1))
        )
      : targetRecords;
  return {
    profileId: scenario.id,
    platform,
    identity: {
      accountSha256: seed.profiles[scenario.id].accountSha256,
      orgSha256: seed.profiles[scenario.id].orgSha256,
    },
    records: records.map((row) => ({
      name: row.name,
      sha256: row.sha256,
      base64: row.bytes.toString('base64'),
    })),
    revision: core.snapshotRevision(records),
    snapshotStable: true,
    endpoint: seed.endpoint,
    nativeGuard: {
      ...core.NATIVE[platform],
      warmGuardVerified: true,
      autoResumeGuardVerified: true,
    },
    noPendingInput: true,
    noScheduledWork: true,
    protectedSnapshotStable: true,
    protectedSnapshot: {
      stamp: failure === 'target-drift' && appends > 0 ? 'changed' : 'synthetic-bound',
    },
  };
};

function writeManifest(): void {
  const policy = {
    version: 1,
    enabled: true,
    sourcePlatform: scenario.source,
    identity: {
      accountUuid: seed.profiles[scenario.id].accountUuid,
      organizationUuid: seed.profiles[scenario.id].organizationUuid,
    },
    project: { cwd: seed.project, originCwd: seed.project, transcriptRoot: seed.transcriptRoot },
    ssh: Object.fromEntries(
      ['mac', 'windows'].map((platform) => [
        platform,
        { alias: seed.aliases[platform], ...seed.plainEndpoint },
      ])
    ),
  };
  fs.writeFileSync(
    path.join(directory, 'claude-desktop-profiles.json'),
    JSON.stringify({
      version: 1,
      profiles: [
        {
          id: scenario.id,
          email: 'synthetic@example.invalid',
          historySync: policy,
          mac: {
            launcherName: 'Synthetic.app',
            launcherPath: '/synthetic/Synthetic.app',
            profilePath: '/synthetic/profile',
            sshHost: 'synthetic-mac',
          },
          // The Windows launcher is the fixed scheduled task: no launcherPath.
          windows: {
            launcherName: 'Fixed synthetic task',
            profilePath: 'C:\\Synthetic\\profile',
            sshHost: 'synthetic-windows',
          },
        },
      ],
    }),
    { mode: 0o600 }
  );
}

/** Change the private policy in place, as an owner editing or removing it would. */
function rewritePolicy(change: (entry: Record<string, unknown>) => void): void {
  const filename = path.join(directory, 'claude-desktop-profiles.json');
  const manifest = JSON.parse(fs.readFileSync(filename, 'utf8'));
  change(manifest.profiles[0]);
  fs.writeFileSync(filename, JSON.stringify(manifest), { mode: 0o600 });
}

function finishedMarker(profileId: string, platform: Platform, ordinal: number): void {
  const nonce = ordinal.toString(16).padStart(32, '0');
  fs.writeFileSync(
    path.join(markerRoot(), `${profileId}-${platform}-${nonce}.json`),
    JSON.stringify({
      version: 1,
      profileId,
      targetPlatform: platform,
      nonce,
      state: 'finished',
      createdAt: '2026-10-02T00:00:00Z',
      finishedAt: '2026-10-02T00:00:01Z',
      terminalReceiptSha256: '0'.repeat(64),
    }),
    { mode: 0o600 }
  );
}

beforeEach(() => {
  directory = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-microbatch-progress-'));
  fs.chmodSync(directory, 0o700);
  priorDir = process.env.CCS_DIR;
  process.env.CCS_DIR = directory;
  scenario = GMAIL;
  count = 3;
  opened = appends = cleanups = 0;
  cleanupFailure = false;
  failure = 'none';
  targetRecords = [];
  historyCopies = [];
  markerStatesDuringAppend = [];
  logEvents = [];
  operations = new ClaudeOpenOperations();
  writeManifest();
  spyOn(storage, 'appendStructuredLogEntry').mockImplementation((entry) => {
    if (entry.source === 'web-server:claude-history') logEvents.push(entry.event);
  });
  spyOn(transport, 'openClaudeMacLauncher').mockImplementation(async () => {
    if (scenario.target !== 'mac') throw Error('unexpected platform');
    opened++;
  });
  spyOn(transport, 'openClaudeWindowsLauncher').mockImplementation(async () => {
    if (scenario.target !== 'windows') throw Error('unexpected platform');
    opened++;
  });
  spyOn(transport, 'runClaudeHistoryHelper').mockImplementation(
    async (_launcher, platform, _profile, request) => {
      let result: unknown;
      if (request.mode === 'closed-check')
        result = { closed: !(failure === 'target-opens' && appends > 0) };
      else if (request.mode === 'collect') {
        if (failure === 'collect-fails' && platform === scenario.target && appends > 0)
          throw Error(privateCanary);
        result = snapshot(platform);
      } else if (request.mode === 'verify-transcripts') {
        if (failure === 'policy-removed-early') rewritePolicy((entry) => delete entry.historySync);
        result = { verified: true };
      } else if (request.mode === 'protected-check') result = { unchanged: true };
      else if (request.mode === 'snapshot-cleanup') {
        if (cleanupFailure) throw Error('ssh lost');
        cleanups++;
        expect(Object.keys(request).sort()).toEqual([
          'expectedEmail',
          'mode',
          'platform',
          'policy',
          'profileId',
        ]);
        result = { kept: ['a', 'b', 'c'], deleted: ['old'], skipped: [] };
      } else if (request.mode === 'append') {
        appends++;
        expect(platform).toBe(scenario.target);
        // Progress counts stay in this process; the fixed helper's strict request
        // keys never carry them.
        expect(Object.keys(request).sort()).toEqual([
          'expectedEmail',
          'expectedTarget',
          'mode',
          'platform',
          'policy',
          'profileId',
          'records',
        ]);
        markerStatesDuringAppend.push(
          core.pendingMarkerState(directory, scenario.id, scenario.target).held
        );
        const current = operations.forProfile(directory, scenario.id);
        historyCopies.push([current?.totalCount ?? null, current?.confirmedCount ?? null]);
        const records = request.records as Array<{ name: string; base64: string; sha256: string }>;
        expect(records).toHaveLength(core.MICROBATCH_RECORDS);
        if (failure === 'lost' && appends === 2) throw Error(privateCanary);
        if (failure === 'clean' && appends === 2) {
          result = {
            status: 'refused',
            createdCount: 0,
            recoveryRequired: false,
            ownedFilesRolledBack: true,
            writerQuiescent: true,
          };
        } else {
          targetRecords.push(
            ...records.map((row) => ({
              name: row.name,
              bytes: Buffer.from(row.base64, 'base64'),
              sha256: row.sha256,
            }))
          );
          result = {
            status: 'created_metadata',
            createdCount: 1,
            protectedBytesUnchanged: true,
            writerQuiescent: true,
          };
          if (failure === 'policy-changes' && appends === 1)
            rewritePolicy((entry) => {
              const policy = entry.historySync as { project: { transcriptRoot: string } };
              policy.project.transcriptRoot = '/changed-user/.claude/projects';
            });
          // Each confirmed batch takes 20 s of (mocked) wall-clock time.
          if (failure === 'slow') setSystemTime(new Date(Date.now() + 20_000));
        }
      } else throw Error('unexpected mode');
      return Buffer.from(JSON.stringify(result));
    }
  );
});

afterEach(() => {
  setSystemTime();
  mock.restore();
  if (priorDir === undefined) delete process.env.CCS_DIR;
  else process.env.CCS_DIR = priorDir;
  fs.rmSync(directory, { recursive: true, force: true });
});

async function runOpen() {
  operations.start(directory, scenario.id, scenario.target, (observer) =>
    openClaudeDesktopProfile(scenario.id, scenario.target, observer)
  );
  const outcome = await operations.settled(directory, scenario.id, scenario.target);
  return { outcome, final: operations.forProfile(directory, scenario.id) };
}

for (const size of [18, 32])
  test(`Open observer preserves the full ${size} count across all guarded microbatches`, async () => {
    count = size;
    const { outcome, final } = await runOpen();
    expect(outcome?.ok).toBe(true);
    expect(appends).toBe(size);
    expect(opened).toBe(1);
    expect(historyCopies).toEqual(Array.from({ length: size }, (_, index) => [size, index]));
    expect(final).toMatchObject({ state: 'opened', confirmedCount: size, totalCount: size });
    expect(JSON.stringify(final)).not.toContain(seed.profiles.gmail.accountUuid);
    expect(JSON.stringify(final)).not.toContain(fx.PRIVATE_TITLE);
    // One durable marker for the whole Open, pending during every append.
    expect(markerStatesDuringAppend).toEqual(Array.from({ length: size }, () => true));
    expect(markerNames()).toHaveLength(1);
    expect(core.pendingMarkerState(directory, 'gmail', 'mac').held).toBe(false);
  });

test('Platyr Mac to Windows fixed-task profile copies in microbatches with progress', async () => {
  scenario = PLATYR;
  writeManifest();
  count = 5;
  const { outcome, final } = await runOpen();
  expect(outcome?.ok).toBe(true);
  expect(appends).toBe(5);
  expect(opened).toBe(1);
  expect(historyCopies).toEqual(Array.from({ length: 5 }, (_, index) => [5, index]));
  expect(final).toMatchObject({
    platform: 'windows',
    state: 'opened',
    confirmedCount: 5,
    totalCount: 5,
  });
  expect(markerNames()).toHaveLength(1);
  expect(markerNames()[0]).toStartWith('platyr-windows-');
  expect(core.pendingMarkerState(directory, 'platyr', 'windows').held).toBe(false);
});

test('a full copy sequence fits in the last free marker slot', async () => {
  // Before one marker served a whole Open, every one-record batch used its own
  // slot, so a 32-record copy stopped after one batch at this capacity.
  fs.mkdirSync(markerRoot(), { mode: 0o700 });
  for (let index = 1; index < core.MARKER_LIMIT; index++) finishedMarker('party', 'mac', index);
  count = 32;
  const { outcome, final } = await runOpen();
  expect(outcome?.ok).toBe(true);
  expect(appends).toBe(32);
  expect(opened).toBe(1);
  expect(final).toMatchObject({ state: 'opened', confirmedCount: 32, totalCount: 32 });
  expect(markerNames()).toHaveLength(core.MARKER_LIMIT);
});

test('a full marker store skips the optional copy and still Opens once', async () => {
  fs.mkdirSync(markerRoot(), { mode: 0o700 });
  for (let index = 1; index <= core.MARKER_LIMIT; index++) finishedMarker('party', 'mac', index);
  const { outcome, final } = await runOpen();
  expect(outcome?.ok).toBe(true);
  expect(appends).toBe(0);
  expect(opened).toBe(1);
  expect(final).toMatchObject({ state: 'opened', confirmedCount: null, totalCount: null });
});

test('clean partial refusal reports one confirmed record then ordinary-Opens once', async () => {
  failure = 'clean';
  const { outcome, final } = await runOpen();
  expect(outcome?.ok).toBe(true);
  expect(opened).toBe(1);
  expect(appends).toBe(2);
  expect(final).toMatchObject({ state: 'opened', confirmedCount: 1, totalCount: 3 });
  expect(core.pendingMarkerState(directory, 'gmail', 'mac').held).toBe(false);
  expect(markerNames()).toHaveLength(1);
});

test('lost second reply reports one confirmed record and blocks Open with durable hold', async () => {
  failure = 'lost';
  const { outcome, final } = await runOpen();
  expect(outcome?.ok).toBe(false);
  expect(opened).toBe(0);
  expect(appends).toBe(2);
  expect(final).toMatchObject({ state: 'blocked_uncertain', confirmedCount: 1, totalCount: 3 });
  expect(core.pendingMarkerState(directory, 'gmail', 'mac').held).toBe(true);
  expect(JSON.stringify(final)).not.toContain(privateCanary);
  expect(markerNames()).toHaveLength(1);
  // A second click cannot start another writer while the hold stands.
  const again = await runOpen();
  expect(again.outcome?.ok).toBe(false);
  expect(again.final).toMatchObject({ state: 'blocked_uncertain' });
  expect(appends).toBe(2);
  expect(opened).toBe(0);
});

test('post-confirmation target drift reports partial count without authorizing another append', async () => {
  failure = 'target-drift';
  const { outcome, final } = await runOpen();
  expect(outcome?.ok).toBe(true);
  expect(opened).toBe(1);
  expect(appends).toBe(1);
  expect(final).toMatchObject({ state: 'opened', confirmedCount: 1, totalCount: 3 });
  expect(core.pendingMarkerState(directory, 'gmail', 'mac').held).toBe(false);
});

test('a failed read between batches keeps the confirmed count, releases the hold and Opens', async () => {
  failure = 'collect-fails';
  const { outcome, final } = await runOpen();
  expect(outcome?.ok).toBe(true);
  expect(opened).toBe(1);
  expect(appends).toBe(1);
  expect(final).toMatchObject({ state: 'opened', confirmedCount: 1, totalCount: 3 });
  expect(core.pendingMarkerState(directory, 'gmail', 'mac').held).toBe(false);
  expect(JSON.stringify(final)).not.toContain(privateCanary);
});

test('legacy one-argument copying observer remains valid and cannot fail copy when it throws', async () => {
  const profile = (await listClaudeDesktopProfiles())[0];
  const totals: number[] = [];
  const result = await synchronizeClaudeHistoryBeforeOpen(profile, 'mac', {
    copying: (total) => {
      totals.push(total);
      throw Error(privateCanary);
    },
  });
  expect(result).toMatchObject({ status: 'synchronized', createdCount: 3 });
  expect(totals).toEqual([3, 3, 3]);
  expect(appends).toBe(3);
  expect(opened).toBe(0);
  expect(core.pendingMarkerState(directory, 'gmail', 'mac').held).toBe(false);
});

test('Open progress counts only move forward and stay within the plan', async () => {
  const store = new ClaudeOpenOperations();
  let release: () => void = () => {};
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  store.start(directory, 'gmail', 'mac', async (observer) => {
    observer.copying?.(5, 0);
    observer.copying?.(5, 2);
    observer.copying?.(5, 1); // backwards: ignored
    observer.copying?.(5, 9); // beyond the plan: ignored
    observer.synchronized?.({ status: 'refused', createdCount: 1 }); // backwards: ignored
    observer.synchronized?.({ status: 'skipped', createdCount: 4 }); // not a copy result
    await gate;
    observer.synchronized?.({ status: 'refused', createdCount: 3 });
  });
  expect(store.forProfile(directory, 'gmail')).toMatchObject({
    state: 'copying',
    confirmedCount: 2,
    totalCount: 5,
  });
  release();
  await store.settled(directory, 'gmail', 'mac');
  expect(store.forProfile(directory, 'gmail')).toMatchObject({
    state: 'opened',
    confirmedCount: 3,
    totalCount: 5,
  });
});

test('a policy change between batches stops cleanly: no hold, the confirmed count stays, Open proceeds', async () => {
  failure = 'policy-changes';
  const { outcome, final } = await runOpen();
  expect(outcome?.ok).toBe(true);
  expect(opened).toBe(1);
  expect(appends).toBe(1);
  expect(final).toMatchObject({ state: 'opened', confirmedCount: 1, totalCount: 3, message: null });
  expect(core.pendingMarkerState(directory, 'gmail', 'mac').held).toBe(false);
  expect(markerNames()).toHaveLength(1);
  expect(logEvents).toEqual(['claude.history.snapshots_retained', 'claude.history.copy_stopped']);
});

test('a policy change between batches returns a clean refusal with its own reason', async () => {
  failure = 'policy-changes';
  const profile = (await listClaudeDesktopProfiles())[0];
  const result = await synchronizeClaudeHistoryBeforeOpen(profile, 'mac');
  expect(result).toMatchObject({
    status: 'refused',
    reason: 'history_policy_changed',
    createdCount: 1,
    recoveryRequired: false,
  });
  expect(core.pendingMarkerState(directory, 'gmail', 'mac').held).toBe(false);
});

test('a policy removed before the first batch arms no marker and Opens normally', async () => {
  failure = 'policy-removed-early';
  const { outcome, final } = await runOpen();
  expect(outcome?.ok).toBe(true);
  expect(opened).toBe(1);
  expect(appends).toBe(0);
  expect(final).toMatchObject({ state: 'opened', confirmedCount: null, totalCount: null });
  expect(markerNames()).toHaveLength(0);
  expect(logEvents).toEqual(['claude.history.copy_stopped']);
});

test('the target opening between batches stops the copy, keeps the count and releases the hold', async () => {
  failure = 'target-opens';
  const profile = (await listClaudeDesktopProfiles())[0];
  const result = await synchronizeClaudeHistoryBeforeOpen(profile, 'mac');
  expect(result).toMatchObject({
    status: 'refused',
    reason: 'target_opened',
    createdCount: 1,
    recoveryRequired: false,
  });
  expect(appends).toBe(1);
  expect(core.pendingMarkerState(directory, 'gmail', 'mac').held).toBe(false);
});

test('the target opening between batches still Opens the profile once through the Open service', async () => {
  failure = 'target-opens';
  const { outcome, final } = await runOpen();
  expect(outcome?.ok).toBe(true);
  expect(opened).toBe(1);
  expect(appends).toBe(1);
  expect(final).toMatchObject({ state: 'opened', confirmedCount: 1, totalCount: 3 });
  expect(core.pendingMarkerState(directory, 'gmail', 'mac').held).toBe(false);
});

test('a full marker store has its own reason and a warning log line', async () => {
  fs.mkdirSync(markerRoot(), { mode: 0o700 });
  for (let index = 1; index <= core.MARKER_LIMIT; index++) finishedMarker('party', 'mac', index);
  const profile = (await listClaudeDesktopProfiles())[0];
  const result = await synchronizeClaudeHistoryBeforeOpen(profile, 'mac');
  expect(result).toMatchObject({
    status: 'refused',
    reason: 'history_marker_store_full',
    createdCount: 0,
    recoveryRequired: false,
  });
  expect(appends).toBe(0);
  expect(logEvents).toEqual(['claude.history.marker_store_full']);
  expect(markerNames()).toHaveLength(core.MARKER_LIMIT);
});

test('the per-Open time budget opens Claude part-way and the next Open copies the rest', async () => {
  failure = 'slow';
  count = 5;
  const first = await runOpen();
  expect(first.outcome?.ok).toBe(true);
  expect(opened).toBe(1);
  // 20 s per batch against the 45 s budget: the bound is reached after batch 3.
  expect(appends).toBe(3);
  expect(first.final).toMatchObject({ state: 'opened', confirmedCount: 3, totalCount: 5 });
  expect(core.pendingMarkerState(directory, 'gmail', 'mac').held).toBe(false);
  expect(logEvents).toEqual([
    'claude.history.snapshots_retained',
    'claude.history.copy_budget_reached',
  ]);
  // Later click (past the 1 s repeat-click window): only the missing records are planned.
  failure = 'none';
  setSystemTime(new Date(Date.now() + 5_000));
  const second = await runOpen();
  expect(second.outcome?.ok).toBe(true);
  expect(opened).toBe(2);
  expect(appends).toBe(5);
  expect(second.final).toMatchObject({ state: 'opened', confirmedCount: 2, totalCount: 2 });
  expect(markerNames()).toHaveLength(2);
  expect(core.pendingMarkerState(directory, 'gmail', 'mac').held).toBe(false);
});

test('retention runs once after each copy that created records, unless Settings turned it off', async () => {
  const { writeDashboardPreferences, defaultDashboardPreferences } = await import(
    '../../../src/web-server/services/dashboard-preferences'
  );
  const profile = (await listClaudeDesktopProfiles())[0];
  const result = await synchronizeClaudeHistoryBeforeOpen(profile, 'mac');
  expect(result).toMatchObject({ status: 'synchronized', createdCount: 3 });
  expect(cleanups).toBe(1);
  expect(logEvents).toContain('claude.history.snapshots_retained');
  writeDashboardPreferences(
    { ...defaultDashboardPreferences(), snapshotCleanup: { auto: false } },
    directory
  );
  failure = 'none';
  const second = await synchronizeClaudeHistoryBeforeOpen(profile, 'mac');
  expect(second.createdCount).toBe(0);
  expect(cleanups).toBe(1);
});

test('a failed retention never fails the copy', async () => {
  cleanupFailure = true;
  const profile = (await listClaudeDesktopProfiles())[0];
  const result = await synchronizeClaudeHistoryBeforeOpen(profile, 'mac');
  expect(result).toMatchObject({ status: 'synchronized', createdCount: 3 });
  expect(cleanups).toBe(0);
  expect(logEvents).toContain('claude.history.snapshots_retain_failed');
});
