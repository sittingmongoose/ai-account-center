import fs from 'fs';
import path from 'path';
import { createHash } from 'crypto';
import { getCcsDir } from '../../utils/config-manager';
import {
  CLAUDE_PROFILE_ID_PATTERN,
  listClaudeDesktopProfiles as listProfiles,
  type ClaudeDesktopProfile,
} from './claude-desktop-profile-service';
import { runClaudeHistoryHelper } from './claude-desktop-transport';
import { claudeHistoryOpenHeldWithoutCore } from './claude-history-hold-fallback';
import { ValidationError } from '../../errors/error-types';
import { createLogger } from '../../services/logging';
import { readDashboardPreferences } from './dashboard-preferences';

export interface ClaudeHistorySyncPolicy {
  version: 1;
  enabled: true;
  sourcePlatform: 'mac' | 'windows';
  identity: { accountUuid: string; organizationUuid: string };
  project: { cwd: string; originCwd: string; transcriptRoot: string };
  ssh: Record<
    'mac' | 'windows',
    { alias: string; hostname: string; username: string; port: number }
  >;
}
interface HistoryResult {
  status: string;
  reason?: string;
  createdCount: number;
  recoveryRequired?: boolean;
  /** Set on `partial`: records the per-Open bound left for the next Open. */
  remainingCount?: number;
}
interface PrivateRecord {
  name: string;
  bytes: Buffer;
  sha256: string;
}
interface PrivateSnapshot {
  profileId: string;
  platform: 'mac' | 'windows';
  records: PrivateRecord[];
  [key: string]: unknown;
}
interface HistoryCore {
  /** IDs the pinned copy core supports; the server keeps no copy of this list. */
  PROFILES: ReadonlySet<string>;
  validatePolicy: (policy: unknown) => ClaudeHistorySyncPolicy;
  synchronizeBeforeProfileOpen: (request: {
    profileId: string;
    targetPlatform: 'mac' | 'windows';
    policy: ClaudeHistorySyncPolicy;
    adapters: Record<string, (...args: unknown[]) => Promise<unknown>>;
  }) => Promise<HistoryResult>;
  pendingMarkerState: (
    directory: string,
    profileId: string,
    platform: 'mac' | 'windows'
  ) => { held: boolean };
  armPendingMarker: (
    directory: string,
    profileId: string,
    platform: 'mac' | 'windows'
  ) => HistoryMarker;
}
/** One Open's durable uncertain-append hold (see armPendingMarker in the core). */
interface HistoryMarker {
  assertBound: () => void;
  finish: (receipt: unknown) => void;
  /** Back to pending before the next batch; only after this marker's own finish. */
  rearm: () => void;
  release: () => void;
}
const scriptsRoot = path.resolve(__dirname, '../../../scripts/claude-history');
// Runtime uses the already packaged scripts directory. It never accepts a
// module path or executable from a request or from the private identity policy.
function loadCore(): HistoryCore {
  return require(path.join(scriptsRoot, 'history-index-sync.cjs')) as HistoryCore;
}
const MAX_MANIFEST_BYTES = 256 * 1024;
const MAX_HELPER_BYTES = 24 * 1024 * 1024;
const UUID = /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/;
const hash = (data: Buffer): string => createHash('sha256').update(data).digest('hex');
const skipped = (reason: string): HistoryResult => ({ status: 'skipped', reason, createdCount: 0 });
const logger = createLogger('web-server:claude-history');

/** Counts, platform and fixed reason tokens only: never ids, paths, titles or errors. */
function logHistory(
  level: 'info' | 'warn',
  event: string,
  message: string,
  context: Record<string, unknown>
): void {
  try {
    logger[level](event, message, context);
  } catch {
    /* Logging never changes the copy or the Open. */
  }
}

/** Check before policy lookup and after every optional-copy outcome. A removed
 * policy, repeated click or server restart cannot clear an uncertain append.
 */
export function claudeHistoryOpenHeld(profileId: string, platform: 'mac' | 'windows'): boolean {
  try {
    // The pinned core reports every ID outside its own set as held, but only
    // IDs in that set can ever arm a marker. Ask the core which IDs those are
    // instead of keeping a server-side copy, so other launchers keep ordinary
    // Open and new manifest IDs are never held by an impossible marker.
    const core = loadCore();
    if (!core.PROFILES.has(profileId)) return false;
    return core.pendingMarkerState(getCcsDir(), profileId, platform).held;
  } catch {
    return claudeHistoryOpenHeldWithoutCore(getCcsDir(), profileId, platform);
  }
}

export { claudeHistoryOpenHeldWithoutCore };

function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}
function keysAre(value: Record<string, unknown>, allowed: string[]): boolean {
  return Object.keys(value).every((key) => allowed.includes(key));
}

/** Optional identity expectations stay in the existing private managed file.
 * Public profile metadata continues to omit this field. Unknown or absent
 * policy never enrolls the current native identity as a new trusted identity.
 */
export function parseClaudeHistoryPolicy(value: unknown): ClaudeHistorySyncPolicy | null {
  if (
    !object(value) ||
    !keysAre(value, ['version', 'enabled', 'sourcePlatform', 'identity', 'project', 'ssh']) ||
    !object(value.identity) ||
    !keysAre(value.identity, ['accountUuid', 'organizationUuid']) ||
    !object(value.project) ||
    !keysAre(value.project, ['cwd', 'originCwd', 'transcriptRoot']) ||
    !object(value.ssh) ||
    !keysAre(value.ssh, ['mac', 'windows'])
  )
    return null;
  if (
    typeof value.identity.accountUuid !== 'string' ||
    !UUID.test(value.identity.accountUuid) ||
    typeof value.identity.organizationUuid !== 'string' ||
    !UUID.test(value.identity.organizationUuid)
  )
    return null;
  for (const platform of ['mac', 'windows']) {
    const endpoint = value.ssh[platform];
    if (!object(endpoint) || !keysAre(endpoint, ['alias', 'hostname', 'username', 'port']))
      return null;
  }
  try {
    // JSON cloning only the validated allowlist prevents private unknown fields
    // from ever entering a transport request. Core validates all field values.
    return loadCore().validatePolicy(JSON.parse(JSON.stringify(value)));
  } catch {
    return null;
  }
}

export async function loadClaudeHistoryPolicy(
  profile: ClaudeDesktopProfile
): Promise<ClaudeHistorySyncPolicy | null> {
  // The profile is already manifest-resolved; the manifest entry below is
  // re-validated before any policy is returned. Only the policy file decides
  // which copy directions are allowed.
  if (!profile.id || !CLAUDE_PROFILE_ID_PATTERN.test(profile.id)) return null;
  const filename = path.join(getCcsDir(), 'claude-desktop-profiles.json');
  let handle: fs.promises.FileHandle | undefined;
  try {
    const before = await fs.promises.lstat(filename);
    if (
      !before.isFile() ||
      before.isSymbolicLink() ||
      before.size > MAX_MANIFEST_BYTES ||
      (process.platform !== 'win32' &&
        (before.uid !== process.getuid?.() || (before.mode & 0o077) !== 0))
    )
      return null;
    handle = await fs.promises.open(
      filename,
      fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0)
    );
    const opened = await handle.stat();
    if (opened.dev !== before.dev || opened.ino !== before.ino || opened.size !== before.size)
      return null;
    const bytes = await handle.readFile();
    const after = await fs.promises.lstat(filename);
    if (
      bytes.length > MAX_MANIFEST_BYTES ||
      after.dev !== opened.dev ||
      after.ino !== opened.ino ||
      after.size !== opened.size ||
      after.mtimeMs !== opened.mtimeMs ||
      after.ctimeMs !== opened.ctimeMs
    )
      return null;
    const manifest: unknown = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes));
    if (!object(manifest) || manifest.version !== 1 || !Array.isArray(manifest.profiles))
      return null;
    const matches = manifest.profiles.filter(
      (entry: unknown) => object(entry) && entry.id === profile.id
    );
    if (matches.length !== 1 || !object(matches[0]) || matches[0].email !== profile.email)
      return null;
    const entry = matches[0];
    // Rebind the launcher paths/hosts to the entry already selected by the
    // authenticated Open handler; a changed manifest cannot redirect the copy.
    for (const platform of ['mac', 'windows'] as const) {
      const expected = profile[platform],
        actual = entry[platform];
      if (
        !expected ||
        !object(actual) ||
        actual.sshHost !== expected.sshHost ||
        actual.profilePath !== expected.profilePath ||
        actual.launcherPath !== expected.launcherPath
      )
        return null;
    }
    return parseClaudeHistoryPolicy(entry.historySync);
  } catch {
    return null;
  } finally {
    await handle?.close();
  }
}

function decodeSnapshot(value: unknown): PrivateSnapshot {
  if (!object(value) || !Array.isArray(value.records) || value.records.length > 200)
    throw new ValidationError('snapshot_unavailable');
  let total = 0;
  const records = value.records.map((row: unknown): PrivateRecord => {
    if (
      !object(row) ||
      typeof row.name !== 'string' ||
      typeof row.base64 !== 'string' ||
      typeof row.sha256 !== 'string' ||
      !/^[0-9a-f]{64}$/.test(row.sha256) ||
      row.base64.length > 2_666_668 ||
      !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(row.base64)
    )
      throw new ValidationError('snapshot_unavailable');
    const bytes = Buffer.from(row.base64, 'base64');
    total += bytes.length;
    if (bytes.length > 2_000_000 || total > 16_000_000 || hash(bytes) !== row.sha256)
      throw new ValidationError('snapshot_unavailable');
    return { name: row.name, bytes, sha256: row.sha256 };
  });
  return { ...value, records } as PrivateSnapshot;
}
function wireSnapshot(value: unknown): unknown {
  if (!object(value) || !Array.isArray(value.records)) return value;
  return {
    ...value,
    records: value.records.map((row: PrivateRecord) => ({
      name: row.name,
      base64: row.bytes.toString('base64'),
      sha256: hash(row.bytes),
    })),
  };
}

/** Progress callbacks for the Open progress view; counts only, never record contents. */
export interface ClaudeHistorySyncObserver {
  /**
   * A bounded batch of the copy is about to be appended. `totalCount` is the
   * whole plan of missing records and `confirmedCount` the records already
   * confirmed by earlier batches of this Open (0 before the first batch).
   */
  copying?: (totalCount: number, confirmedCount?: number) => void;
}

/** An observer can never change or fail the copy transaction it watches. */
function notifyObserver(callback: () => void): void {
  try {
    callback();
  } catch {
    /* Progress is informational only. */
  }
}

/** Snapshot retention counts for one profile on one platform (counts only, never names). */
export interface HistorySnapshotCleanup {
  kept: number;
  deleted: number;
  skipped: number;
}

function isCleanupResult(
  value: unknown
): value is Record<'kept' | 'deleted' | 'skipped', string[]> {
  if (!object(value)) return false;
  return (['kept', 'deleted', 'skipped'] as const).every((key) => {
    const names: unknown = value[key];
    return (
      Array.isArray(names) &&
      names.length <= 1024 &&
      names.every((name: unknown) => typeof name === 'string' && name.length <= 128)
    );
  });
}

/**
 * Retention for one profile's history snapshots on one platform, through the
 * pinned helper path: the fixed helper keeps the newest 3 snapshot folders it
 * created and deletes older ones. A refused or malformed answer throws; the
 * caller decides whether the copy or the button press fails with it.
 */
export async function cleanupHistorySnapshots(
  profile: ClaudeDesktopProfile,
  platform: 'mac' | 'windows'
): Promise<HistorySnapshotCleanup> {
  const id = profile.id;
  const launcher = profile[platform];
  const policy = await loadClaudeHistoryPolicy(profile);
  if (!id || !launcher || !policy) throw new ValidationError('helper_unavailable');
  const data = await runClaudeHistoryHelper(launcher, platform, id, {
    mode: 'snapshot-cleanup',
    profileId: id,
    platform,
    policy,
    expectedEmail: profile.email,
  });
  if (data.length > MAX_HELPER_BYTES) throw new ValidationError('helper_unavailable');
  const response: unknown = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(data));
  if (!isCleanupResult(response)) throw new ValidationError('helper_unavailable');
  return {
    kept: response.kept.length,
    deleted: response.deleted.length,
    skipped: response.skipped.length,
  };
}

/** "Clean up now": retention for every known profile on each of its platforms. */
export interface HistorySnapshotCleanupAll {
  profiles: number;
  targets: number;
  kept: number;
  deleted: number;
  skipped: number;
  failed: number;
}

export async function cleanupAllHistorySnapshots(
  list: () => Promise<ClaudeDesktopProfile[]> = listProfiles
): Promise<HistorySnapshotCleanupAll> {
  const total: HistorySnapshotCleanupAll = {
    profiles: 0,
    targets: 0,
    kept: 0,
    deleted: 0,
    skipped: 0,
    failed: 0,
  };
  let profiles: ClaudeDesktopProfile[];
  try {
    profiles = await list();
  } catch {
    return total;
  }
  for (const profile of profiles) {
    if (!profile?.id) continue;
    total.profiles += 1;
    for (const platform of ['mac', 'windows'] as const) {
      if (!profile[platform]) continue;
      total.targets += 1;
      try {
        const cleanup = await cleanupHistorySnapshots(profile, platform);
        total.kept += cleanup.kept;
        total.deleted += cleanup.deleted;
        total.skipped += cleanup.skipped;
      } catch {
        total.failed += 1;
      }
    }
  }
  logHistory('info', 'claude.history.snapshots_cleaned', 'History snapshots cleaned now', {
    ...total,
  });
  return total;
}

/** Called only inside the existing coalesced authenticated Open operation.
 * This does not open/resume/stop an app, write a transcript or return private
 * descriptors. Pre-append skips preserve Open; uncertain append holds Open.
 */
export async function synchronizeClaudeHistoryBeforeOpen(
  profile: ClaudeDesktopProfile,
  platform: 'mac' | 'windows',
  observer: ClaudeHistorySyncObserver = {}
): Promise<HistoryResult> {
  try {
    if (profile.id && claudeHistoryOpenHeld(profile.id, platform))
      return {
        status: 'refused',
        reason: 'create_only_transaction_unconfirmed',
        createdCount: 0,
        recoveryRequired: true,
      };
    const policy = await loadClaudeHistoryPolicy(profile);
    if (!policy || !profile.id) return skipped('history_policy_unverified');
    const id = profile.id;
    const call = async (
      target: 'mac' | 'windows',
      mode: string,
      fields: Record<string, unknown> = {}
    ): Promise<unknown> => {
      const launcher = profile[target];
      if (!launcher) throw new ValidationError('helper_unavailable');
      const data = await runClaudeHistoryHelper(launcher, target, id, {
        mode,
        profileId: id,
        platform: target,
        policy,
        expectedEmail: profile.email,
        ...fields,
      });
      if (data.length > MAX_HELPER_BYTES) throw new ValidationError('helper_unavailable');
      const response: unknown = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(data));
      // The fixed helper emits canonical ASCII-only append receipts. Reject
      // duplicate keys, trailing data and other malformed acknowledgements.
      if (mode === 'append' && !data.equals(Buffer.from(JSON.stringify(response))))
        throw new ValidationError('helper_unavailable');
      return response;
    };
    // One durable marker per Open, so the bounded marker capacity counts Opens
    // that copied, not records. It is armed before the first batch, finished by
    // each batch's trusted terminal receipt and re-armed before the next batch;
    // a lost or malformed receipt leaves it pending and stops the sequence.
    const sequence: { marker: HistoryMarker | null; stopReason: string | null } = {
      marker: null,
      stopReason: null,
    };
    // A stop before this batch's marker is armed or re-armed: no remote append
    // was invoked and no marker is pending, so the copy ends cleanly and Open
    // proceeds. The next Open's plan skips the records already confirmed.
    const cleanStop = (reason: string): unknown => {
      sequence.stopReason = reason;
      return {
        status: 'refused',
        createdCount: 0,
        recoveryRequired: false,
        ownedFilesRolledBack: true,
      };
    };
    const appendCreateOnly = async (payload: unknown): Promise<unknown> => {
      // A changed or removed policy (for example, removed to stop a long copy)
      // stops before this batch; it never claims an unconfirmed append.
      const currentPolicy = await loadClaudeHistoryPolicy(profile);
      if (!currentPolicy || JSON.stringify(currentPolicy) !== JSON.stringify(policy))
        return cleanStop('history_policy_changed');
      if (!object(payload)) return cleanStop('history_payload_invalid');
      let marker: HistoryMarker;
      try {
        if (sequence.marker) sequence.marker.rearm();
        else sequence.marker = loadCore().armPendingMarker(getCcsDir(), id, platform);
        marker = sequence.marker;
      } catch (error) {
        // No remote append was invoked. Existing/unknown markers hold Open;
        // lack of private marker storage alone skips this optional copy.
        if (claudeHistoryOpenHeld(id, platform)) throw new ValidationError('append_pending');
        return cleanStop(
          object(error) && error.reason === 'history_marker_store_full'
            ? 'history_marker_store_full'
            : 'history_marker_unavailable'
        );
      }
      marker.assertBound();
      const batchCount = Array.isArray(payload.records) ? payload.records.length : null;
      const totalCount = payload.totalCount ?? batchCount;
      const confirmedCount = payload.confirmedCount ?? 0;
      // Counts describe the whole neutral plan, never just this batch.
      // Invalid observer metadata is ignored; it cannot affect mutation.
      if (
        batchCount !== null &&
        Number.isSafeInteger(totalCount) &&
        Number.isSafeInteger(confirmedCount) &&
        typeof totalCount === 'number' &&
        typeof confirmedCount === 'number' &&
        totalCount >= batchCount &&
        totalCount <= 200 &&
        confirmedCount >= 0 &&
        confirmedCount + batchCount <= totalCount
      )
        notifyObserver(() => observer.copying?.(totalCount, confirmedCount));
      const result = await call(platform, 'append', {
        expectedTarget: wireSnapshot(payload.target),
        records: (wireSnapshot({ records: payload.records }) as { records: unknown }).records,
      });
      // Only the fixed helper's successful terminal response says its
      // sole Node mutator is finished/killed and reaped. Lost/malformed
      // responses retain the durable hold; no automatic retry occurs.
      marker.finish(result);
      return result;
    };
    try {
      const result = await loadCore().synchronizeBeforeProfileOpen({
        profileId: id,
        targetPlatform: platform,
        policy,
        adapters: {
          isTargetClosed: async () => {
            const response = await call(platform, 'closed-check');
            return object(response) && response.closed === true;
          },
          readSource: async (_id, source) =>
            decodeSnapshot(await call(source as 'mac' | 'windows', 'collect')),
          readTarget: async () => decodeSnapshot(await call(platform, 'collect')),
          verifyTranscriptReferences: async (_id, source, records) => {
            const response = await call(source as 'mac' | 'windows', 'verify-transcripts', {
              records: (wireSnapshot({ records }) as { records: unknown }).records,
            });
            return object(response) && response.verified === true;
          },
          targetProtectedStateUnchanged: async (target) => {
            const response = await call(platform, 'protected-check', {
              expectedTarget: wireSnapshot(target),
            });
            return object(response) && response.unchanged === true;
          },
          appendCreateOnly,
        },
      });
      // After each copy that created records, retention runs once for this profile: the newest 3
      // snapshot folders stay, older ones the helper created go. It never fails the copy or Open;
      // Settings can turn it off, and "Clean up now" runs it on demand.
      if (result.createdCount > 0) {
        let auto = true;
        try {
          auto = readDashboardPreferences().snapshotCleanup.auto !== false;
        } catch {
          auto = true;
        }
        if (auto) {
          try {
            const cleanup = await cleanupHistorySnapshots(profile, platform);
            logHistory('info', 'claude.history.snapshots_retained', 'History snapshots retained', {
              platform,
              kept: cleanup.kept,
              deleted: cleanup.deleted,
              skipped: cleanup.skipped,
            });
          } catch {
            logHistory(
              'warn',
              'claude.history.snapshots_retain_failed',
              'History snapshot retention did not run',
              { platform }
            );
          }
        }
      }
      if (
        sequence.stopReason &&
        result.status === 'refused' &&
        result.reason === 'create_only_transaction_refused' &&
        result.recoveryRequired !== true
      ) {
        // Its own reason and log line, so a full marker store or a policy change
        // is visible rather than a silent skip.
        const reason = sequence.stopReason;
        const full = reason === 'history_marker_store_full';
        logHistory(
          full ? 'warn' : 'info',
          full ? 'claude.history.marker_store_full' : 'claude.history.copy_stopped',
          full
            ? 'Claude history marker store is full; nothing was copied'
            : 'Claude history copy stopped before an append',
          { reason, platform, confirmedCount: result.createdCount }
        );
        return { ...result, reason };
      }
      if (result.status === 'partial')
        logHistory(
          'info',
          'claude.history.copy_budget_reached',
          'Claude history copy paused for this Open',
          {
            platform,
            confirmedCount: result.createdCount,
            remainingCount: result.remainingCount,
          }
        );
      return result;
    } finally {
      sequence.marker?.release();
    }
  } catch {
    return skipped('unavailable');
  }
}
