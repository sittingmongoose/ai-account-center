/**
 * G13 Claude Open + history-sync parity table. Generates `claude-open-table.json` from the
 * REAL TypeScript behaviour: the pinned copy core (`history-index-sync.cjs`), the Open
 * progress store (`claude-open-operations.ts`), the private policy parse
 * (`claude-history-sync-service.ts`) and the Windows account-list rule
 * (`claude-desktop-open-service.ts`). The Rust port's table test compares against it.
 *
 *   bun tests/spec/agent/claude-open-table.ts          # rewrite the checked-in table
 *   bun tests/spec/agent/claude-open-table.ts --check  # fail when it drifted
 *
 * Every value is invented for offline fixtures: no profile, credential, provider call,
 * remote computer or real app is touched. The copy sequences run scripted adapters only.
 */
import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';
import { createRequire } from 'module';
import { createHash } from 'crypto';
import { ClaudeOpenOperations } from '../../../src/web-server/services/claude-open-operations';
import {
  ClaudeHistoryOpenHeldError,
  claudeWindowsLauncherAccountList,
  type ClaudeOpenObserver,
} from '../../../src/web-server/services/claude-desktop-open-service';
import { parseClaudeHistoryPolicy } from '../../../src/web-server/services/claude-history-sync-service';
import { ClaudeDesktopTransportError } from '../../../src/web-server/services/claude-desktop-transport';
import { ConfigError, ProfileError } from '../../../src/errors/error-types';
import type { ClaudeDesktopProfile } from '../../../src/web-server/services/claude-desktop-profile-service';
import type { PendingClaudeProfile } from '../../../src/web-server/services/claude-account-stores';

interface ByteRow {
  name: string;
  bytes: Buffer;
  sha256: string;
}
interface WireRow {
  name: string;
  bytes: string;
  sha256: string;
}
type SnapshotWire = Record<string, unknown> & { records: WireRow[] };
type SnapshotBytes = Record<string, unknown> & { records: ByteRow[] };
interface SequenceAdapters {
  isTargetClosed(): Promise<boolean>;
  readTarget(): Promise<SnapshotBytes>;
  readSource(): Promise<SnapshotBytes>;
  verifyTranscriptReferences(): Promise<boolean>;
  targetProtectedStateUnchanged(): Promise<boolean>;
  appendCreateOnly(payload: { records: { name: string; bytes: Buffer }[] }): Promise<unknown>;
}
/** The pinned core is an untyped .cjs library; this is its checked boundary. */
interface Core {
  NATIVE: Record<string, { version: string; managerSha256: string }>;
  snapshotRevision(rows: ByteRow[]): string;
  neutral(
    source: Record<string, unknown>,
    sourcePlatform: string,
    targetPlatform: string,
    policy: unknown
  ): { descriptor?: Record<string, unknown>; reason?: string };
  planMissing(
    source: Record<string, unknown>[],
    target: Record<string, unknown>[],
    sourcePlatform: string,
    targetPlatform: string,
    policy: unknown
  ): { records: ByteRow[]; alreadyPresentCount: number; skipped: Record<string, number> };
  decodeRecords(snapshot: { records: ByteRow[] }): Record<string, unknown>[];
  synchronizeBeforeProfileOpen(request: {
    profileId: string;
    targetPlatform: string;
    policy: unknown;
    adapters: SequenceAdapters;
    limits?: { budgetMs?: number; maxBatches?: number; now?: () => number };
  }): Promise<Record<string, unknown>>;
}
const require = createRequire(import.meta.url);
// Untyped packaged library: the Core interface above is the reviewed contract.
const core = require('../../../scripts/claude-history/history-index-sync.cjs') as Core;

const sha = (value: string | Buffer): string =>
  createHash('sha256').update(value).digest('hex');
const uuid = (n: number): string =>
  '00000000-0000-4000-8000-' + n.toString(16).padStart(12, '0');
const hex = (bytes: Buffer): string => bytes.toString('hex');
const unhex = (text: string): Buffer => Buffer.from(text, 'hex');

// One synthetic binding set, as tests/unit/claude-history/synthetic-history-fixtures.cjs builds.
const seed = {
  accountUuid: uuid(1065),
  organizationUuid: uuid(2065),
  hostname: 'synthetic-host-A',
  username: 'synthetic-user-A',
  port: 2265,
  aliases: { mac: 'synthetic-mac-alias-A', windows: 'synthetic-windows-alias-A' },
  project: '/mnt/Cursor/PuppetMaster',
  transcriptRoot: '/synthetic-user/.claude/projects',
};
const policy = {
  version: 1,
  enabled: true,
  sourcePlatform: 'windows',
  identity: { accountUuid: seed.accountUuid, organizationUuid: seed.organizationUuid },
  project: { cwd: seed.project, originCwd: seed.project, transcriptRoot: seed.transcriptRoot },
  ssh: Object.fromEntries(
    ['mac', 'windows'].map((p) => [
      p,
      {
        alias: seed.aliases[p as 'mac' | 'windows'],
        hostname: seed.hostname,
        username: seed.username,
        port: seed.port,
      },
    ])
  ),
};
const endpoint = {
  hostnameSha256: sha(seed.hostname),
  usernameSha256: sha(seed.username),
  portSha256: sha(String(seed.port)),
};
const identity = {
  accountSha256: sha(seed.accountUuid),
  orgSha256: sha(seed.organizationUuid),
};

function record(
  ordinal: number,
  platform: 'mac' | 'windows',
  changes: Record<string, unknown> = {}
): Record<string, unknown> {
  const desktopId = uuid(ordinal * 2 - 1);
  const cliId = uuid(ordinal * 2);
  return {
    sessionId: 'local_' + desktopId,
    cliSessionId: cliId,
    title: 'SYNTHETIC_PRIVATE_HISTORY_TITLE',
    cwd: seed.project,
    originCwd: seed.project,
    sshConfig: { sshHost: seed.aliases[platform] },
    sshRemoteTranscriptPath: `${seed.transcriptRoot}/-synthetic-project-A/${cliId}.jsonl`,
    createdAt: 1,
    lastActivityAt: 2,
    isArchived: false,
    ...changes,
  };
}
function envelope(value: Record<string, unknown>): ByteRow {
  const bytes = Buffer.from(JSON.stringify(value));
  return { name: (value.sessionId as string) + '.json', bytes, sha256: sha(bytes) };
}
function wire(row: ByteRow): WireRow {
  return { name: row.name, bytes: hex(row.bytes), sha256: row.sha256 };
}
function snapshot(
  platform: 'mac' | 'windows',
  rows: ByteRow[],
  changes: Record<string, unknown> = {}
): SnapshotWire {
  return {
    profileId: 'gmail',
    platform,
    identity,
    endpoint,
    records: rows.map(wire),
    snapshotStable: true,
    revision: core.snapshotRevision(rows),
    nativeGuard: {
      ...core.NATIVE[platform],
      warmGuardVerified: true,
      autoResumeGuardVerified: true,
    },
    noPendingInput: true,
    noScheduledWork: true,
    protectedSnapshotStable: true,
    protectedSnapshot: { files: [{ relative: 'config.json', sha256: sha('synthetic-protected') }] },
    ...changes,
  };
}
const sourceRows = (count: number, start = 1): ByteRow[] =>
  Array.from({ length: count }, (_, i) => envelope(record(start + i, 'windows')));
const targetRows = (count: number, start = 1000): ByteRow[] =>
  Array.from({ length: count }, (_, i) => envelope(record(start + i, 'mac')));
const toBuffers = (snap: SnapshotWire): SnapshotBytes => ({
  ...snap,
  records: snap.records.map((row) => ({ name: row.name, bytes: unhex(row.bytes), sha256: row.sha256 })),
});
const decodeRows = (rows: WireRow[]): Record<string, unknown>[] =>
  rows.map((row) => JSON.parse(unhex(row.bytes).toString()) as Record<string, unknown>);

// --- copy sequences over scripted adapters -------------------------------------------------
interface SequenceCase {
  name: string;
  profileId?: string;
  targetPlatform?: string;
  policy?: unknown;
  limits?: { budgetMs?: number; maxBatches?: number; clockStep?: number };
  closed?: boolean[];
  target?: SnapshotWire[];
  source?: SnapshotWire[];
  verify?: boolean;
  protected?: boolean[];
  append?: ({ receipt: unknown } | { throw: true })[];
}
const created = {
  status: 'created_metadata',
  createdCount: 1,
  protectedBytesUnchanged: true,
  writerQuiescent: true,
};
const sequences: SequenceCase[] = [
  {
    name: 'synchronized-4',
    target: [snapshot('mac', [])],
    source: [snapshot('windows', sourceRows(4))],
    append: [{ receipt: created }, { receipt: created }, { receipt: created }, { receipt: created }],
  },
  {
    name: 'unchanged-all-present',
    target: [snapshot('mac', targetRows(4))],
    source: [
      snapshot(
        'windows',
        targetRows(4).map((row) =>
          envelope({
            ...(JSON.parse(row.bytes.toString()) as Record<string, unknown>),
            sshConfig: { sshHost: seed.aliases.windows },
          })
        )
      ),
    ],
  },
  {
    name: 'partial-budget',
    limits: { budgetMs: 10000, clockStep: 4000 },
    target: [snapshot('mac', [])],
    source: [snapshot('windows', sourceRows(18))],
    append: Array.from({ length: 18 }, () => ({ receipt: created })),
  },
  {
    name: 'partial-batches',
    limits: { maxBatches: 2, clockStep: 0 },
    target: [snapshot('mac', [])],
    source: [snapshot('windows', sourceRows(60))],
    append: Array.from({ length: 60 }, () => ({ receipt: created })),
  },
  {
    name: 'lost-second-receipt',
    target: [snapshot('mac', [])],
    source: [snapshot('windows', sourceRows(4))],
    append: [{ receipt: created }, { throw: true }, { receipt: created }],
  },
  {
    name: 'clean-second-refusal',
    target: [snapshot('mac', [])],
    source: [snapshot('windows', sourceRows(4))],
    append: [
      { receipt: created },
      {
        receipt: {
          status: 'refused',
          createdCount: 0,
          recoveryRequired: false,
          ownedFilesRolledBack: true,
          writerQuiescent: true,
        },
      },
    ],
  },
  {
    name: 'registry-bound',
    target: [snapshot('mac', targetRows(180))],
    source: [snapshot('windows', sourceRows(32))],
  },
  { name: 'unsupported-profile', profileId: 'me' },
  { name: 'unsupported-direction', profileId: 'gmail', targetPlatform: 'windows' },
  {
    name: 'policy-direction-mismatch',
    policy: { ...policy, sourcePlatform: 'mac' },
    target: [snapshot('mac', [])],
    source: [snapshot('windows', sourceRows(2))],
  },
  {
    name: 'target-open',
    closed: [false],
    target: [snapshot('mac', [])],
    source: [snapshot('windows', sourceRows(2))],
  },
  {
    name: 'native-guard-unverified',
    target: [
      snapshot('mac', [], {
        nativeGuard: {
          ...core.NATIVE.mac,
          managerSha256: '0'.repeat(64),
          warmGuardVerified: true,
          autoResumeGuardVerified: true,
        },
      }),
    ],
    source: [snapshot('windows', sourceRows(2))],
  },
  {
    name: 'destination-pending-input',
    target: [snapshot('mac', [], { noPendingInput: false })],
    source: [snapshot('windows', sourceRows(2))],
  },
  {
    name: 'source-changed',
    target: [snapshot('mac', [])],
    source: [snapshot('windows', sourceRows(2)), snapshot('windows', sourceRows(3))],
  },
  {
    name: 'transcripts-unverified',
    verify: false,
    target: [snapshot('mac', [])],
    source: [snapshot('windows', sourceRows(2))],
  },
  {
    name: 'drift-target-opens',
    closed: [true, true, false],
    target: [snapshot('mac', [])],
    source: [snapshot('windows', sourceRows(4))],
    append: [{ receipt: created }, { receipt: created }],
  },
  {
    name: 'drift-protected-state',
    protected: [true, false],
    target: [snapshot('mac', [])],
    source: [snapshot('windows', sourceRows(4))],
    append: [{ receipt: created }, { receipt: created }],
  },
  {
    name: 'drift-extra-record',
    target: [snapshot('mac', []), snapshot('mac', [envelope(record(9999, 'mac'))])],
    source: [snapshot('windows', sourceRows(4))],
    append: [{ receipt: created }, { receipt: created }],
  },
  {
    name: 'identity-collision',
    target: [snapshot('mac', [envelope(record(1, 'mac', { cliSessionId: uuid(7777) }))])],
    source: [snapshot('windows', sourceRows(2))],
  },
  {
    name: 'skip-reasons',
    target: [snapshot('mac', [])],
    source: [
      snapshot('windows', [
        envelope(record(1, 'windows')),
        envelope(record(2, 'windows', { title: '' })),
        envelope(record(3, 'windows', { sshRemoteTranscriptPath: seed.transcriptRoot + '/bad.jsonl' })),
        envelope(record(4, 'windows', { createdAt: -1 })),
      ]),
    ],
    append: [{ receipt: created }],
  },
];

async function runSequence(test: SequenceCase): Promise<string> {
  const profileId = test.profileId ?? 'gmail';
  const targetPlatform = test.targetPlatform ?? 'mac';
  const counters = { closed: 0, target: 0, source: 0, protected: 0, append: 0 };
  const pick = <T,>(list: T[] | undefined, index: number, fallback: T): T =>
    list === undefined ? fallback : list[Math.min(index, list.length - 1)];
  let clock = 0;
  const step = test.limits?.clockStep ?? 0;
  // The fake writer grows the target registry exactly as the packaged transaction does:
  // each confirmed batch's records become part of the next readTarget snapshot. Scripted
  // target entries override the growth (the drift cases pin an explicit changed snapshot).
  const appended: ByteRow[] = [];
  const baseline = (test.target ?? [snapshot('mac', [])])[0];
  const adapters: SequenceAdapters = {
    isTargetClosed: async () => pick(test.closed, counters.closed++, true),
    readTarget: async () => {
      const index = counters.target++;
      const scripted = test.target?.[index];
      if (scripted !== undefined) return toBuffers(scripted);
      const rows = [
        ...baseline.records.map((row) => ({
          name: row.name,
          bytes: unhex(row.bytes),
          sha256: row.sha256,
        })),
        ...appended,
      ];
      return toBuffers({ ...baseline, records: rows.map(wire), revision: core.snapshotRevision(rows) });
    },
    readSource: async () => toBuffers(pick(test.source, counters.source++, snapshot('windows', []))),
    verifyTranscriptReferences: async () => test.verify ?? true,
    targetProtectedStateUnchanged: async () => pick(test.protected, counters.protected++, true),
    appendCreateOnly: async (payload) => {
      const entry = pick(test.append, counters.append++, { receipt: created });
      clock += step;
      if ('throw' in entry) throw new Error('SYNTHETIC_PRIVATE_ADAPTER_ERROR');
      const receipt = entry.receipt;
      if (
        receipt &&
        typeof receipt === 'object' &&
        'status' in receipt &&
        receipt.status === 'created_metadata'
      ) {
        appended.push(
          ...payload.records.map((row) => ({ name: row.name, bytes: row.bytes, sha256: sha(row.bytes) }))
        );
      }
      return entry.receipt;
    },
  };
  const limits = {
    ...(test.limits?.budgetMs !== undefined ? { budgetMs: test.limits.budgetMs } : {}),
    ...(test.limits?.maxBatches !== undefined ? { maxBatches: test.limits.maxBatches } : {}),
    now: () => clock,
  };
  const result = await core.synchronizeBeforeProfileOpen({
    profileId,
    targetPlatform,
    policy: test.policy ?? policy,
    adapters,
    limits,
  });
  return JSON.stringify(result);
}

// --- policy parse vectors ------------------------------------------------------------------
const policyCases: { name: string; value: unknown }[] = [
  { name: 'valid', value: policy },
  { name: 'version', value: { ...policy, version: 2 } },
  { name: 'enabled', value: { ...policy, enabled: false } },
  {
    name: 'uuid-uppercase',
    value: { ...policy, identity: { ...policy.identity, accountUuid: seed.accountUuid.toUpperCase() } },
  },
  {
    name: 'uuid-shape',
    value: { ...policy, identity: { ...policy.identity, organizationUuid: 'not-a-uuid' } },
  },
  { name: 'project-cwd', value: { ...policy, project: { ...policy.project, cwd: '/elsewhere' } } },
  {
    name: 'root-double-slash',
    value: { ...policy, project: { ...policy.project, transcriptRoot: '/synthetic-user//.claude/projects' } },
  },
  {
    name: 'root-dot-dot',
    value: { ...policy, project: { ...policy.project, transcriptRoot: '/synthetic-user/../.claude/projects' } },
  },
  {
    name: 'root-backslash',
    value: { ...policy, project: { ...policy.project, transcriptRoot: '/synthetic-user\\x/.claude/projects' } },
  },
  {
    name: 'root-suffix',
    value: { ...policy, project: { ...policy.project, transcriptRoot: '/synthetic-user/.claude/other' } },
  },
  {
    name: 'root-control',
    value: { ...policy, project: { ...policy.project, transcriptRoot: '/synthetic\u0001user/.claude/projects' } },
  },
  {
    name: 'alias-shape',
    value: { ...policy, ssh: { ...policy.ssh, mac: { ...policy.ssh.mac, alias: '-bad' } } },
  },
  {
    name: 'port-range',
    value: {
      ...policy,
      ssh: { ...policy.ssh, mac: { ...policy.ssh.mac, port: 0 }, windows: { ...policy.ssh.windows, port: 0 } },
    },
  },
  {
    name: 'port-float',
    value: {
      ...policy,
      ssh: { ...policy.ssh, mac: { ...policy.ssh.mac, port: 22.5 }, windows: { ...policy.ssh.windows, port: 22.5 } },
    },
  },
  {
    name: 'endpoint-mismatch',
    value: { ...policy, ssh: { ...policy.ssh, mac: { ...policy.ssh.mac, hostname: 'other-host' } } },
  },
  { name: 'unknown-key', value: { ...policy, extra: 1 } },
  { name: 'identity-unknown-key', value: { ...policy, identity: { ...policy.identity, extra: 1 } } },
  { name: 'ssh-missing-platform', value: { ...policy, ssh: { mac: policy.ssh.mac } } },
  { name: 'null', value: null },
  { name: 'array', value: [policy] },
];

// --- neutral projection vectors --------------------------------------------------------------
const neutralCases: { name: string; source: Record<string, unknown> }[] = [
  { name: 'plain', source: record(1, 'windows') },
  {
    name: 'model-effort-turns',
    source: record(1, 'windows', { model: 'claude-opus-4-5', effort: 'high', completedTurns: 12 }),
  },
  { name: 'model-invalid', source: record(1, 'windows', { model: 'gpt-5' }) },
  { name: 'effort-invalid', source: record(1, 'windows', { effort: 'ultra' }) },
  { name: 'turns-negative', source: record(1, 'windows', { completedTurns: -1 }) },
  { name: 'turns-float', source: record(1, 'windows', { completedTurns: 1.5 }) },
  { name: 'wsl-config', source: record(1, 'windows', { wslConfig: { distro: 'x' } }) },
  { name: 'backend-local', source: record(1, 'windows', { backend: { kind: 'local' } }) },
  { name: 'backend-ssh', source: record(1, 'windows', { backend: { kind: 'ssh' } }) },
  {
    name: 'reference-backslash',
    source: record(1, 'windows', { sshRemoteTranscriptPath: seed.transcriptRoot + '\\bad\\x.jsonl' }),
  },
  {
    name: 'reference-depth',
    source: record(1, 'windows', { sshRemoteTranscriptPath: seed.transcriptRoot + '/a/b/c.jsonl' }),
  },
  {
    name: 'reference-name',
    source: record(1, 'windows', {
      sshRemoteTranscriptPath: seed.transcriptRoot + '/-synthetic-project-A/other.jsonl',
    }),
  },
  { name: 'title-empty', source: record(1, 'windows', { title: '' }) },
  { name: 'title-control', source: record(1, 'windows', { title: 'bad\u0001title' }) },
  { name: 'title-long', source: record(1, 'windows', { title: 'x'.repeat(1001) }) },
  { name: 'created-negative', source: record(1, 'windows', { createdAt: -1 }) },
  { name: 'archived-string', source: record(1, 'windows', { isArchived: 'false' }) },
  { name: 'cwd-mismatch', source: record(1, 'windows', { cwd: '/elsewhere' }) },
  { name: 'alias-mismatch', source: record(1, 'windows', { sshConfig: { sshHost: 'other-alias' } }) },
];

// --- progress store vectors ------------------------------------------------------------------
interface OpStep {
  action: 'start' | 'copying' | 'synchronized' | 'opening' | 'finish' | 'advance' | 'snapshot';
  profile?: string;
  platform?: 'mac' | 'windows';
  total?: number;
  confirmed?: number;
  status?: string;
  createdCount?: number;
  error?: 'held' | 'notfound' | 'notconfig' | 'timeout' | 'transport' | 'other';
  ms?: number;
}
const opError = (kind: string): unknown =>
  kind === 'held'
    ? new ClaudeHistoryOpenHeldError()
    : kind === 'notfound'
      ? new ProfileError('x')
      : kind === 'notconfig'
        ? new ConfigError('x')
        : kind === 'timeout'
          ? new ClaudeDesktopTransportError(true)
          : kind === 'transport'
            ? new ClaudeDesktopTransportError(false)
            : new Error('SYNTHETIC_PRIVATE');
const operationsCases: { name: string; retainMs?: number; steps: OpStep[] }[] = [
  {
    name: 'managed-progress',
    steps: [
      { action: 'start', profile: 'gmail', platform: 'mac' },
      { action: 'snapshot' },
      { action: 'copying', total: 4, confirmed: 0 },
      { action: 'snapshot' },
      { action: 'copying', total: 4, confirmed: 2 },
      { action: 'synchronized', status: 'synchronized', createdCount: 4 },
      { action: 'opening' },
      { action: 'snapshot' },
      { action: 'finish' },
      { action: 'snapshot' },
    ],
  },
  {
    name: 'held-after-batches',
    steps: [
      { action: 'start', profile: 'gmail', platform: 'mac' },
      { action: 'copying', total: 4, confirmed: 0 },
      { action: 'synchronized', status: 'refused', createdCount: 1 },
      { action: 'finish', error: 'held' },
      { action: 'snapshot' },
    ],
  },
  {
    name: 'failure-kinds',
    steps: [
      { action: 'start', profile: 'a', platform: 'mac' },
      { action: 'finish', error: 'notfound' },
      { action: 'start', profile: 'b', platform: 'mac' },
      { action: 'finish', error: 'notconfig' },
      { action: 'start', profile: 'c', platform: 'mac' },
      { action: 'finish', error: 'timeout' },
      { action: 'start', profile: 'd', platform: 'mac' },
      { action: 'finish', error: 'transport' },
      { action: 'start', profile: 'e', platform: 'mac' },
      { action: 'finish', error: 'other' },
      { action: 'snapshot' },
    ],
  },
  {
    name: 'counts-only-forward',
    steps: [
      { action: 'start', profile: 'gmail', platform: 'mac' },
      { action: 'copying', total: 4, confirmed: 2 },
      { action: 'copying', total: 4, confirmed: 1 },
      { action: 'copying', total: 3, confirmed: 3 },
      { action: 'synchronized', status: 'synchronized', createdCount: 9 },
      { action: 'synchronized', status: 'skipped', createdCount: 3 },
      { action: 'snapshot' },
    ],
  },
  {
    name: 'join-running',
    steps: [
      { action: 'start', profile: 'gmail', platform: 'mac' },
      { action: 'start', profile: 'gmail', platform: 'mac' },
      { action: 'snapshot' },
      { action: 'finish' },
      { action: 'start', profile: 'gmail', platform: 'mac' },
      { action: 'snapshot' },
    ],
  },
  {
    name: 'retain-and-cap',
    retainMs: 1000,
    steps: [
      { action: 'start', profile: 'a', platform: 'mac' },
      { action: 'finish' },
      { action: 'advance', ms: 1500 },
      { action: 'snapshot' },
      { action: 'start', profile: 'b', platform: 'mac' },
      { action: 'finish' },
      { action: 'snapshot' },
    ],
  },
  {
    name: 'scope-order',
    steps: [
      { action: 'start', profile: 'b', platform: 'mac' },
      { action: 'finish' },
      { action: 'start', profile: 'a', platform: 'mac' },
      { action: 'start', profile: 'a', platform: 'windows' },
      { action: 'snapshot' },
    ],
  },
];

interface RunningEntry {
  resolve: () => void;
  reject: (error: unknown) => void;
  observer: ClaudeOpenObserver;
}

async function runOperations(test: (typeof operationsCases)[number]): Promise<unknown[]> {
  let clock = 1_000_000;
  const operations = new ClaudeOpenOperations({ now: () => clock, retainMs: test.retainMs });
  const running = new Map<string, RunningEntry>();
  const snapshots: unknown[] = [];
  const scope = '/synthetic/.ccs';
  for (const step of test.steps) {
    const profile = step.profile ?? 'gmail';
    const platform = step.platform ?? 'mac';
    const key = `${profile}/${platform}`;
    if (step.action === 'start') {
      operations.start(scope, profile, platform, (observer) => {
        const { promise, resolve, reject } = Promise.withResolvers<void>();
        running.set(key, { resolve, reject, observer });
        return promise;
      });
    } else if (step.action === 'copying') {
      running.get(key)?.observer.copying?.(step.total ?? 0, step.confirmed);
    } else if (step.action === 'synchronized') {
      running.get(key)?.observer.synchronized?.({
        status: step.status ?? '',
        createdCount: step.createdCount ?? 0,
      });
    } else if (step.action === 'opening') {
      running.get(key)?.observer.opening?.();
    } else if (step.action === 'finish') {
      const entry = running.get(key);
      if (entry) {
        if (step.error) entry.reject(opError(step.error));
        else entry.resolve();
        running.delete(key);
      }
      await Promise.resolve();
      await Promise.resolve();
    } else if (step.action === 'advance') {
      clock += step.ms ?? 0;
    } else if (step.action === 'snapshot') {
      await Promise.resolve();
      snapshots.push({
        forScope: operations.forScope(scope),
        running: operations.running(scope, profile, platform),
      });
    }
  }
  return snapshots;
}

// --- account-list vectors ---------------------------------------------------------------------
const win = (id: string, changes: Record<string, unknown> = {}): ClaudeDesktopProfile => ({
  id,
  email: `${id}@example.test`,
  windows: {
    launcherName: id,
    sshHost: 'synthetic-windows-alias-A',
    profilePath: `C:/Users/u/AppData/Roaming/Claude-${id}`,
    ...changes,
  },
});
const accountListCases: {
  name: string;
  manifest: ClaudeDesktopProfile[];
  pending?: PendingClaudeProfile[];
}[] = [
  { name: 'default-first', manifest: [win('work2'), win('store', { isDefault: true })] },
  {
    name: 'default-folder',
    manifest: [win('work2'), win('store', { profilePath: 'C:/Users/u/AppData/Roaming/Claude' })],
  },
  { name: 'no-default', manifest: [win('work2'), win('party')] },
  {
    name: 'two-defaults',
    manifest: [
      win('store', { isDefault: true }),
      win('work2', { profilePath: 'C:/Users/u/AppData/Roaming/Claude/' }),
    ],
  },
  {
    name: 'pending-opens',
    manifest: [win('store', { isDefault: true })],
    pending: [
      {
        id: 'new1',
        label: null,
        mac: {
          launcherName: 'new1',
          profilePath: '/Users/u/Library/Application Support/Claude-new1',
          sshHost: 'synthetic-mac-alias-A',
        },
        windows: {
          launcherName: 'new1',
          profilePath: 'C:/Users/u/AppData/Roaming/Claude-new1',
          sshHost: 'synthetic-windows-alias-A',
        },
        createdAt: '2026-10-02T00:00:00.000Z',
        expectedEmail: null,
      },
    ],
  },
  {
    name: 'mac-only-skipped',
    manifest: [
      win('store', { isDefault: true }),
      {
        id: 'maconly',
        email: 'm@example.test',
        mac: {
          launcherName: 'maconly',
          launcherPath: '/Applications/Claude.app',
          sshHost: 'synthetic-mac-alias-A',
        },
      },
    ],
  },
];

// --- assemble ---------------------------------------------------------------------------------
async function main(): Promise<void> {
  const table = {
    note: 'G13 parity goldens from the real TS core, progress store, policy parse and account-list rule. Synthetic fixtures only.',
    policy: policyCases.map((test) => ({
      name: test.name,
      value: test.value,
      expected: parseClaudeHistoryPolicy(test.value),
    })),
    neutral: neutralCases.map((test) => ({
      name: test.name,
      source: test.source,
      expected: core.neutral(test.source, 'windows', 'mac', policy),
    })),
    plan: [
      {
        name: 'mixed',
        source: [
          envelope(record(1, 'windows')),
          envelope(record(2, 'windows')),
          envelope(record(3, 'windows', { title: '' })),
        ].map(wire),
        target: [envelope(record(1, 'mac', { cliSessionId: uuid(2) }))].map(wire),
      },
    ].map((test) => {
      const plan = core.planMissing(
        decodeRows(test.source),
        decodeRows(test.target),
        'windows',
        'mac',
        policy
      );
      return {
        name: test.name,
        source: test.source,
        target: test.target,
        expected: {
          records: plan.records.map((row) => ({ name: row.name, bytes: hex(row.bytes) })),
          alreadyPresentCount: plan.alreadyPresentCount,
          skipped: plan.skipped,
        },
      };
    }),
    revision: [
      { name: 'two', rows: sourceRows(2).map(wire) },
      { name: 'empty', rows: [] as WireRow[] },
    ].map((test) => ({
      name: test.name,
      rows: test.rows,
      expected: core.snapshotRevision(
        test.rows.map((row) => ({ name: row.name, bytes: unhex(row.bytes), sha256: row.sha256 }))
      ),
    })),
    decode: [
      { name: 'valid', rows: sourceRows(2).map(wire) },
      {
        name: 'bad-name',
        rows: [{ name: 'other.json', bytes: hex(Buffer.from('{}')), sha256: sha(Buffer.from('{}')) }],
      },
      { name: 'bad-sha', rows: [{ ...wire(sourceRows(1)[0]), sha256: '0'.repeat(64) }] },
      {
        name: 'bad-identity',
        rows: [wire(envelope(record(1, 'windows', { sessionId: 'local_not-a-uuid' })))],
      },
      { name: 'duplicate', rows: [wire(sourceRows(1)[0]), wire(sourceRows(1)[0])] },
    ].map((test) => {
      let reason: string | null = null;
      try {
        core.decodeRecords({
          records: test.rows.map((row) => ({
            name: row.name,
            bytes: unhex(row.bytes),
            sha256: row.sha256,
          })),
        });
      } catch (error) {
        reason =
          error && typeof error === 'object' && 'reason' in error && typeof error.reason === 'string'
            ? error.reason
            : 'unavailable';
      }
      return { name: test.name, rows: test.rows, expected: reason };
    }),
    sequences: [] as { name: string; expected: string }[],
    operations: [] as { name: string; expected: unknown }[],
    accountList: accountListCases.map((test) => ({
      name: test.name,
      manifest: test.manifest,
      pending: test.pending ?? [],
      expected: claudeWindowsLauncherAccountList(test.manifest, test.pending ?? []),
    })),
  };
  for (const test of sequences) {
    table.sequences.push({ name: test.name, expected: await runSequence(test) });
  }
  for (const test of operationsCases) {
    table.operations.push({ name: test.name, expected: await runOperations(test) });
  }
  const file = path.join(path.dirname(fileURLToPath(import.meta.url)), 'claude-open-table.json');
  // Operation ids are random on both sides; the table pins one token (the Rust test
  // normalizes its own ids the same way before comparing).
  const text = JSON.stringify(table, null, 2).replace(/op_[0-9a-f]{24}/g, 'op_ID') + '\n';
  if (process.argv.includes('--check')) {
    const existing = fs.readFileSync(file, 'utf8');
    if (existing !== text) {
      console.error('claude-open-table.json drifted; regenerate with bun tests/spec/agent/claude-open-table.ts');
      process.exit(1);
    }
    console.log('claude-open-table.json matches the TS reference');
    return;
  }
  fs.writeFileSync(file, text);
  console.log(
    `claude-open-table.json written: ${table.sequences.length} sequences, ${table.operations.length} operation cases`
  );
}

void main();
