/**
 * L2 (DESIGN-M3-COMPLETE 2.2): config/private-files.json is the one private-file registry. TS
 * reads it here (src/shared/private-files.ts) and Rust embeds it (crates/aac-files registry.rs,
 * whose `cargo test` applies the same rule edits). This file checks:
 * - both parsers' rules: the same broken rows are refused;
 * - drift: each row's file name and cap equal the constants of the TS module that writes it;
 * - one writer: once a row is the Rust service's, every TS writer of it refuses and writes
 *   nothing, and the boot JSON carries the owners the service checks.
 * Runs on a fixture CCS_HOME; nothing reads the real home.
 */
import { afterAll, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';

const home = fs.mkdtempSync(path.join(process.env.TMPDIR ?? os.tmpdir(), 'aac-l2-private-'));
process.env.CCS_HOME = home;
afterAll(() => fs.rmSync(home, { recursive: true, force: true }));

const root = path.resolve(import.meta.dir, '../..');
const registry = JSON.parse(fs.readFileSync(path.join(root, 'config/private-files.json'), 'utf8'));
const files = await import('../../src/shared/private-files');
const { getCcsDir } = await import('../../src/utils/config-manager');

function edited(edit: (value: any) => void): unknown {
  const value = structuredClone(registry);
  edit(value);
  return value;
}

describe('the registry rules', () => {
  it('parses the shipped registry and lists every reader the service has', () => {
    const ids = files.PRIVATE_FILE_ROWS.map((row) => row.id);
    for (const id of [
      'account-keys',
      'account-visibility',
      'dashboard-preferences',
      'dashboard-preferences-extra-sources',
      'claude-desktop-profiles',
      'account-refresh-settings',
      'readers-check',
    ])
      expect(ids).toContain(id);
    expect(files.PRIVATE_FILE_ROWS.every((r) => r.owner === 'ts')).toBe(true);
    const moved = files.resolvePrivateFileOwners(
      files.PRIVATE_FILE_ROWS,
      (g) => g === 'collection'
    );
    expect(Object.keys(moved).filter((id) => moved[id] === 'rust-service')).toEqual([
      'account-usage-observations',
      'claude-live-snapshots',
    ]);
    expect(
      files.privateFileRowFor(path.join(home, 'account-usage-failover', 'cooldown-key.json'), home)
        ?.id
    ).toBe('account-usage-cooldown');
    expect(
      files.privateFileRowFor(
        path.join(home, 'account-usage-failover', 'key.muse-peers.json'),
        home
      )?.id
    ).toBe('muse-usage-peer-links');
  });

  it('refuses the rows aac_files::registry::parse refuses', () => {
    const edits: Array<(v: any) => void> = [
      (v) => (v.rows[0].path = '../escape'),
      (v) => (v.rows[0].path = '/absolute'),
      (v) => (v.rows[0].path = 'a\\b'),
      (v) => (v.rows[0].path = 'x.lock'),
      (v) => (v.rows[0].path = '*/x.json'),
      (v) => (v.rows[0].path = 'a/*.*'),
      (v) => (v.rows[0].mode = '0644'),
      (v) => (v.rows[0].mode = '600'),
      (v) => (v.rows[0].maxBytes = 0),
      (v) => (v.rows[0].owner = 'rust-service'),
      (v) => (v.rows[0].owner = 'python'),
      (v) => (v.rows[0].id = 'Upper'),
      (v) => (v.rows[1].id = v.rows[0].id),
      (v) => (v.rows[1].path = v.rows[0].path),
      (v) => (v.rows[0].extra = true),
      (v) => delete v.rows[0].movesWith,
      (v) => (v.version = 2),
    ];
    expect(() => files.parsePrivateFiles(registry)).not.toThrow();
    edits.forEach((edit, i) => {
      expect(() => files.parsePrivateFiles(edited(edit)), `edit ${i}`).toThrow();
    });
  });

  it('names only route groups in movesWith', () => {
    const groups = new Set(
      JSON.parse(
        fs.readFileSync(path.join(root, 'src/web-server/rust-service-routes.json'), 'utf8')
      ).map((r: { group: string }) => r.group)
    );
    for (const row of files.PRIVATE_FILE_ROWS)
      if (row.movesWith !== null) expect(groups.has(row.movesWith)).toBe(true);
  });
});

describe('drift against the TS writers', () => {
  it('maps each writer path to its row, with its cap', async () => {
    const ccs = getCcsDir();
    const visibility = await import('../../src/web-server/services/account-visibility');
    const registryV2 = await import('../../src/web-server/services/account-registry-v2');
    const auth = await import('../../src/web-server/services/dashboard-auth-files');
    const sessions = await import('../../src/web-server/middleware/dashboard-session-store');
    const snapshot = await import('../../src/web-server/agent-usage/agent-usage-settings');
    const hub = await import('../../src/web-server/usage-hub/usage-hub-key-store');
    const claude = await import('../../src/web-server/services/claude-account-stores');
    const cases: Array<[string, string, number | null]> = [
      [
        visibility.accountVisibilityPath(ccs),
        'account-visibility',
        visibility.MAX_VISIBILITY_BYTES,
      ],
      [registryV2.accountRegistryPath(ccs), 'account-registry', registryV2.MAX_REGISTRY_BYTES],
      [auth.authFile('state.json'), 'auth-state', auth.MAX_AUTH_FILE_BYTES],
      [auth.authFile('devices.json'), 'auth-devices', auth.MAX_AUTH_FILE_BYTES],
      [auth.authFile('setup-code'), 'auth-setup-code', auth.MAX_AUTH_FILE_BYTES],
      [sessions.sessionsFile(), 'auth-sessions', null],
      [snapshot.snapshotSettingsPath(ccs), 'agent-usage-snapshot', null],
      [path.join(auth.authDirectory(), hub.USAGE_HUB_KEY_FILE), 'usage-hub-key', null],
      [claude.pendingFile(ccs), 'claude-pending', null],
      [claude.trashFile(ccs), 'claude-trash', null],
      [claude.inventoryFile(ccs), 'claude-desktop-profiles', null],
      [path.join(ccs, 'dashboard-preferences.json'), 'dashboard-preferences', null],
      [path.join(ccs, 'account-refresh-settings.json'), 'account-refresh-settings', null],
      [path.join(ccs, 'account-usage', 'keys', 'zai-12345678.json'), 'account-keys', null],
    ];
    for (const [file, id, cap] of cases) {
      const row = files.privateFileRowFor(file, ccs);
      expect(row?.id, file).toBe(id);
      if (cap !== null) expect(row?.maxBytes, id).toBe(cap);
    }
    // Caps the TS modules keep private: the source states the same number.
    const source = (relative: string) => fs.readFileSync(path.join(root, relative), 'utf8');
    const statedCaps: Array<[string, RegExp, string]> = [
      [
        'src/web-server/middleware/dashboard-session-store.ts',
        /MAX_FILE_BYTES = (.+);/,
        'auth-sessions',
      ],
      [
        'src/web-server/agent-usage/agent-usage-settings.ts',
        /MAX_SETTINGS_BYTES = (.+);/,
        'agent-usage-snapshot',
      ],
      [
        'src/web-server/usage-hub/usage-hub-key-store.ts',
        /MAX_FILE_BYTES = (.+);/,
        'usage-hub-key',
      ],
      [
        'src/web-server/services/claude-account-stores.ts',
        /MAX_STORE_BYTES = (.+);/,
        'claude-pending',
      ],
      [
        'src/web-server/services/claude-account-stores.ts',
        /MAX_STORE_BYTES = (.+);/,
        'claude-trash',
      ],
      [
        'src/web-server/services/claude-account-stores.ts',
        /MAX_INVENTORY_BYTES = (.+);/,
        'claude-desktop-profiles',
      ],
      [
        'src/web-server/services/dashboard-preferences.ts',
        /MAX_PREFERENCES_BYTES = (.+);/,
        'dashboard-preferences',
      ],
      [
        'src/web-server/services/dashboard-preferences.ts',
        /MAX_PREFERENCES_BYTES = (.+);/,
        'dashboard-preferences-extra-sources',
      ],
      [
        'src/web-server/services/account-refresh-settings.ts',
        /MAX_SETTINGS_BYTES = (.+);/,
        'account-refresh-settings',
      ],
    ];
    for (const [file, pattern, id] of statedCaps) {
      const stated = pattern.exec(source(file))?.[1];
      expect(stated, file).toBeDefined();
      const value = Function(`"use strict"; return (${stated});`)() as number;
      expect(files.PRIVATE_FILE_ROWS.find((r) => r.id === id)?.maxBytes, id).toBe(value);
    }
    // Files the registry does not list, or outside the CCS folder, are not its rows.
    expect(files.privateFileRowFor(path.join(ccs, 'unlisted.json'), ccs)).toBeUndefined();
    expect(
      files.privateFileRowFor(path.join(ccs, 'account-usage', 'keys', '.x.json'), ccs)
    ).toBeUndefined();
    expect(files.privateFileRowFor('/elsewhere/account-visibility.json', ccs)).toBeUndefined();
  });
});

describe('one writer per file', () => {
  const moved = (ids: string[]) =>
    files.parsePrivateFiles(
      edited((v) => {
        for (const row of v.rows) if (ids.includes(row.id)) row.movesWith = 'accounts';
      })
    );

  it('resolves owners from the route groups', () => {
    const rows = moved(['account-visibility']);
    expect(files.resolvePrivateFileOwners(rows, () => false)['account-visibility']).toBe('ts');
    expect(
      files.resolvePrivateFileOwners(rows, (g) => g === 'accounts')['account-visibility']
    ).toBe('rust-service');
    // A row that moves with no group never leaves TS.
    expect(files.resolvePrivateFileOwners(rows, () => true)['auth-state']).toBe('ts');
    const shippedWithCollectionOff = files.resolvePrivateFileOwners(
      files.PRIVATE_FILE_ROWS,
      (group) => group !== 'collection'
    );
    for (const owner of Object.values(shippedWithCollectionOff)) expect(owner).toBe('ts');
  });

  it('refuses every TS writer of a row the service owns, and writes nothing', async () => {
    const ccs = getCcsDir();
    fs.mkdirSync(ccs, { recursive: true, mode: 0o700 });
    const visibility = await import('../../src/web-server/services/account-visibility');
    const refresh = await import('../../src/web-server/services/account-refresh-settings');
    const keys = await import('../../src/web-server/services/account-key-store');
    const readers = await import('../../src/web-server/status/readers-check');
    const empty = {
      hiddenProviders: [],
      hiddenAccountIds: [],
      trayHiddenProviders: [],
      trayHiddenAccountIds: [],
    };
    const shareRoot = fs.mkdtempSync(path.join(home, 'share-'));
    const writer = new readers.ReadersCheckWriter({ root: () => shareRoot });
    try {
      files.settlePrivateFileOwners(
        () => true,
        moved(['account-visibility', 'account-refresh-settings', 'account-keys', 'readers-check'])
      );
      await expect(visibility.writeAccountVisibility(ccs, empty)).rejects.toBeInstanceOf(
        files.PrivateFileOwnerError
      );
      expect(fs.existsSync(visibility.accountVisibilityPath(ccs))).toBe(false);
      expect(() =>
        refresh.writeAccountRefreshSettings({ refreshIntervalSeconds: 300 }, ccs)
      ).toThrow(files.PrivateFileOwnerError);
      expect(fs.existsSync(path.join(ccs, 'account-refresh-settings.json'))).toBe(false);
      const store = new keys.LocalKeyStore(ccs);
      await expect(store.put('zai', '12345678', 'fixture-secret-1')).rejects.toBeInstanceOf(
        files.PrivateFileOwnerError
      );
      expect(fs.existsSync(path.join(ccs, 'account-usage'))).toBe(false);
      await writer.flush();
      expect(fs.readdirSync(shareRoot)).toEqual([]);
      // A row that stays with TS is still written in the same process.
      const auth = await import('../../src/web-server/services/dashboard-auth-files');
      await auth.writeAuthJson(auth.authFile('devices.json'), { version: 1, devices: [] });
      expect(fs.existsSync(auth.authFile('devices.json'))).toBe(true);
    } finally {
      files.settlePrivateFileOwners(() => false);
    }
    // Back with TS: the same writers write.
    await visibility.writeAccountVisibility(ccs, empty);
    expect(fs.existsSync(visibility.accountVisibilityPath(ccs))).toBe(true);
    refresh.writeAccountRefreshSettings({ refreshIntervalSeconds: 300 }, ccs);
    expect(fs.existsSync(path.join(ccs, 'account-refresh-settings.json'))).toBe(true);
  });

  it('sends the settled owners in the boot JSON and attaches only to the same owners', () => {
    const lifecycle = fs.readFileSync(
      path.join(root, 'src/web-server/rust-service-lifecycle.ts'),
      'utf8'
    );
    // The spawn writes `files` into the boot line and the attach check compares it.
    expect(lifecycle).toMatch(/bootId,\s*files: boot\.files,/);
    expect(lifecycle).toMatch(/ownersKey\(boot\.files\) !== ownersKey\(privateFileOwners\(\)\)/);
    const supervisor = fs.readFileSync(
      path.join(root, 'src/web-server/rust-service-supervisor.ts'),
      'utf8'
    );
    expect(supervisor).toContain("this.mode === 'on' && rustGroupOn('collection')");
    expect(supervisor).toContain("group === 'collection' ? collectionOn : this.mode === 'on'");
    const owners = files.privateFileOwners();
    expect(Object.keys(owners).sort()).toEqual(files.PRIVATE_FILE_ROWS.map((r) => r.id).sort());
  });
});
