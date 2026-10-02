import fs from 'fs';
import path from 'path';
import { createHash } from 'crypto';
import { getCcsDir } from '../../utils/config-manager';
import type { ClaudeDesktopProfile } from './claude-desktop-profile-service';
import { runClaudeHistoryHelper } from './claude-desktop-transport';
import { ValidationError } from '../../errors/error-types';

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

/** Check before policy lookup and after every optional-copy outcome. A removed
 * policy, repeated click or server restart cannot clear an uncertain append.
 */
export function claudeHistoryOpenHeld(profileId: string, platform: 'mac' | 'windows'): boolean {
  // Optional history copying supports only these managed profiles. Other Mac
  // launchers never arm a history transaction and must retain ordinary Open.
  if (!['platyr', 'gmail', 'party', 'me'].includes(profileId)) return false;
  try {
    return loadCore().pendingMarkerState(getCcsDir(), profileId, platform).held;
  } catch {
    try {
      fs.lstatSync(path.join(getCcsDir(), 'claude-history-pending'));
      return true;
    } catch (error) {
      return (error as NodeJS.ErrnoException).code !== 'ENOENT';
    }
  }
}

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
  if (!profile.id || !['platyr', 'gmail', 'party', 'me'].includes(profile.id)) return null;
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
    const sequence: { marker: HistoryMarker | null } = { marker: null };
    const appendCreateOnly = async (payload: unknown): Promise<unknown> => {
      const currentPolicy = await loadClaudeHistoryPolicy(profile);
      if (!currentPolicy || JSON.stringify(currentPolicy) !== JSON.stringify(policy))
        throw new ValidationError('policy_changed');
      if (!object(payload)) throw new ValidationError('helper_unavailable');
      let marker: HistoryMarker;
      try {
        if (sequence.marker) sequence.marker.rearm();
        else sequence.marker = loadCore().armPendingMarker(getCcsDir(), id, platform);
        marker = sequence.marker;
      } catch {
        // No remote append was invoked. Existing/unknown markers hold Open;
        // lack of private marker storage alone skips this optional copy.
        if (claudeHistoryOpenHeld(id, platform)) throw new ValidationError('append_pending');
        return {
          status: 'refused',
          createdCount: 0,
          recoveryRequired: false,
          ownedFilesRolledBack: true,
        };
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
      return await loadCore().synchronizeBeforeProfileOpen({
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
    } finally {
      sequence.marker?.release();
    }
  } catch {
    return skipped('unavailable');
  }
}
