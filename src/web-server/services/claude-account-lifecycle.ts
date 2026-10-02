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
import {
  inventoryRecord,
  pendingRecord,
  stamp,
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
  | 'account_default'
  | 'app_running'
  | 'app_state_unknown'
  | 'trash_cross_volume'
  | 'remove_failed'
  | 'unknown_trash'
  | 'restore_failed'
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

  async add(input: { profileId: string; label: string | null }): Promise<PendingClaudeProfile> {
    this.assertEnabled();
    if (!CLAUDE_PROFILE_ID.test(input.profileId)) throw new ClaudeLifecycleError('id_in_use');
    if (await this.idInUse(input.profileId)) throw new ClaudeLifecycleError('id_in_use');
    const aliases = await this.hostAliases();
    if (!aliases) throw new ClaudeLifecycleError('not_configured');
    const created: Partial<Record<ClaudeHost, ClaudeHostLauncher>> = {};
    const undo = async () => {
      for (const host of [...CLAUDE_HOSTS].reverse()) {
        const launcher = created[host];
        if (!launcher) continue;
        await this.deps.transport
          .undoCreate(host, { profileId: input.profileId, launcher })
          .catch(() => undefined);
      }
    };
    for (const host of CLAUDE_HOSTS) {
      try {
        created[host] = await this.deps.transport.create(host, {
          profileId: input.profileId,
          sshHost: aliases[host],
        });
      } catch {
        await undo();
        throw new ClaudeLifecycleError('host_unreachable', host);
      }
    }
    const profile: PendingClaudeProfile = {
      id: input.profileId,
      label: input.label,
      mac: created.mac as ClaudeHostLauncher,
      windows: created.windows as ClaudeHostLauncher,
      createdAt: stamp(this.now()),
    };
    try {
      await updatePendingProfiles(this.ccsDir(), async (profiles) => {
        if (profiles.some((candidate) => candidate.id === profile.id)) {
          throw new ClaudeLifecycleError('id_in_use');
        }
        return { next: [...profiles, profile], result: undefined };
      });
    } catch (error) {
      await undo();
      throw error instanceof ClaudeLifecycleError
        ? error
        : new ClaudeLifecycleError('write_failed');
    }
    return profile;
  }

  /** The first refusal a remove would hit; `checkHosts` asks each host whether the app runs. */
  async removeRefusal(
    profile: ClaudeProfileRecord,
    checkHosts: boolean
  ): Promise<'account_default' | 'app_running' | 'app_state_unknown' | null> {
    if (profile.isDefault) return 'account_default';
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

  async remove(profile: ClaudeProfileRecord): Promise<{ trashId: string; purgeAfter: string }> {
    this.assertEnabled();
    if (
      profile.incomplete ||
      !CLAUDE_PROFILE_ID.test(profile.id) ||
      Object.keys(profile.hosts).length === 0
    ) {
      throw new ClaudeLifecycleError('not_removable');
    }
    const now = this.now();
    const moved: Array<[ClaudeHost, ClaudeTrashEntry['hosts'][ClaudeHost]]> = [];
    const putBack = async () => {
      for (const [host, detail] of moved.reverse()) {
        if (!detail) continue;
        await this.deps.transport
          .restore(host, { profileId: profile.id, ...detail })
          .catch(() => undefined);
      }
    };
    for (const host of CLAUDE_HOSTS) {
      const launcher = profile.hosts[host];
      if (!launcher) continue;
      const detail = { trashName: trashName(profile.id, now), launcher };
      let outcome: 'moved' | 'cross_volume';
      try {
        outcome = await this.deps.transport.trash(host, { profileId: profile.id, ...detail });
      } catch {
        await putBack();
        throw new ClaudeLifecycleError('remove_failed', host);
      }
      if (outcome === 'cross_volume') {
        await putBack();
        throw new ClaudeLifecycleError('trash_cross_volume', host);
      }
      moved.push([host, detail]);
    }
    const entry: ClaudeTrashEntry = {
      trashId: `tr_${randomBytes(8).toString('hex')}`,
      provider: 'claude',
      accountId: `claude:${profile.id}`,
      label: profile.label,
      source: profile.source,
      hosts: Object.fromEntries(moved),
      entry: profile.entry,
      trashedAt: stamp(now),
      purgeAfter: stamp(now + CLAUDE_TRASH_DAYS * DAY_MS),
      state: 'trashed',
    };
    try {
      await updateTrash(this.ccsDir(), async (entries) => {
        await this.dropFromStore(profile);
        return { next: [...entries, entry], result: undefined };
      });
    } catch {
      await putBack();
      throw new ClaudeLifecycleError('remove_failed');
    }
    return { trashId: entry.trashId, purgeAfter: entry.purgeAfter };
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

  async restore(trashId: string): Promise<{ accountId: string }> {
    this.assertEnabled();
    return updateTrash(this.ccsDir(), async (entries) => {
      const entry = entries.find((candidate) => candidate.trashId === trashId);
      if (!entry || entry.state !== 'trashed') throw new ClaudeLifecycleError('unknown_trash');
      const id = entry.accountId.slice('claude:'.length);
      const ids = new Set<string>();
      for (const item of await readInventoryEntries(this.ccsDir())) {
        if (typeof item.id === 'string') ids.add(item.id.toLowerCase());
      }
      for (const profile of await this.listPending()) ids.add(profile.id);
      if (ids.has(id)) throw new ClaudeLifecycleError('id_in_use');
      const restored: ClaudeHost[] = [];
      for (const host of CLAUDE_HOSTS) {
        const detail = entry.hosts[host];
        if (!detail) continue;
        try {
          await this.deps.transport.restore(host, { profileId: id, ...detail });
          restored.push(host);
        } catch {
          for (const done of restored) {
            const back = entry.hosts[done];
            if (back) {
              await this.deps.transport
                .trash(done, { profileId: id, ...back })
                .catch(() => undefined);
            }
          }
          throw new ClaudeLifecycleError('restore_failed', host);
        }
      }
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
      return {
        next: entries.filter((candidate) => candidate.trashId !== trashId),
        result: { accountId: entry.accountId },
      };
    });
  }

  /** Delete trash past `purgeAfter`; an unreachable host keeps the entry as `deleting`. */
  async purgeDue(): Promise<number> {
    if (!this.deps.enabled) return 0;
    const now = this.now();
    let due: ClaudeTrashEntry[];
    try {
      due = (await readTrash(this.ccsDir())).filter((entry) => Date.parse(entry.purgeAfter) <= now);
    } catch {
      return 0;
    }
    const purged = new Set<string>();
    const pending = new Set<string>();
    for (const entry of due) {
      let complete = true;
      for (const host of CLAUDE_HOSTS) {
        const detail = entry.hosts[host];
        if (!detail) continue;
        try {
          await this.deps.transport.purge(host, {
            sshHost: detail.launcher.sshHost,
            trashName: detail.trashName,
          });
        } catch {
          complete = false;
        }
      }
      (complete ? purged : pending).add(entry.trashId);
    }
    if (purged.size === 0 && pending.size === 0) return 0;
    await updateTrash(this.ccsDir(), async (entries) => ({
      next: entries
        .filter((entry) => !purged.has(entry.trashId))
        .map(
          (entry): ClaudeTrashEntry =>
            pending.has(entry.trashId) ? { ...entry, state: 'deleting' } : entry
        ),
      result: undefined,
    })).catch(() => undefined);
    return purged.size;
  }

  /** The first reading confirmed the email: move the profile into the inventory. */
  async confirmPending(id: string, email: string): Promise<boolean> {
    if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email) || email.length > 254) {
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
}
