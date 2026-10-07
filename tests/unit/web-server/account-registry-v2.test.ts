import { afterEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  ACCOUNT_REGISTRY_FILE,
  parseAccountRegistry,
  readAccountRegistry,
  registryFromSourceManifest,
  updateAccountRegistry,
  type AccountRegistryV2,
} from '../../../src/web-server/services/account-registry-v2';
import { ADDITIONAL_PROVIDERS } from '../../../src/web-server/services/additional-usage-transport';

const temporaryDirs: string[] = [];
afterEach(() => {
  for (const dir of temporaryDirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true });
});

function ccsDir(): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-registry-v2-'));
  temporaryDirs.push(dir);
  return dir;
}

/** The contract's own example (CONTRACT-registry-lifecycle 3.1). */
function example(): Record<string, unknown> {
  return {
    version: 2,
    accounts: [
      {
        id: 'zai:usage',
        provider: 'zai',
        platform: 'ubuntu',
        sshHost: null,
        label: null,
        credential: { kind: 'discover' },
        createdAt: '2026-10-02T07:00:00Z',
        createdBy: 'migration',
      },
      {
        id: 'zai:acct:9f2c41d0',
        provider: 'zai',
        platform: 'ubuntu',
        sshHost: null,
        label: 'Work',
        credential: { kind: 'aac-key', keyId: '9f2c41d0' },
        createdAt: '2026-10-02T07:05:00Z',
        createdBy: 'dashboard',
      },
      {
        id: 'qwen:usage',
        provider: 'qwen',
        platform: 'windows',
        sshHost: 'windows-host',
        label: null,
        credential: { kind: 'browser-capsule', capsuleId: 'default' },
        createdAt: '2026-10-02T07:00:00Z',
        createdBy: 'migration',
      },
    ],
  };
}

function withAccount(change: Record<string, unknown>, index = 1): Record<string, unknown> {
  const value = example();
  const accounts = value.accounts as Record<string, unknown>[];
  accounts[index] = { ...accounts[index], ...change };
  return value;
}

function keyAccount(provider: string, hex: string): Record<string, unknown> {
  return {
    id: `${provider}:acct:${hex}`,
    provider,
    platform: 'ubuntu',
    credential: { kind: 'aac-key', keyId: hex },
  };
}

describe('account registry v2 schema', () => {
  it('accepts the contract example and normalizes optional fields', () => {
    const registry = parseAccountRegistry(example());
    expect(registry?.accounts.map((entry) => entry.id)).toEqual([
      'zai:usage',
      'zai:acct:9f2c41d0',
      'qwen:usage',
    ]);
    expect(registry?.accounts[2].sshHost).toBe('windows-host');
    const minimal = parseAccountRegistry({ version: 2, accounts: [keyAccount('zai', '0a1b2c3d')] });
    expect(minimal?.accounts[0]).toEqual({
      id: 'zai:acct:0a1b2c3d',
      provider: 'zai',
      platform: 'ubuntu',
      sshHost: null,
      label: null,
      credential: { kind: 'aac-key', keyId: '0a1b2c3d' },
      createdAt: null,
      createdBy: null,
    });
    expect(parseAccountRegistry({ version: 2, accounts: [] })).toEqual({
      version: 2,
      accounts: [],
    });
  });

  it.each([
    ['version 1', { ...example(), version: 1 }],
    ['an unknown top-level key', { ...example(), command: 'PRIVATE' }],
    ['an unknown entry key', withAccount({ helper: '/tmp/PRIVATE' })],
    ['a core provider', withAccount({ provider: 'claude', id: 'claude:acct:9f2c41d0' })],
    ['an id of another provider', withAccount({ id: 'kimi-code:acct:9f2c41d0' })],
    ['an id that is not hex', withAccount({ id: 'zai:acct:9F2C41D0' })],
    ['discover outside the migrated id', withAccount({ credential: { kind: 'discover' } })],
    ['a duplicate id', withAccount({ id: 'zai:usage', credential: { kind: 'discover' } })],
    ['an api key on Qwen', withAccount({ provider: 'qwen', id: 'qwen:acct:9f2c41d0' })],
    [
      'a capsule on Z.ai',
      withAccount({ credential: { kind: 'browser-capsule', capsuleId: 'default' } }),
    ],
    ['a bad key id', withAccount({ credential: { kind: 'aac-key', keyId: '../9f2c41' } })],
    [
      'an extra credential key',
      withAccount({ credential: { kind: 'aac-key', keyId: '9f2c41d0', path: '/x' } }),
    ],
    [
      'an unknown credential kind',
      withAccount({ credential: { kind: 'env', name: 'ZAI_API_KEY' } }),
    ],
    ['platform linux', withAccount({ platform: 'linux' })],
    ['an unsafe ssh alias', withAccount({ sshHost: '-oProxyCommand=PRIVATE' })],
    ['a 49-character label', withAccount({ label: 'x'.repeat(49) })],
    ['an empty label', withAccount({ label: '' })],
    ['a blank label', withAccount({ label: '   ' })],
    ['a control character label', withAccount({ label: 'Work\u0007' })],
    ['a bidi override label', withAccount({ label: 'Work\u202e' })],
    ['a zero-width space label', withAccount({ label: '\u200b' })],
    ['a zero-width space in a label', withAccount({ label: 'Wo\u200brk' })],
    ['a left-to-right mark in a label', withAccount({ label: 'Work\u200e' })],
    ['an Arabic letter mark in a label', withAccount({ label: 'Work\u061c' })],
    ['a word joiner in a label', withAccount({ label: 'Work\u2060' })],
    ['an invisible plus in a label', withAccount({ label: 'Work\u2064' })],
    ['a byte order mark label', withAccount({ label: '\ufeff' })],
    ['a lone surrogate in a label', withAccount({ label: 'Work\ud800' })],
    ['a label of spaces and format characters', withAccount({ label: ' \u200b\u2060 ' })],
    ['discover created by the dashboard', withAccount({ createdBy: 'dashboard' }, 0)],
    ['a bad timestamp', withAccount({ createdAt: 'yesterday' })],
    ['an unknown creator', withAccount({ createdBy: 'cli' })],
  ])('rejects the whole file for %s', (_name, value) => {
    expect(parseAccountRegistry(value)).toBeNull();
  });

  it('accepts ordinary labels in any script, and discover from migration or no creator', () => {
    for (const label of ['Work 2', 'Café', '仕事', 'Работа', 'x'.repeat(48)]) {
      expect({ label, ok: parseAccountRegistry(withAccount({ label })) !== null }).toEqual({
        label,
        ok: true,
      });
    }
    expect(parseAccountRegistry(withAccount({ createdBy: null }, 0))).not.toBeNull();
    expect(parseAccountRegistry(withAccount({ createdBy: undefined }, 0))).not.toBeNull();
  });

  it("treats Qwen's discover row as holding the default console capsule", () => {
    const qwen = (id: string, credential: Record<string, unknown>) => ({
      id,
      provider: 'qwen',
      platform: 'windows',
      sshHost: 'windows-host',
      credential,
    });
    const discover = qwen('qwen:usage', { kind: 'discover' });
    // Both would read qwen-console-session.json, so the account would show twice.
    expect(
      parseAccountRegistry({
        version: 2,
        accounts: [
          discover,
          qwen('qwen:acct:0a1b2c3d', { kind: 'browser-capsule', capsuleId: 'default' }),
        ],
      })
    ).toBeNull();
    expect(
      parseAccountRegistry({
        version: 2,
        accounts: [
          qwen('qwen:acct:0a1b2c3d', { kind: 'browser-capsule', capsuleId: 'default' }),
          discover,
        ],
      })
    ).toBeNull();
    expect(
      parseAccountRegistry({
        version: 2,
        accounts: [
          discover,
          qwen('qwen:acct:0a1b2c3d', { kind: 'browser-capsule', capsuleId: '0a1b2c3d' }),
        ],
      })?.accounts
    ).toHaveLength(2);
    // Other providers' discover rows hold no store reference.
    expect(
      parseAccountRegistry({
        version: 2,
        accounts: [
          {
            id: 'zai:usage',
            provider: 'zai',
            platform: 'ubuntu',
            credential: { kind: 'discover' },
          },
          keyAccount('zai', '0a1b2c3d'),
        ],
      })?.accounts
    ).toHaveLength(2);
  });

  it('rejects a second account on the same key, and more than 16 per provider or 64 in all', () => {
    const sameKey = example();
    (sameKey.accounts as unknown[]).push({
      ...keyAccount('zai', '0a1b2c3d'),
      credential: { kind: 'aac-key', keyId: '9f2c41d0' },
    });
    expect(parseAccountRegistry(sameKey)).toBeNull();
    // The same key id under another provider is a different file.
    expect(
      parseAccountRegistry({
        version: 2,
        accounts: [keyAccount('zai', '9f2c41d0'), keyAccount('kimi-code', '9f2c41d0')],
      })
    ).not.toBeNull();
    const hex = (index: number) => index.toString(16).padStart(8, '0');
    const sixteen = Array.from({ length: 16 }, (_, index) => keyAccount('zai', hex(index)));
    expect(parseAccountRegistry({ version: 2, accounts: sixteen })?.accounts).toHaveLength(16);
    expect(
      parseAccountRegistry({ version: 2, accounts: [...sixteen, keyAccount('zai', hex(16))] })
    ).toBeNull();
    const sixtyFive = ['zai', 'kimi-code', 'opencode-go', 'zai', 'kimi-code']
      .flatMap((provider, block) =>
        Array.from({ length: 13 }, (_, index) => keyAccount(provider, hex(block * 100 + index)))
      )
      .slice(0, 65);
    expect(parseAccountRegistry({ version: 2, accounts: sixtyFive })).toBeNull();
  });
});

describe('account registry v2 file', () => {
  it('is absent, invalid (never empty) or valid', async () => {
    const dir = ccsDir();
    const file = path.join(dir, ACCOUNT_REGISTRY_FILE);
    expect(await readAccountRegistry(dir)).toEqual({ state: 'absent' });
    fs.writeFileSync(file, JSON.stringify(example()), { mode: 0o600 });
    const read = await readAccountRegistry(dir);
    expect(read.state).toBe('ok');
    fs.chmodSync(file, 0o644);
    expect(await readAccountRegistry(dir)).toEqual({ state: 'invalid' });
    fs.chmodSync(file, 0o600);
    fs.writeFileSync(file, '{"version":2,', { mode: 0o600 });
    expect(await readAccountRegistry(dir)).toEqual({ state: 'invalid' });
    fs.writeFileSync(
      file,
      JSON.stringify({ version: 2, accounts: [], pad: 'x'.repeat(64 * 1024) })
    );
    expect(await readAccountRegistry(dir)).toEqual({ state: 'invalid' });
    fs.rmSync(file);
    const target = path.join(dir, 'elsewhere.json');
    fs.writeFileSync(target, JSON.stringify(example()), { mode: 0o600 });
    fs.symlinkSync(target, file);
    expect(await readAccountRegistry(dir)).toEqual({ state: 'invalid' });
  });

  it('migrates the effective version 1 sources on the first write and never touches version 1', async () => {
    const dir = ccsDir();
    const v1 = path.join(dir, 'account-usage-sources.json');
    const v1Contents = `${JSON.stringify({
      version: 1,
      sources: [{ provider: 'cursor', platform: 'mac', sshHost: 'mac-usage' }],
    })}\n`;
    fs.writeFileSync(v1, v1Contents, { mode: 0o644 });
    const v1Stat = fs.statSync(v1);
    const saved = await updateAccountRegistry(
      dir,
      (registry) => ({
        version: 2,
        accounts: [
          ...registry.accounts,
          {
            ...registry.accounts[5],
            id: 'zai:acct:9f2c41d0',
            label: 'Work',
            credential: { kind: 'aac-key', keyId: '9f2c41d0' },
            createdBy: 'dashboard',
          },
        ],
      }),
      { now: () => Date.parse('2026-10-02T07:00:00Z') }
    );
    expect(saved.accounts.map((entry) => entry.id)).toEqual([
      ...ADDITIONAL_PROVIDERS.map((provider) => `${provider}:usage`),
      'zai:acct:9f2c41d0',
    ]);
    expect(saved.accounts[2]).toEqual({
      id: 'cursor:usage',
      provider: 'cursor',
      platform: 'mac',
      sshHost: 'mac-usage',
      label: null,
      credential: { kind: 'discover' },
      createdAt: '2026-10-02T07:00:00.000Z',
      createdBy: 'migration',
    });
    const file = path.join(dir, ACCOUNT_REGISTRY_FILE);
    expect(fs.statSync(file).mode & 0o777).toBe(0o600);
    expect(fs.readFileSync(v1, 'utf8')).toBe(v1Contents);
    expect(fs.statSync(v1).mtimeMs).toBe(v1Stat.mtimeMs);
    expect(fs.statSync(v1).mode & 0o777).toBe(0o644);
    expect(fs.readdirSync(dir).sort()).toEqual([
      ACCOUNT_REGISTRY_FILE,
      'account-usage-sources.json',
    ]);
    // A second write builds on the saved registry and does not migrate again.
    const second = await updateAccountRegistry(dir, (registry) => ({
      version: 2,
      accounts: registry.accounts.filter((entry) => entry.provider !== 'kimi-code'),
    }));
    expect(second.accounts.some((entry) => entry.provider === 'kimi-code')).toBe(false);
    expect((await readAccountRegistry(dir)).state).toBe('ok');
    expect(fs.readFileSync(v1, 'utf8')).toBe(v1Contents);
  });

  it('creates a missing CCS directory at 0700 and serializes concurrent writes', async () => {
    const dir = path.join(ccsDir(), 'nested', '.ccs');
    const add =
      (hex: string) =>
      (registry: AccountRegistryV2): AccountRegistryV2 => ({
        version: 2,
        accounts: [
          ...registry.accounts,
          {
            ...registry.accounts[5],
            id: `zai:acct:${hex}`,
            credential: { kind: 'aac-key', keyId: hex },
          },
        ],
      });
    await Promise.all([
      updateAccountRegistry(dir, add('00000001')),
      updateAccountRegistry(dir, add('00000002')),
      updateAccountRegistry(dir, add('00000003')),
    ]);
    expect(fs.statSync(dir).mode & 0o777).toBe(0o700);
    const read = await readAccountRegistry(dir);
    expect(
      read.state === 'ok' &&
        read.registry.accounts.filter((entry) => entry.provider === 'zai').map((entry) => entry.id)
    ).toEqual(['zai:usage', 'zai:acct:00000001', 'zai:acct:00000002', 'zai:acct:00000003']);
    expect(fs.readdirSync(dir)).toEqual([ACCOUNT_REGISTRY_FILE]);
  });

  it('refuses a lifecycle write that would add a dashboard discover row', async () => {
    const dir = ccsDir();
    await expect(
      updateAccountRegistry(dir, (registry) => ({
        version: 2,
        accounts: registry.accounts.map((entry) =>
          entry.id === 'zai:usage' ? { ...entry, createdBy: 'dashboard' } : entry
        ),
      }))
    ).rejects.toThrow('The account list change is not valid.');
    expect(fs.existsSync(path.join(dir, ACCOUNT_REGISTRY_FILE))).toBe(false);
  });

  it('refuses to replace an invalid registry or migrate an invalid version 1 manifest', async () => {
    const dir = ccsDir();
    const file = path.join(dir, ACCOUNT_REGISTRY_FILE);
    fs.writeFileSync(file, '{"version":2,"accounts":[{"id":"PRIVATE"}]}', { mode: 0o600 });
    await expect(updateAccountRegistry(dir, (registry) => registry)).rejects.toThrow(
      'Account list could not be read safely.'
    );
    expect(fs.readFileSync(file, 'utf8')).toContain('PRIVATE');
    fs.rmSync(file);
    fs.writeFileSync(
      path.join(dir, 'account-usage-sources.json'),
      '{"version":1,"sources":[{"provider":"claude"}]}'
    );
    await expect(updateAccountRegistry(dir, (registry) => registry)).rejects.toThrow(
      'Account list could not be read safely.'
    );
    expect(fs.existsSync(file)).toBe(false);
    fs.rmSync(path.join(dir, 'account-usage-sources.json'));
    await expect(
      updateAccountRegistry(dir, (registry) => ({
        ...registry,
        accounts: [...registry.accounts, ...registry.accounts],
      }))
    ).rejects.toThrow('The account list change is not valid.');
    expect(fs.existsSync(file)).toBe(false);
  });

  it('builds the migration from defaults when no version 1 manifest exists', () => {
    const registry = registryFromSourceManifest([], '2026-10-02T07:00:00.000Z');
    expect(registry.accounts.map((entry) => [entry.id, entry.platform, entry.sshHost])).toEqual(
      ADDITIONAL_PROVIDERS.map((provider) => [`${provider}:usage`, 'ubuntu', null])
    );
    expect(parseAccountRegistry(registry)).toEqual(registry);
  });
});
