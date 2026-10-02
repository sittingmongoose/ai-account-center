import { afterEach, beforeEach, expect, test, mock, spyOn } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import * as transport from '../../../src/web-server/services/claude-desktop-transport';
import * as storage from '../../../src/services/logging/log-storage';
import { openClaudeDesktopProfile } from '../../../src/web-server/services/claude-desktop-open-service';
import { ClaudeOpenOperations } from '../../../src/web-server/services/claude-open-operations';
import { synchronizeClaudeHistoryBeforeOpen } from '../../../src/web-server/services/claude-history-sync-service';
import { listClaudeDesktopProfiles } from '../../../src/web-server/services/claude-desktop-profile-service';

const fx = require('./synthetic-history-fixtures.cjs');
const core = require('../../../scripts/claude-history/history-index-sync.cjs');
const seed = fx.bindings('M');
const privateCanary = 'SYNTHETIC_PRIVATE_LOST_REPLY';
let directory: string, priorDir: string | undefined;
let count = 3,
  opened = 0,
  appends = 0;
let failure: 'none' | 'lost' | 'clean' | 'target-drift' = 'none';
let targetRecords: Array<{ name: string; bytes: Buffer; sha256: string }> = [];
let historyCopies: Array<[number | null, number | null]> = [];
let operations: ClaudeOpenOperations;

const snapshot = (platform: 'mac' | 'windows') => {
  const records =
    platform === 'windows'
      ? Array.from({ length: count }, (_, index) =>
          fx.envelope(fx.record(seed, 'windows', {}, index + 1))
        )
      : targetRecords;
  return {
    profileId: 'gmail',
    platform,
    identity: {
      accountSha256: seed.profiles.gmail.accountSha256,
      orgSha256: seed.profiles.gmail.orgSha256,
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

beforeEach(() => {
  directory = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-microbatch-progress-'));
  fs.chmodSync(directory, 0o700);
  priorDir = process.env.CCS_DIR;
  process.env.CCS_DIR = directory;
  count = 3;
  opened = appends = 0;
  failure = 'none';
  targetRecords = [];
  historyCopies = [];
  operations = new ClaudeOpenOperations();
  const policy = {
    version: 1,
    enabled: true,
    sourcePlatform: 'windows',
    identity: {
      accountUuid: seed.profiles.gmail.accountUuid,
      organizationUuid: seed.profiles.gmail.organizationUuid,
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
          id: 'gmail',
          email: 'synthetic@example.invalid',
          historySync: policy,
          mac: {
            launcherName: 'Synthetic.app',
            launcherPath: '/synthetic/Synthetic.app',
            profilePath: '/synthetic/profile',
            sshHost: 'synthetic-mac',
          },
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
  spyOn(storage, 'appendStructuredLogEntry').mockImplementation(() => {});
  spyOn(transport, 'openClaudeMacLauncher').mockImplementation(async () => {
    opened++;
  });
  spyOn(transport, 'openClaudeWindowsLauncher').mockImplementation(async () => {
    throw Error('unexpected platform');
  });
  spyOn(transport, 'runClaudeHistoryHelper').mockImplementation(
    async (_launcher, platform, _profile, request) => {
      let result: unknown;
      if (request.mode === 'closed-check') result = { closed: true };
      else if (request.mode === 'collect') result = snapshot(platform);
      else if (request.mode === 'verify-transcripts') result = { verified: true };
      else if (request.mode === 'protected-check') result = { unchanged: true };
      else if (request.mode === 'append') {
        appends++;
        const current = operations.forProfile(directory, 'gmail');
        historyCopies.push([current?.totalCount ?? null, current?.confirmedCount ?? null]);
        const records = request.records as Array<{ name: string; base64: string; sha256: string }>;
        expect(records).toHaveLength(1);
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
        }
      } else throw Error('unexpected mode');
      return Buffer.from(JSON.stringify(result));
    }
  );
});

afterEach(() => {
  mock.restore();
  if (priorDir === undefined) delete process.env.CCS_DIR;
  else process.env.CCS_DIR = priorDir;
  fs.rmSync(directory, { recursive: true, force: true });
});

async function runOpen() {
  operations.start(directory, 'gmail', 'mac', (observer) =>
    openClaudeDesktopProfile('gmail', 'mac', observer)
  );
  const outcome = await operations.settled(directory, 'gmail', 'mac');
  return { outcome, final: operations.forProfile(directory, 'gmail') };
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
  });

test('clean partial refusal reports one confirmed record then ordinary-Opens once', async () => {
  failure = 'clean';
  const { outcome, final } = await runOpen();
  expect(outcome?.ok).toBe(true);
  expect(opened).toBe(1);
  expect(appends).toBe(2);
  expect(final).toMatchObject({ state: 'opened', confirmedCount: 1, totalCount: 3 });
  expect(core.pendingMarkerState(directory, 'gmail', 'mac').held).toBe(false);
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
});

test('post-confirmation target drift reports partial count without authorizing another append', async () => {
  failure = 'target-drift';
  const { outcome, final } = await runOpen();
  expect(outcome?.ok).toBe(true);
  expect(opened).toBe(1);
  expect(appends).toBe(1);
  expect(final).toMatchObject({ state: 'opened', confirmedCount: 1, totalCount: 3 });
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
