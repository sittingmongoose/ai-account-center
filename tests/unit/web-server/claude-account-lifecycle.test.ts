/**
 * Claude Add, Remove, trash, restore and purge (CONTRACT-registry-lifecycle
 * 6.2, 6.7, 6.8) against a fake host transport and a temporary CCS folder.
 * Nothing here reaches a real host.
 */
import { afterEach, describe, expect, it, mock, spyOn } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { ProfileError } from '../../../src/errors/error-types';
import { listClaudeDesktopProfiles } from '../../../src/web-server/services/claude-desktop-profile-service';
import { openClaudeDesktopProfile } from '../../../src/web-server/services/claude-desktop-open-service';
import * as desktopTransport from '../../../src/web-server/services/claude-desktop-transport';
import {
  ClaudeAccountLifecycle,
  ClaudeLifecycleError,
  CLAUDE_TRASH_DAYS,
} from '../../../src/web-server/services/claude-account-lifecycle';
import type {
  ClaudeHostStep,
  ClaudeHostTransport,
} from '../../../src/web-server/services/claude-host-transport';
import type {
  ClaudeHost,
  ClaudeHostLauncher,
} from '../../../src/web-server/services/claude-account-stores';
import {
  inventoryRecord,
  isDefaultClaudeDataFolder,
  pendingRecord,
} from '../../../src/web-server/services/claude-account-records';

const ORIGINAL_CCS_HOME = process.env.CCS_HOME;
const dirs: string[] = [];
afterEach(() => {
  if (ORIGINAL_CCS_HOME === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = ORIGINAL_CCS_HOME;
  for (const dir of dirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true });
  mock.restore();
});

const DAY = 24 * 60 * 60_000;
const START = Date.parse('2026-10-02T08:00:00Z');

/** Hosts as plain sets: data folders, launchers and trash folders by name. */
class FakeHosts implements ClaudeHostTransport {
  calls: string[] = [];
  data: Record<ClaudeHost, Set<string>> = { mac: new Set(), windows: new Set() };
  launchers: Record<ClaudeHost, Set<string>> = { mac: new Set(), windows: new Set() };
  trashDirs: Record<ClaudeHost, Set<string>> = { mac: new Set(), windows: new Set() };
  fail: Partial<Record<string, ClaudeHost>> = {};
  running = new Set<string>();
  sessions = new Set<string>();
  unknown = new Set<string>();
  crossVolume = new Set<ClaudeHost>();
  /** The Windows helper allowlist, in file order (the first id is the default). */
  accounts: string[] = ['gmail', 'party'];

  private check(op: string, host: ClaudeHost) {
    this.calls.push(`${op}:${host}`);
    if (this.fail[op] === host) throw new Error('ssh: connect to host failed /private/path');
  }

  async create(host: ClaudeHost, input: { profileId: string; sshHost: string }) {
    this.check('create', host);
    const launcher: ClaudeHostLauncher = {
      launcherName: `Claude (${input.profileId})`,
      launcherPath: `/fake/${host}/Claude (${input.profileId}).app`,
      profilePath: `/fake/${host}/Claude-${input.profileId}`,
      sshHost: input.sshHost,
    };
    this.data[host].add(launcher.profilePath);
    this.launchers[host].add(launcher.launcherPath as string);
    // The Windows host step appends the id to ccs-claude-accounts.txt, once.
    if (host === 'windows' && !this.accounts.includes(input.profileId)) {
      this.accounts.push(input.profileId);
    }
    return launcher;
  }

  async undoCreate(host: ClaudeHost, input: ClaudeHostStep) {
    this.check('undo', host);
    this.data[host].delete(input.launcher.profilePath);
    this.launchers[host].delete(input.launcher.launcherPath as string);
    if (host === 'windows') {
      this.accounts = this.accounts.filter((id) => id !== input.profileId);
    }
  }

  async appState(host: ClaudeHost, input: { launcher: ClaudeHostLauncher }) {
    this.check('state', host);
    if (this.unknown.has(input.launcher.profilePath)) throw new Error('unreachable');
    return this.running.has(input.launcher.profilePath) ? 'running' : 'stopped';
  }

  async sessionState(host: ClaudeHost, input: { launcher: ClaudeHostLauncher }) {
    this.check('session', host);
    if (this.unknown.has(input.launcher.profilePath)) throw new Error('unreachable');
    return this.sessions.has(input.launcher.profilePath) ? 'signed-in' : 'signed-out';
  }

  async trash(host: ClaudeHost, input: ClaudeHostStep & { trashName: string }) {
    this.check('trash', host);
    if (this.crossVolume.has(host)) return 'cross_volume' as const;
    this.data[host].delete(input.launcher.profilePath);
    this.launchers[host].delete(input.launcher.launcherPath as string);
    this.trashDirs[host].add(input.trashName);
    return 'moved' as const;
  }

  async restore(host: ClaudeHost, input: ClaudeHostStep & { trashName: string }) {
    this.check('restore', host);
    this.trashDirs[host].delete(input.trashName);
    this.data[host].add(input.launcher.profilePath);
    this.launchers[host].add(input.launcher.launcherPath as string);
  }

  async purge(host: ClaudeHost, input: { sshHost: string; trashName: string }) {
    this.check('purge', host);
    this.trashDirs[host].delete(input.trashName);
  }
}

function setup(enabled = true) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-claude-lifecycle-'));
  dirs.push(root);
  process.env.CCS_HOME = root;
  const ccsDir = path.join(root, '.ccs');
  fs.mkdirSync(ccsDir, { recursive: true });
  const inventory = {
    version: 1,
    note: 'kept as is',
    profiles: [
      {
        id: 'gmail',
        email: 'gmail@example.com',
        mac: {
          launcherName: 'Claude-gmail',
          launcherPath: '/fake/mac/Claude (gmail@example.com).app',
          profilePath: '/fake/mac/Claude-gmail',
          sshHost: 'jared-mac',
        },
        windows: { launcherName: 'Claude', isDefault: true, sshHost: 'jared-windows' },
      },
      {
        id: 'party',
        email: 'party@example.com',
        mac: {
          launcherName: 'Claude-party',
          launcherPath: '/fake/mac/Claude (party@example.com).app',
          profilePath: '/fake/mac/Claude-party',
          sshHost: 'jared-mac',
        },
        windows: {
          launcherName: 'Claude (party)',
          launcherPath: 'C:\\Users\\x\\Desktop\\Claude (party@example.com).lnk',
          profilePath: 'C:\\Users\\x\\AppData\\Roaming\\Claude-party',
          sshHost: 'jared-windows',
        },
      },
    ],
  };
  fs.writeFileSync(
    path.join(ccsDir, 'claude-desktop-profiles.json'),
    JSON.stringify(inventory, null, 2)
  );
  const hosts = new FakeHosts();
  hosts.data.mac.add('/fake/mac/Claude-party');
  hosts.data.windows.add('C:\\Users\\x\\AppData\\Roaming\\Claude-party');
  let now = START;
  const lifecycle = new ClaudeAccountLifecycle({
    ccsDir: () => ccsDir,
    transport: hosts,
    enabled,
    now: () => now,
  });
  return {
    ccsDir,
    hosts,
    lifecycle,
    advance: (ms: number) => {
      now += ms;
    },
    inventory: () =>
      JSON.parse(fs.readFileSync(path.join(ccsDir, 'claude-desktop-profiles.json'), 'utf8')),
  };
}

describe('Claude Add', () => {
  it('creates launchers on both hosts and waits in the pending store', async () => {
    const { ccsDir, hosts, lifecycle } = setup();
    const profile = await lifecycle.add({ profileId: 'work2', label: 'Work 2', email: 'work2@example.com' });
    expect(profile).toMatchObject({
      id: 'work2',
      label: 'Work 2',
      createdAt: '2026-10-02T08:00:00Z',
    });
    expect(profile.mac.sshHost).toBe('jared-mac');
    expect(profile.windows.sshHost).toBe('jared-windows');
    expect(hosts.calls).toEqual(['create:mac', 'create:windows']);
    const pending = path.join(ccsDir, 'accounts', 'claude-pending.json');
    expect(fs.statSync(pending).mode & 0o777).toBe(0o600);
    expect(fs.statSync(path.dirname(pending)).mode & 0o777).toBe(0o700);
    expect((await lifecycle.listPending()).map((entry) => entry.id)).toEqual(['work2']);
    expect(await lifecycle.count()).toBe(3);
    // The version 1 inventory is untouched and still parses.
    expect((await listClaudeDesktopProfiles()).map((entry) => entry.id)).toEqual([
      'gmail',
      'party',
    ]);
  });

  it('is all or nothing: a failing second host removes the first host again', async () => {
    const { hosts, lifecycle } = setup();
    hosts.fail.create = 'windows';
    await expect(lifecycle.add({ profileId: 'work2', label: null, email: 'work2@example.com' })).rejects.toMatchObject({
      code: 'host_unreachable',
      host: 'windows',
    });
    expect(hosts.calls).toEqual(['create:mac', 'create:windows', 'undo:mac']);
    expect([...hosts.data.mac]).toEqual(['/fake/mac/Claude-party']);
    expect(hosts.launchers.mac.size).toBe(0);
    expect(await lifecycle.listPending()).toEqual([]);
  });

  it('lists a new id in the Windows helper allowlist, after the default', async () => {
    const { hosts, lifecycle } = setup();
    await lifecycle.add({ profileId: 'work2', label: null, email: 'work2@example.com' });
    // The default stays first; the new id is appended, never reordered.
    expect(hosts.accounts).toEqual(['gmail', 'party', 'work2']);
  });

  it('leaves the allowlist unchanged when the Windows step fails', async () => {
    const { hosts, lifecycle } = setup();
    hosts.fail.create = 'windows';
    await expect(lifecycle.add({ profileId: 'work2', label: null, email: 'work2@example.com' })).rejects.toMatchObject({
      code: 'host_unreachable',
      host: 'windows',
    });
    expect(hosts.accounts).toEqual(['gmail', 'party']);
    expect(await lifecycle.listPending()).toEqual([]);
  });

  it('refuses an id in use (inventory, pending or trash, any case) and runs nothing', async () => {
    const { hosts, lifecycle } = setup();
    await lifecycle.add({ profileId: 'work2', label: null, email: 'work2@example.com' });
    hosts.calls = [];
    for (const id of ['party', 'work2']) {
      await expect(lifecycle.add({ profileId: id, label: null, email: 'work2@example.com' })).rejects.toMatchObject({
        code: 'id_in_use',
      });
    }
    expect(hosts.calls).toEqual([]);
  });

  it('answers not_implemented while host steps are switched off', async () => {
    const { hosts, lifecycle } = setup(false);
    await expect(lifecycle.add({ profileId: 'work2', label: null, email: 'work2@example.com' })).rejects.toMatchObject({
      code: 'not_implemented',
    });
    const party = await lifecycle.findProfile('party');
    await expect(lifecycle.remove(party as never)).rejects.toMatchObject({
      code: 'not_implemented',
    });
    expect(await lifecycle.purgeDue()).toBe(0);
    expect(hosts.calls).toEqual([]);
  });

  it('moves a pending profile into the inventory once its email is confirmed', async () => {
    const { lifecycle, inventory } = setup();
    await lifecycle.add({ profileId: 'work2', label: 'Work 2', email: 'work2@example.com' });
    expect(await lifecycle.confirmPending('work2', 'work2@example.com')).toBe(true);
    expect(await lifecycle.listPending()).toEqual([]);
    expect(inventory().note).toBe('kept as is');
    const parsed = await listClaudeDesktopProfiles();
    expect(parsed.map((entry) => [entry.id, entry.email])).toEqual([
      ['gmail', 'gmail@example.com'],
      ['party', 'party@example.com'],
      ['work2', 'work2@example.com'],
    ]);
    expect(parsed[2].mac?.sshHost).toBe('jared-mac');
    expect(await lifecycle.confirmPending('work2', 'work2@example.com')).toBe(false);
  });

  it('re-check pins the asserted email once the chosen host reports a session', async () => {
    const { hosts, lifecycle } = setup();
    await lifecycle.add({ profileId: 'work2', label: 'Work 2', email: 'work2@example.com' });
    // No session on the Mac yet: still pending, assertion kept.
    expect(await lifecycle.checkPendingSignIn('work2', 'mac', null)).toEqual({
      promoted: false,
      email: 'work2@example.com',
    });
    expect(hosts.calls).toEqual(['create:mac', 'create:windows', 'session:mac']);
    // The user signs in on the Mac: the profile promotes with its email.
    hosts.sessions.add('/fake/mac/Claude-work2');
    expect(await lifecycle.checkPendingSignIn('work2', 'mac', null)).toEqual({
      promoted: true,
      email: 'work2@example.com',
    });
    expect(await lifecycle.listPending()).toEqual([]);
    const parsed = await listClaudeDesktopProfiles();
    expect(parsed.map((entry) => [entry.id, entry.email])).toContainEqual([
      'work2',
      'work2@example.com',
    ]);
    // A listed id answers found; an unknown id answers nothing.
    expect(await lifecycle.checkPendingSignIn('work2', 'mac', null)).toEqual({
      promoted: true,
      email: null,
    });
    expect(await lifecycle.checkPendingSignIn('nobody', 'mac', null)).toBeNull();
  });

  it('re-check answers host_unreachable when the chosen host cannot be read', async () => {
    const { hosts, lifecycle } = setup();
    await lifecycle.add({ profileId: 'work2', label: 'Work 2', email: 'work2@example.com' });
    hosts.fail = { session: 'mac' };
    await expect(lifecycle.checkPendingSignIn('work2', 'mac', null)).rejects.toMatchObject({
      code: 'host_unreachable',
    });
  });
});

describe('Pending profiles open on both hosts', () => {
  it('opens a newly added id on Mac and on Windows; an unknown id is refused', async () => {
    const { lifecycle } = setup();
    await lifecycle.add({ profileId: 'work2', label: 'Work 2', email: 'work2@example.com' });
    const macOpen = spyOn(desktopTransport, 'openClaudeMacLauncher').mockResolvedValue(undefined);
    const windowsOpen = spyOn(desktopTransport, 'openClaudeWindowsLauncher').mockResolvedValue(
      undefined
    );
    await openClaudeDesktopProfile('work2', 'mac');
    expect(macOpen).toHaveBeenCalledTimes(1);
    expect(macOpen).toHaveBeenCalledWith({
      launcherName: 'Claude (work2)',
      launcherPath: '/fake/mac/Claude (work2).app',
      profilePath: '/fake/mac/Claude-work2',
      sshHost: 'jared-mac',
    });
    await openClaudeDesktopProfile('work2', 'windows');
    expect(windowsOpen).toHaveBeenCalledTimes(1);
    expect(windowsOpen).toHaveBeenCalledWith(
      {
        launcherName: 'Claude (work2)',
        launcherPath: '/fake/windows/Claude (work2).app',
        profilePath: '/fake/windows/Claude-work2',
        sshHost: 'jared-windows',
      },
      'work2',
      // The launcher's account list is rebuilt from the current profiles in the same call:
      // the Windows default first, then the others, the new pending profile included.
      ['gmail', 'party', 'work2']
    );
    // In neither the pending registry nor the manifest: refused, nothing opens.
    // A malformed id is refused the same way.
    for (const id of ['no-such-id', 'bad;id']) {
      await expect(openClaudeDesktopProfile(id, 'mac')).rejects.toBeInstanceOf(ProfileError);
      await expect(openClaudeDesktopProfile(id, 'windows')).rejects.toBeInstanceOf(ProfileError);
    }
    expect(macOpen).toHaveBeenCalledTimes(1);
    expect(windowsOpen).toHaveBeenCalledTimes(1);
  });
});

describe('Claude Remove and trash', () => {
  it('refuses the default launcher, a running app and an unknown app state', async () => {
    const { hosts, lifecycle } = setup();
    const gmail = await lifecycle.findProfile('gmail');
    expect(await lifecycle.removeRefusal(gmail as never, true)).toBe('account_protected');
    // even a caller that skips the refusal cannot remove a computer's default profile
    await expect(lifecycle.remove(gmail as never)).rejects.toMatchObject({
      code: 'account_protected',
    });
    expect(hosts.calls.filter((call) => call.startsWith('trash'))).toEqual([]);
    const party = await lifecycle.findProfile('party');
    hosts.running.add('C:\\Users\\x\\AppData\\Roaming\\Claude-party');
    expect(await lifecycle.removeRefusal(party as never, true)).toBe('app_running');
    hosts.running.clear();
    hosts.unknown.add('/fake/mac/Claude-party');
    expect(await lifecycle.removeRefusal(party as never, true)).toBe('app_state_unknown');
    hosts.unknown.clear();
    expect(await lifecycle.removeRefusal(party as never, true)).toBeNull();
    expect(await lifecycle.removeRefusal(party as never, false)).toBeNull();
  });

  it("refuses a computer's default profile even when the inventory lost its isDefault flag", async () => {
    const { ccsDir, hosts, lifecycle, inventory } = setup();
    // the app's own data folder on either host is the default profile, whatever the flag says
    const document = inventory();
    document.profiles.push({
      id: 'desk',
      email: 'desk@example.com',
      mac: {
        launcherName: 'Claude',
        profilePath: '/Users/x/Library/Application Support/Claude/',
        sshHost: 'jared-mac',
      },
      windows: {
        launcherName: 'Claude (desk)',
        profilePath: 'C:\\Users\\x\\AppData\\Roaming\\Claude-desk',
        sshHost: 'jared-windows',
      },
    });
    fs.writeFileSync(
      path.join(ccsDir, 'claude-desktop-profiles.json'),
      JSON.stringify(document, null, 2)
    );
    const desk = await lifecycle.findProfile('desk');
    expect(desk?.isDefault).toBe(true);
    expect(await lifecycle.removeRefusal(desk as never, true)).toBe('account_protected');
    await expect(lifecycle.remove(desk as never)).rejects.toMatchObject({
      code: 'account_protected',
    });
    expect(hosts.calls.filter((call) => call.startsWith('trash'))).toEqual([]);
    expect(inventory().profiles.map((entry: { id: string }) => entry.id)).toContain('desk');
  });

  it('reads the default data folder by its last path part, on either separator, in any case', () => {
    for (const value of [
      '/Users/x/Library/Application Support/Claude',
      '/Users/x/Library/Application Support/Claude/',
      'C:\\Users\\x\\AppData\\Roaming\\Claude',
      'C:\\Users\\x\\AppData\\Roaming\\claude\\',
      'C:/Users/x/AppData/Roaming/CLAUDE',
    ]) {
      expect(isDefaultClaudeDataFolder(value)).toBe(true);
    }
    for (const value of [
      '/Users/x/Library/Application Support/Claude-party',
      'C:\\Users\\x\\AppData\\Roaming\\Claude (party)',
      '/Users/x/Claude/party',
      '',
      null,
      42,
    ]) {
      expect(isDefaultClaudeDataFolder(value)).toBe(false);
    }
    const launcher = (profilePath: string) => ({
      launcherName: 'Claude',
      profilePath,
      sshHost: 'jared-mac',
    });
    expect(
      inventoryRecord({
        id: 'desk',
        windows: {
          ...launcher('C:\\Users\\x\\AppData\\Roaming\\Claude'),
          sshHost: 'jared-windows',
        },
      })?.isDefault
    ).toBe(true);
    // a launcher that does not parse still counts: its folder is what is protected
    expect(inventoryRecord({ id: 'desk', mac: { profilePath: '/x/Claude' } })?.isDefault).toBe(
      true
    );
    expect(inventoryRecord({ id: 'party', mac: launcher('/x/Claude-party') })?.isDefault).toBe(
      false
    );
    expect(
      pendingRecord({
        id: 'odd',
        label: null,
        mac: launcher('/x/claude'),
        windows: { ...launcher('C:\\x\\Claude-odd'), sshHost: 'jared-windows' },
        createdAt: '2026-10-02T08:00:00Z',
      }).isDefault
    ).toBe(true);
    expect(
      pendingRecord({
        id: 'work2',
        label: null,
        mac: launcher('/x/Claude-work2'),
        windows: { ...launcher('C:\\x\\Claude-work2'), sshHost: 'jared-windows' },
        createdAt: '2026-10-02T08:00:00Z',
      }).isDefault
    ).toBe(false);
  });

  it('trashes both hosts, drops only that inventory entry and keeps 30 days', async () => {
    const { ccsDir, hosts, lifecycle, inventory } = setup();
    const party = await lifecycle.findProfile('party');
    const result = await lifecycle.remove(party as never);
    expect(result.trashId).toMatch(/^tr_[a-f0-9]{16}$/);
    expect(result.purgeAfter).toBe(
      new Date(START + CLAUDE_TRASH_DAYS * DAY).toISOString().replace('.000Z', 'Z')
    );
    expect(hosts.calls).toEqual(['trash:mac', 'trash:windows']);
    expect([...hosts.trashDirs.mac]).toEqual(['party-20261002T080000Z']);
    expect(hosts.data.mac.size + hosts.data.windows.size).toBe(0);
    expect(inventory().profiles.map((entry: { id: string }) => entry.id)).toEqual(['gmail']);
    expect(inventory().note).toBe('kept as is');
    expect((await listClaudeDesktopProfiles()).map((entry) => entry.id)).toEqual(['gmail']);
    const trashFile = path.join(ccsDir, 'accounts', 'trash.json');
    expect(fs.statSync(trashFile).mode & 0o777).toBe(0o600);
    const listed = await lifecycle.listTrash();
    expect(listed).toEqual([
      {
        trashId: result.trashId,
        provider: 'claude',
        label: 'party@example.com',
        trashedAt: '2026-10-02T08:00:00Z',
        purgeAfter: result.purgeAfter,
        state: 'trashed',
      },
    ]);
    // Host paths stay on the server.
    expect(JSON.stringify(listed)).not.toContain('/fake/');
    expect(JSON.stringify(listed)).not.toContain('Roaming');
  });

  it('puts the first host back when the second cannot move, and refuses another volume', async () => {
    const { hosts, lifecycle, inventory } = setup();
    const party = await lifecycle.findProfile('party');
    hosts.fail.trash = 'windows';
    await expect(lifecycle.remove(party as never)).rejects.toMatchObject({
      code: 'remove_failed',
      host: 'windows',
    });
    expect(hosts.calls).toEqual(['trash:mac', 'trash:windows', 'restore:mac']);
    expect(hosts.data.mac.has('/fake/mac/Claude-party')).toBe(true);
    expect(hosts.trashDirs.mac.size).toBe(0);
    hosts.fail = {};
    hosts.calls = [];
    hosts.crossVolume.add('windows');
    await expect(lifecycle.remove(party as never)).rejects.toMatchObject({
      code: 'trash_cross_volume',
    });
    expect(hosts.calls).toEqual(['trash:mac', 'trash:windows', 'restore:mac']);
    expect(inventory().profiles).toHaveLength(2);
    expect(await lifecycle.listTrash()).toEqual([]);
  });

  it('takes the trash record out again when the inventory entry cannot be dropped', async () => {
    const { ccsDir, hosts, lifecycle } = setup();
    const party = await lifecycle.findProfile('party');
    // The inventory becomes unreadable after the review, so dropping its entry fails.
    const file = path.join(ccsDir, 'claude-desktop-profiles.json');
    fs.writeFileSync(file, '{ not json');
    await expect(lifecycle.remove(party as never)).rejects.toMatchObject({
      code: 'remove_failed',
    });
    expect(hosts.calls).toEqual(['trash:mac', 'trash:windows', 'restore:windows', 'restore:mac']);
    expect(hosts.data.mac.has('/fake/mac/Claude-party')).toBe(true);
    expect(hosts.data.windows.has('C:\\Users\\x\\AppData\\Roaming\\Claude-party')).toBe(true);
    // Never missing from both: no trash record was left behind for the untouched entry.
    expect(await lifecycle.listTrash()).toEqual([]);
    expect(fs.readFileSync(file, 'utf8')).toBe('{ not json');
  });

  it('restores everything within 30 days, and refuses an id that was reused', async () => {
    const { hosts, lifecycle, inventory } = setup();
    const party = await lifecycle.findProfile('party');
    const { trashId } = await lifecycle.remove(party as never);
    const entry = await lifecycle.findTrash(trashId);
    expect(lifecycle.restoreFingerprint(entry as never)).toMatch(/^[a-f0-9]{64}$/);
    expect(await lifecycle.restore(trashId)).toEqual({ accountId: 'claude:party' });
    expect(hosts.data.mac.has('/fake/mac/Claude-party')).toBe(true);
    expect(
      hosts.launchers.windows.has('C:\\Users\\x\\Desktop\\Claude (party@example.com).lnk')
    ).toBe(true);
    expect(inventory().profiles.map((item: { id: string }) => item.id)).toEqual(['gmail', 'party']);
    expect(await lifecycle.listTrash()).toEqual([]);
    const again = await lifecycle.remove((await lifecycle.findProfile('party')) as never);
    const reused = setupReuse(inventory());
    fs.writeFileSync(reused.file, JSON.stringify(reused.document));
    await expect(lifecycle.restore(again.trashId)).rejects.toMatchObject({ code: 'id_in_use' });
    await expect(lifecycle.restore('tr_0000000000000000')).rejects.toBeInstanceOf(
      ClaudeLifecycleError
    );
  });

  it('purges only entries past 30 days and keeps an unreachable host as deleting', async () => {
    const { hosts, lifecycle, advance } = setup();
    const { trashId } = await lifecycle.remove((await lifecycle.findProfile('party')) as never);
    hosts.calls = [];
    advance(29 * DAY);
    expect(await lifecycle.purgeDue()).toBe(0);
    expect(hosts.calls).toEqual([]);
    advance(2 * DAY);
    hosts.fail.purge = 'windows';
    expect(await lifecycle.purgeDue()).toBe(0);
    expect((await lifecycle.listTrash())[0]).toMatchObject({ trashId, state: 'deleting' });
    hosts.fail = {};
    expect(await lifecycle.purgeDue()).toBe(1);
    expect(await lifecycle.listTrash()).toEqual([]);
    expect(hosts.trashDirs.mac.size + hosts.trashDirs.windows.size).toBe(0);
  });

  it('purges one entry now on both hosts, before its 30 days', async () => {
    const { hosts, lifecycle } = setup();
    const { trashId } = await lifecycle.remove((await lifecycle.findProfile('party')) as never);
    hosts.calls = [];
    expect(await lifecycle.purgeOne(trashId)).toEqual({ trashId });
    expect(hosts.calls).toEqual(['purge:mac', 'purge:windows']);
    expect(await lifecycle.listTrash()).toEqual([]);
    expect(hosts.trashDirs.mac.size + hosts.trashDirs.windows.size).toBe(0);
    await expect(lifecycle.purgeOne(trashId)).rejects.toMatchObject({ code: 'unknown_trash' });
  });

  it('purgeOne keeps a failed host as deleting and retries it', async () => {
    const { hosts, lifecycle } = setup();
    const { trashId } = await lifecycle.remove((await lifecycle.findProfile('party')) as never);
    hosts.calls = [];
    hosts.fail.purge = 'windows';
    await expect(lifecycle.purgeOne(trashId)).rejects.toMatchObject({
      code: 'host_unreachable',
      host: 'windows',
    });
    expect((await lifecycle.listTrash())[0]).toMatchObject({ trashId, state: 'deleting' });
    hosts.fail = {};
    hosts.calls = [];
    expect(await lifecycle.purgeOne(trashId)).toEqual({ trashId });
    expect(hosts.calls).toEqual(['purge:mac', 'purge:windows']);
    expect(await lifecycle.listTrash()).toEqual([]);
  });

  it('purgeOne refuses a default profile found in the trash and runs nothing', async () => {
    const { ccsDir, hosts, lifecycle } = setup();
    const party = await lifecycle.findProfile('party');
    const { trashId } = await lifecycle.remove(party as never);
    // A default marker smuggled into the stored entry (Remove never writes one).
    const file = path.join(ccsDir, 'accounts', 'trash.json');
    const document = JSON.parse(fs.readFileSync(file, 'utf8'));
    const entry = document.entries.find((item: { trashId: string }) => item.trashId === trashId);
    entry.entry.windows = { ...(entry.entry.windows as object), isDefault: true };
    fs.writeFileSync(file, JSON.stringify(document));
    hosts.calls = [];
    await expect(lifecycle.purgeOne(trashId)).rejects.toMatchObject({
      code: 'account_protected',
    });
    expect(hosts.calls).toEqual([]);
    expect(await lifecycle.listTrash()).toHaveLength(1);
  });

  it('purgeOne is off while host steps are off', async () => {
    const { hosts, lifecycle } = setup(false);
    await expect(lifecycle.purgeOne('tr_0000000000000000')).rejects.toMatchObject({
      code: 'not_implemented',
    });
    expect(hosts.calls).toEqual([]);
  });
});

function setupReuse(document: { profiles: Array<Record<string, unknown>> }) {
  const file = path.join(process.env.CCS_HOME as string, '.ccs', 'claude-desktop-profiles.json');
  return {
    file,
    document: {
      ...document,
      profiles: [
        ...document.profiles,
        { id: 'party', email: 'new@example.com', mac: { launcherName: 'x' } },
      ],
    },
  };
}
