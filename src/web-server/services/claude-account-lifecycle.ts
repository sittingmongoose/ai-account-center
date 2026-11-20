import { runClaudeWorkflow } from './claude-lifecycle-workflow';
import { lifecyclePlan } from '../../shared/private-file-lease';
import { serialized } from './account-lifecycle-queue';
import { randomBytes } from 'crypto';
import { ConfigError } from '../../errors/error-types';
import { stateFingerprint } from './account-confirmations';
import {
  CLAUDE_HOSTS,
  CLAUDE_PROFILE_ID,
  readInventoryEntries,
  readPendingProfiles,
  readTrash,
  updateInventory,
  updatePendingProfiles,
  updateTrash,
  type ClaudeHost,
  type ClaudeHostLauncher,
  type ClaudeTrashEntry,
  type PendingClaudeProfile,
} from './claude-account-stores';
import type { ClaudeHostTransport } from './claude-host-transport';
import { claudeHostPurgePermit } from './agent-verbs';
import {
  inventoryRecord,
  pendingRecord,
  stamp,
  trashEntryIsDefault,
  trashName,
  type ClaudeProfileRecord,
  type PublicTrashEntry,
} from './claude-account-records';

export type { ClaudeProfileRecord, PublicTrashEntry } from './claude-account-records';

/**
 * Claude Add, Remove, trash, restore and purge (CONTRACT-registry-lifecycle
 * 6.2, 6.7 and 6.8).
 *
 * - Add creates the data folder and launchers on both hosts, Mac first. If the
 *   second host fails, the first host's launcher and its empty data folder are
 *   removed again: both or nothing. The profile waits in the pending store
 *   until its first reading confirms an email.
 * - Remove renames each host's data folder into that host's trash (same
 *   volume only; never a copy), removes the launchers and the Windows task,
 *   then moves the store entry into the 30-day trash. A failure part-way puts
 *   back what was moved.
 * - Purge deletes only the exact trash folder of an entry past `purgeAfter`;
 *   an unreachable host keeps the entry, shown as deleting, for the next sweep.
 * Every host step goes through the transport; tests use a fake one.
 */
export const CLAUDE_TRASH_DAYS = 30;
const DAY_MS = 24 * 60 * 60_000;

export type ClaudeLifecycleCode =
  | 'not_implemented'
  | 'not_configured'
  | 'id_in_use'
  | 'host_unreachable'
  | 'unknown_account'
  | 'not_removable'
  | 'account_protected'
  | 'app_running'
  | 'app_state_unknown'
  | 'trash_cross_volume'
  | 'remove_failed'
  | 'unknown_trash'
  | 'confirmation_stale'
  | 'restore_failed'
  | 'purge_failed'
  | 'write_failed';

export class ClaudeLifecycleError extends Error {
  constructor(
    readonly code: ClaudeLifecycleCode,
    readonly host: ClaudeHost | null = null
  ) {
    super(code);
    this.name = 'ClaudeLifecycleError';
  }
}

export function isClaudeEmail(value: unknown): value is string {
  return (
    typeof value === 'string' && value.length <= 254 && /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(value)
  );
}

export interface ClaudeLifecycleDeps {
  ccsDir: () => string;
  transport: ClaudeHostTransport;
  /** False until the host helpers are deployed and a supervised dry run passed. */
  enabled: boolean;
  now?: () => number;
}

export class ClaudeAccountLifecycle {
  constructor(private readonly deps: ClaudeLifecycleDeps) {}

  get enabled(): boolean {
    return this.deps.enabled;
  }

  private now(): number {
    return (this.deps.now ?? Date.now)();
  }

  private ccsDir(): string {
    return this.deps.ccsDir();
  }

  private assertEnabled(): void {
    if (!this.deps.enabled) throw new ClaudeLifecycleError('not_implemented');
  }

  async listPending(): Promise<PendingClaudeProfile[]> {
    return readPendingProfiles(this.ccsDir());
  }

  async listTrash(): Promise<PublicTrashEntry[]> {
    return (await readTrash(this.ccsDir())).map((entry) => ({
      trashId: entry.trashId,
      provider: entry.provider,
      label: entry.label,
      trashedAt: entry.trashedAt,
      purgeAfter: entry.purgeAfter,
      state: entry.state,
    }));
  }

  /** Every inventory profile plus every pending one. */
  async count(): Promise<number> {
    return (await readInventoryEntries(this.ccsDir())).length + (await this.listPending()).length;
  }

  /** An inventory profile with an id, or a pending one. */
  async findProfile(id: string): Promise<ClaudeProfileRecord | null> {
    const inventory = (await readInventoryEntries(this.ccsDir()))
      .map(inventoryRecord)
      .find((candidate) => candidate?.id === id);
    if (inventory) return inventory;
    const pending = (await this.listPending()).find((candidate) => candidate.id === id);
    return pending ? pendingRecord(pending) : null;
  }

  private async idInUse(id: string): Promise<boolean> {
    const ids = new Set<string>();
    for (const entry of await readInventoryEntries(this.ccsDir())) {
      if (typeof entry.id === 'string') ids.add(entry.id.toLowerCase());
    }
    for (const profile of await this.listPending()) ids.add(profile.id);
    for (const entry of await readTrash(this.ccsDir())) ids.add(entry.accountId.slice(7));
    return ids.has(id.toLowerCase());
  }

  /** The ssh alias each host's existing launchers use; Add needs both. */
  private async hostAliases(): Promise<Record<ClaudeHost, string> | null> {
    const aliases: Partial<Record<ClaudeHost, string>> = {};
    for (const entry of await readInventoryEntries(this.ccsDir())) {
      for (const host of CLAUDE_HOSTS) {
        const alias = (entry[host] as Record<string, unknown> | undefined)?.sshHost;
        if (
          !aliases[host] &&
          typeof alias === 'string' &&
          /^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(alias)
        ) {
          aliases[host] = alias;
        }
      }
    }
    return aliases.mac && aliases.windows ? { mac: aliases.mac, windows: aliases.windows } : null;
  }

  async add(input: {
    profileId: string;
    label: string | null;
    email: string;
  }): Promise<PendingClaudeProfile> {
    this.assertEnabled();
    if (!CLAUDE_PROFILE_ID.test(input.profileId)) throw new ClaudeLifecycleError('id_in_use');
    if (!isClaudeEmail(input.email)) throw new ConfigError('The asserted email is not valid.');
    if (await this.idInUse(input.profileId)) throw new ClaudeLifecycleError('id_in_use');
    const aliases = await this.hostAliases();
    if (!aliases) throw new ClaudeLifecycleError('not_configured');
    const created: Partial<Record<ClaudeHost, ClaudeHostLauncher>> = {};
    const root = this.ccsDir();
    const pending = () => ({
      id: input.profileId,
      label: input.label,
      mac: created.mac as ClaudeHostLauncher,
      windows: created.windows as ClaudeHostLauncher,
      createdAt: stamp(this.now()),
      expectedEmail: input.email,
    });
    let profile: PendingClaudeProfile | null = null;
    await runClaudeWorkflow(root, 'add', async (effect) => {
      if (effect === 'commit') {
        const record = pending();
        try {
          await updatePendingProfiles(root, async (profiles) => {
            if (profiles.some((candidate) => candidate.id === record.id)) {
              throw new ClaudeLifecycleError('id_in_use');
            }
            return { next: [...profiles, record], result: undefined };
          });
        } catch (error) {
          throw error instanceof ClaudeLifecycleError
            ? error
            : new ClaudeLifecycleError('write_failed');
        }
        profile = record;
        return;
      }
      const host = effect.endsWith('-mac') ? 'mac' : 'windows';
      if (effect.startsWith('undo-')) {
        const launcher = created[host];
        if (launcher)
          await this.deps.transport.undoCreate(host, { profileId: input.profileId, launcher });
        return;
      }
      try {
        created[host] = await this.deps.transport.create(host, {
          profileId: input.profileId,
          sshHost: aliases[host],
        });
      } catch {
        throw new ClaudeLifecycleError('host_unreachable', host);
      }
    });
    if (!profile) throw new ClaudeLifecycleError('write_failed');
    return profile;
  }

  /**
   * The first refusal a remove would hit; `checkHosts` asks each host whether the app runs. A computer's
   * default Claude profile (a launcher marked `isDefault`) is removed only through the typed-email
   * confirmation in account-lifecycle-removal.ts: `account_protected` unless `allowDefault` is set by
   * that path (which checks the typed email itself before committing).
   */
  async removeRefusal(
    profile: ClaudeProfileRecord,
    checkHosts: boolean,
    allowDefault = false
  ): Promise<'account_protected' | 'app_running' | 'app_state_unknown' | null> {
    if (profile.isDefault && !allowDefault) return 'account_protected';
    if (!checkHosts) return null;
    let unknown = false;
    for (const host of CLAUDE_HOSTS) {
      const launcher = profile.hosts[host];
      if (!launcher) continue;
      const state = await this.deps.transport
        .appState(host, { launcher })
        .catch((): 'unknown' => 'unknown');
      if (state === 'running') return 'app_running';
      if (state !== 'stopped') unknown = true;
    }
    return unknown ? 'app_state_unknown' : null;
  }

  removeFingerprint(profile: ClaudeProfileRecord): string {
    return stateFingerprint({ source: profile.source, entry: profile.entry });
  }

  async remove(
    profile: ClaudeProfileRecord,
    options: { typedEmailConfirmed?: boolean } = {}
  ): Promise<{ trashId: string; purgeAfter: string }> {
    // A default profile is refused before anything else, even if a caller skipped removeRefusal,
    // unless the typed-email confirmation already passed on the remove path.
    if (profile.isDefault && options.typedEmailConfirmed !== true) {
      throw new ClaudeLifecycleError('account_protected');
    }
    this.assertEnabled();
    if (
      profile.incomplete ||
      !CLAUDE_PROFILE_ID.test(profile.id) ||
      Object.keys(profile.hosts).length === 0
    ) {
      throw new ClaudeLifecycleError('not_removable');
    }
    const now = this.now();
    const moved: ClaudeTrashEntry['hosts'] = {};
    const root = this.ccsDir();
    const trashId = `tr_${randomBytes(8).toString('hex')}`;
    const purgeAfter = stamp(now + CLAUDE_TRASH_DAYS * DAY_MS);
    await runClaudeWorkflow(root, 'remove', async (effect) => {
      if (effect === 'commit') {
        const entry: ClaudeTrashEntry = {
          trashId,
          provider: 'claude',
          accountId: `claude:${profile.id}`,
          label: profile.label,
          source: profile.source,
          hosts: moved,
          entry: profile.entry,
          trashedAt: stamp(now),
          purgeAfter,
          state: 'trashed',
        };
        try {
          await updateTrash(root, async (entries) => ({
            next: [...entries, entry],
            result: undefined,
          }));
        } catch {
          throw new ClaudeLifecycleError('remove_failed');
        }
        try {
          await this.dropFromStore(profile);
        } catch {
          await updateTrash(root, async (entries) => ({
            next: entries.filter((candidate) => candidate.trashId !== trashId),
            result: undefined,
          })).catch(() => undefined);
          throw new ClaudeLifecycleError('remove_failed');
        }
        return;
      }
      const host = effect.endsWith('-mac') ? 'mac' : 'windows';
      const launcher = profile.hosts[host];
      if (!launcher) return false;
      const detail = { trashName: trashName(profile.id, now), launcher };
      if (effect.startsWith('undo-')) {
        await this.deps.transport.restore(host, { profileId: profile.id, ...detail });
        return;
      }
      const outcome = await this.deps.transport
        .trash(host, { profileId: profile.id, ...detail })
        .catch(() => {
          throw new ClaudeLifecycleError('remove_failed', host);
        });
      if (outcome === 'cross_volume') throw new ClaudeLifecycleError('trash_cross_volume', host);
      moved[host] = detail;
      return true;
    });
    return { trashId, purgeAfter };
  }

  private async dropFromStore(profile: ClaudeProfileRecord): Promise<void> {
    if (profile.source === 'pending') {
      await updatePendingProfiles(this.ccsDir(), async (profiles) => ({
        next: profiles.filter((candidate) => candidate.id !== profile.id),
        result: undefined,
      }));
      return;
    }
    await updateInventory(this.ccsDir(), (profiles) => ({
      next: profiles.filter((candidate) => candidate.id !== profile.id),
      result: undefined,
    }));
  }

  async findTrash(trashId: string): Promise<ClaudeTrashEntry | null> {
    return (await readTrash(this.ccsDir())).find((entry) => entry.trashId === trashId) ?? null;
  }

  restoreFingerprint(entry: ClaudeTrashEntry): string {
    return stateFingerprint({ trashId: entry.trashId, state: entry.state, entry: entry.entry });
  }

  async restore(trashId: string, reviewed?: string): Promise<{ accountId: string }> {
    this.assertEnabled();
    return updateTrash(this.ccsDir(), async (entries) => {
      const entry = entries.find((candidate) => candidate.trashId === trashId);
      if (!entry || entry.state !== 'trashed') throw new ClaudeLifecycleError('unknown_trash');
      if (reviewed !== undefined && this.restoreFingerprint(entry) !== reviewed) {
        throw new ClaudeLifecycleError('confirmation_stale');
      }
      const id = entry.accountId.slice('claude:'.length);
      const ids = new Set<string>();
      for (const item of await readInventoryEntries(this.ccsDir())) {
        if (typeof item.id === 'string') ids.add(item.id.toLowerCase());
      }
      for (const profile of await this.listPending()) ids.add(profile.id);
      if (ids.has(id)) throw new ClaudeLifecycleError('id_in_use');
      await runClaudeWorkflow(this.ccsDir(), 'restore', async (effect) => {
        if (effect === 'commit') {
          try {
            if (entry.source === 'pending') {
              await updatePendingProfiles(this.ccsDir(), async (profiles) => ({
                next: [...profiles, entry.entry as unknown as PendingClaudeProfile],
                result: undefined,
              }));
            } else {
              await updateInventory(this.ccsDir(), (profiles) => ({
                next: [...profiles, entry.entry],
                result: undefined,
              }));
            }
          } catch {
            throw new ClaudeLifecycleError('write_failed');
          }
          return;
        }
        const host = effect.endsWith('-mac') ? 'mac' : 'windows';
        const detail = entry.hosts[host];
        if (!detail) return false;
        if (effect.startsWith('undo-')) {
          await this.deps.transport.trash(host, { profileId: id, ...detail });
          return;
        }
        await this.deps.transport.restore(host, { profileId: id, ...detail }).catch(() => {
          throw new ClaudeLifecycleError('restore_failed', host);
        });
        return true;
      });
      return {
        next: entries.filter((candidate) => candidate.trashId !== trashId),
        result: { accountId: entry.accountId },
      };
    });
  }

  purgeFingerprint(entry: ClaudeTrashEntry): string {
    return this.restoreFingerprint(entry);
  }

  private async reservePurge(
    trashId: string,
    reviewed?: string,
    dueBy?: number
  ): Promise<ClaudeTrashEntry> {
    try {
      return await updateTrash(this.ccsDir(), async (entries) => {
        const entry = entries.find((candidate) => candidate.trashId === trashId);
        if (!entry || (entry.state !== 'trashed' && entry.state !== 'deleting'))
          throw new ClaudeLifecycleError('unknown_trash');
        if (reviewed !== undefined && this.purgeFingerprint(entry) !== reviewed)
          throw new ClaudeLifecycleError('confirmation_stale');
        if (trashEntryIsDefault(entry)) throw new ClaudeLifecycleError('account_protected');
        if (dueBy !== undefined && !(Date.parse(entry.purgeAfter) <= dueBy))
          throw new ClaudeLifecycleError('unknown_trash');
        const deleting = { ...entry, state: 'deleting' as const };
        return {
          next: entries.map((candidate) => (candidate === entry ? deleting : candidate)),
          result: deleting,
        };
      });
    } catch (error) {
      if (error instanceof ClaudeLifecycleError) throw error;
      throw new ClaudeLifecycleError('purge_failed');
    }
  }

  /**
   * Delete one trash entry now, before its 30 days are up, on both hosts
   * through the same transport as Remove. An unreachable host keeps the entry
   * as `deleting` for the next sweep (or a retry) and answers
   * `host_unreachable`. A default profile is never in the trash; one found
   * there is refused with `account_protected`.
   */
  async purgeOne(trashId: string, reviewed?: string): Promise<{ trashId: string }> {
    this.assertEnabled();
    return serialized(this.ccsDir(), 'claude-purge', () => this.performPurgeOne(trashId, reviewed));
  }

  private async performPurgeOne(trashId: string, reviewed?: string): Promise<{ trashId: string }> {
    const entry = await this.reservePurge(trashId, reviewed);
    await this.purgeHosts(entry, async () => {
      try {
        await updateTrash(this.ccsDir(), async (entries) => ({
          next: entries.filter((candidate) => candidate.trashId !== trashId),
          result: undefined,
        }));
      } catch {
        throw new ClaudeLifecycleError('purge_failed');
      }
    });
    return { trashId };
  }

  private async purgeHosts(entry: ClaudeTrashEntry, commit: () => Promise<void>): Promise<void> {
    await runClaudeWorkflow(this.ccsDir(), 'purge', async (effect) => {
      if (effect === 'commit') return commit();
      const host = effect.endsWith('-mac') ? 'mac' : 'windows';
      const detail = entry.hosts[host];
      if (!detail) return false;
      await this.deps.transport
        .purge(host, {
          sshHost: detail.launcher.sshHost,
          trashName: detail.trashName,
          token: claudeHostPurgePermit(host, detail.trashName),
        })
        .catch(() => {
          throw new ClaudeLifecycleError('host_unreachable', host);
        });
    });
  }

  /** Delete trash past `purgeAfter`; an unreachable host keeps the entry as `deleting`. */
  async purgeDue(): Promise<number> {
    if (!this.deps.enabled) return 0;
    return serialized(this.ccsDir(), 'claude-purge', () => this.sweepDueTrash());
  }

  private async sweepDueTrash(): Promise<number> {
    const now = this.now();
    let due: ClaudeTrashEntry[];
    try {
      const root = this.ccsDir();
      const entries = await readTrash(root);
      const plan = lifecyclePlan(root, {
        operation: 'due-trash',
        now,
        entries: entries.map((entry) => ({ id: entry.trashId, at: Date.parse(entry.purgeAfter) })),
      });
      const selected = plan === undefined ? null : new Set(Array.isArray(plan) ? plan : []);
      due = entries.filter(
        (entry) => Date.parse(entry.purgeAfter) <= now && (!selected || selected.has(entry.trashId))
      );
    } catch {
      return 0;
    }
    const purged = new Set<string>();
    for (const { trashId } of due) {
      let entry: ClaudeTrashEntry;
      try {
        entry = await this.reservePurge(trashId, undefined, now);
      } catch {
        continue;
      }
      await this.purgeHosts(entry, async () => {
        purged.add(entry.trashId);
      }).catch(() => undefined);
    }
    if (purged.size === 0) return 0;
    await updateTrash(this.ccsDir(), async (entries) => ({
      next: entries.filter((entry) => !purged.has(entry.trashId)),
      result: undefined,
    })).catch(() => undefined);
    return purged.size;
  }

  /** The first reading confirmed the email: move the profile into the inventory. */
  async confirmPending(id: string, email: string): Promise<boolean> {
    if (!isClaudeEmail(email)) {
      throw new ConfigError('The confirmed email is not valid.');
    }
    return updatePendingProfiles(this.ccsDir(), async (profiles) => {
      const profile = profiles.find((candidate) => candidate.id === id);
      if (!profile) return { next: profiles, result: false };
      await updateInventory(this.ccsDir(), (entries) => {
        if (entries.some((entry) => entry.id === id)) {
          throw new ConfigError('The Claude profile id is already listed.');
        }
        return {
          next: [
            ...entries,
            { id, email, mac: { ...profile.mac }, windows: { ...profile.windows } },
          ],
          result: undefined,
        };
      });
      return { next: profiles.filter((candidate) => candidate.id !== id), result: true };
    });
  }

  /**
   * Re-check a pending profile after the user signs in: read the sign-in
   * marker on one host, and pin the asserted email when a session is there.
   * The first usage reading verifies the pinned email against the provider.
   * Returns null when the id names nothing, so the route can answer 404.
   */
  async checkPendingSignIn(
    id: string,
    platform: ClaudeHost,
    email: string | null
  ): Promise<{ promoted: boolean; email: string | null } | null> {
    this.assertEnabled();
    const pending = (await readPendingProfiles(this.ccsDir())).find(
      (candidate) => candidate.id === id
    );
    if (!pending) {
      const listed = (await readInventoryEntries(this.ccsDir())).some((entry) => entry.id === id);
      return listed ? { promoted: true, email: null } : null;
    }
    let reply: { promoted: boolean; email: string | null } = {
      promoted: false,
      email: pending.expectedEmail,
    };
    await runClaudeWorkflow(this.ccsDir(), 'recheck', async (effect) => {
      if (effect === 'commit') {
        if (!reply.email) throw new ClaudeLifecycleError('write_failed');
        await this.confirmPending(id, reply.email);
        reply = { ...reply, promoted: true };
        return;
      }
      if (effect !== 'session') throw new ClaudeLifecycleError('write_failed');
      const state = await this.deps.transport
        .sessionState(platform, { launcher: pending[platform] })
        .catch(() => {
          throw new ClaudeLifecycleError('host_unreachable', platform);
        });
      if (state === 'signed-out') return false;
      const address = email ?? pending.expectedEmail;
      reply = { promoted: false, email: isClaudeEmail(address) ? address : null };
      return reply.email !== null;
    });
    return reply;
  }
}
