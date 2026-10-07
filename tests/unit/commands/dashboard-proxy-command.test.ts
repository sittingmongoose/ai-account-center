/**
 * `ai-account-center dashboard proxy status|set|off`: the LAN HTTPS proxy set
 * through the product, with a 0600 backup of the previous config.yaml and
 * every other setting kept. Temporary CCS_HOME, placeholder values only.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { handleProxyCommand } from '../../../src/commands/dashboard-proxy-command';
import { invalidateConfigCache } from '../../../src/config/config-loader-facade';
import { createEmptyUnifiedConfig } from '../../../src/config/unified-config-types';
import {
  loadOrCreateUnifiedConfig,
  saveUnifiedConfig,
} from '../../../src/config/unified-config-loader';
import { getDashboardTlsSettings } from '../../../src/web-server/services/dashboard-tls-config';

const SELF = ['127.0.0.1', '::1', '192.168.1.10'];
const PROXY = '192.168.1.20';
const ORIGIN = 'https://aac.example.test';
const HASH = '$2b$04$abcdefghijklmnopqrstuuJ8bS1kq5v2yHq9w1Ue2o0m6Yt1yF6xK';
const NOW = new Date('2026-10-07T08:09:10.123Z');

let tempHome = '';
let previous: Record<'CCS_HOME' | 'CCS_DIR', string | undefined> = {
  CCS_HOME: undefined,
  CCS_DIR: undefined,
};

beforeEach(() => {
  tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-proxy-cli-'));
  previous = { CCS_HOME: process.env.CCS_HOME, CCS_DIR: process.env.CCS_DIR };
  delete process.env.CCS_DIR;
  process.env.CCS_HOME = tempHome;
  fs.mkdirSync(path.join(tempHome, '.ccs'), { recursive: true, mode: 0o700 });
  invalidateConfigCache();
});

afterEach(() => {
  for (const [name, value] of Object.entries(previous)) {
    if (value === undefined) delete process.env[name];
    else process.env[name] = value;
  }
  invalidateConfigCache();
  fs.rmSync(tempHome, { recursive: true, force: true });
});

function configFile(): string {
  return path.join(tempHome, '.ccs', 'config.yaml');
}

function backups(): string[] {
  return fs
    .readdirSync(path.join(tempHome, '.ccs'))
    .filter((name) => name.startsWith('config.yaml.bak-proxy-'))
    .sort();
}

/** A config with sign-in, the trusted local network and a logging choice, to prove they survive. */
function seedConfig(tls?: Record<string, unknown>): void {
  const config = createEmptyUnifiedConfig();
  config.dashboard_auth = {
    enabled: true,
    username: 'fixture-admin',
    password_hash: HASH,
    session_timeout_hours: 24,
  };
  config.logging = { ...config.logging, enabled: true, level: 'warn' };
  (config as unknown as Record<string, unknown>).dashboard_network = {
    trust_local_network: true,
    trusted_networks: ['192.168.1.0/24'],
  };
  if (tls) (config as unknown as Record<string, unknown>).dashboard_tls = tls;
  saveUnifiedConfig(config);
  invalidateConfigCache();
}

async function run(...args: string[]) {
  let out = '';
  let err = '';
  const code = await handleProxyCommand(
    args,
    {
      out: (text) => {
        out += text;
      },
      err: (text) => {
        err += text;
      },
    },
    { selfAddresses: SELF, now: () => NOW }
  );
  return { code, out, err, all: out + err };
}

function withoutTls(): Record<string, unknown> {
  const { dashboard_tls: _tls, ...rest } = loadOrCreateUnifiedConfig() as unknown as Record<
    string,
    unknown
  >;
  return rest;
}

describe('ai-account-center dashboard proxy', () => {
  it('reports off on a fresh config', async () => {
    seedConfig();
    const result = await run('status');
    expect(result.code).toBe(0);
    expect(result.out).toContain('LAN HTTPS proxy: off');
    expect(result.out).toContain('dashboard proxy set --address <ip> --origin <https url>');
  });

  it('sets the proxy, backs up the previous file (0600) and keeps every other setting', async () => {
    seedConfig();
    const before = fs.readFileSync(configFile());
    const othersBefore = withoutTls();
    const result = await run('set', '--address', PROXY, '--origin', `${ORIGIN}/`);
    expect([result.code, result.err]).toEqual([0, '']);
    expect(result.out).toContain(`address ${PROXY}, public origin ${ORIGIN}`);
    expect(result.out).toContain('no restart is needed');
    expect(result.out).toContain('config.yaml.bak-proxy-20261007T080910Z');
    expect(result.all).not.toContain(HASH);

    expect(backups()).toEqual(['config.yaml.bak-proxy-20261007T080910Z']);
    const backup = path.join(tempHome, '.ccs', backups()[0]);
    expect(fs.readFileSync(backup).equals(before)).toBe(true);
    expect(fs.statSync(backup).mode & 0o777).toBe(0o600);
    expect(fs.statSync(configFile()).mode & 0o777).toBe(0o600);

    invalidateConfigCache();
    expect(loadOrCreateUnifiedConfig().dashboard_tls).toEqual({
      trusted_proxy: 'lan-https-proxy',
      trusted_proxy_addresses: [PROXY],
      public_origin: ORIGIN,
    });
    expect(withoutTls()).toEqual(othersBefore);
    // The dashboard's own reader (what the running server uses) sees it at once.
    expect(getDashboardTlsSettings()).toMatchObject({
      trustedProxy: 'lan-https-proxy',
      trustedProxyAddresses: [PROXY],
      publicOrigin: ORIGIN,
    });

    const status = await run('status');
    expect(status.out).toContain('LAN HTTPS proxy: on');
    expect(status.out).toContain(`proxy address: ${PROXY}`);
    expect(status.out).toContain(`public origin: ${ORIGIN}`);
  });

  it('accepts several addresses and the --flag=value form, and changes nothing when repeated', async () => {
    seedConfig();
    const set = await run(
      'set',
      `--address=${PROXY}`,
      '--address',
      '::ffff:10.0.0.20',
      `--origin=${ORIGIN}`
    );
    expect(set.code).toBe(0);
    invalidateConfigCache();
    expect(loadOrCreateUnifiedConfig().dashboard_tls?.trusted_proxy_addresses).toEqual([
      PROXY,
      '10.0.0.20',
    ]);
    const again = await run(
      'set',
      '--address',
      PROXY,
      '--address',
      '10.0.0.20',
      '--origin',
      ORIGIN
    );
    expect(again.code).toBe(0);
    expect(again.out).toContain('nothing changed');
    expect(backups()).toHaveLength(1);
  });

  it('refuses addresses and origins it may not hold, and writes nothing', async () => {
    seedConfig();
    const before = fs.readFileSync(configFile());
    for (const [args, words] of [
      [['--address', '127.0.0.1', '--origin', ORIGIN], 'loopback'],
      [['--address', '0.0.0.0', '--origin', ORIGIN], 'unspecified'],
      [['--address', '::', '--origin', ORIGIN], 'unspecified'],
      [['--address', '192.168.1.10', '--origin', ORIGIN], "this computer's own"],
      [['--address', '192.168.1.0/24', '--origin', ORIGIN], 'not one exact IP address'],
      [['--address', 'proxy.example.test', '--origin', ORIGIN], 'not one exact IP address'],
      [['--address', '203.0.113.9', '--origin', ORIGIN], 'not a private LAN address'],
      [['--address', PROXY, '--origin', 'http://aac.example.test'], 'https://'],
      [['--address', PROXY, '--origin', `${ORIGIN}/path`], 'https://'],
      [['--address', PROXY, '--origin', 'https://user:pw@aac.example.test'], 'https://'],
      [['--address', PROXY], '--origin is required'],
      [['--origin', ORIGIN], '--address is required'],
      [['--address'], '--address needs a value'],
      [['--address', PROXY, '--origin', ORIGIN, '--force'], 'unexpected argument'],
    ] as const) {
      const result = await run('set', ...args);
      expect([args, result.code, result.out]).toEqual([args, 1, '']);
      expect(result.err).toContain(words);
    }
    expect(fs.readFileSync(configFile()).equals(before)).toBe(true);
    expect(backups()).toEqual([]);
    expect(
      (
        await run(
          'set',
          ...Array.from({ length: 9 }, (_, i) => ['--address', `10.0.0.${i + 20}`]).flat(),
          '--origin',
          ORIGIN
        )
      ).err
    ).toContain('more than 8');
  });

  it('will not silently replace a local proxy kind', async () => {
    seedConfig({
      trusted_proxy: 'tailscale-serve',
      public_origin: 'https://vm.tailnet.example.test',
    });
    const refused = await run('set', '--address', PROXY, '--origin', ORIGIN);
    expect(refused.code).toBe(1);
    expect(refused.err).toContain('--replace');
    expect(backups()).toEqual([]);
    const replaced = await run('set', '--address', PROXY, '--origin', ORIGIN, '--replace');
    expect(replaced.code).toBe(0);
    invalidateConfigCache();
    expect(loadOrCreateUnifiedConfig().dashboard_tls?.trusted_proxy).toBe('lan-https-proxy');
  });

  it('reports a refused list in status, and turns the proxy off with a backup', async () => {
    seedConfig({
      trusted_proxy: 'lan-https-proxy',
      trusted_proxy_addresses: ['127.0.0.1'],
      public_origin: ORIGIN,
      https_listener: { enabled: false },
    });
    const status = await run('status');
    expect(status.out).toContain('LAN HTTPS proxy: off');
    expect(status.out).toContain('loopback');
    const othersBefore = withoutTls();
    const off = await run('off');
    expect(off.code).toBe(0);
    expect(off.out).toContain('LAN HTTPS proxy off');
    expect(backups()).toHaveLength(1);
    invalidateConfigCache();
    // The proxy keys go; other dashboard_tls keys stay.
    expect(loadOrCreateUnifiedConfig().dashboard_tls).toEqual({
      https_listener: { enabled: false },
    });
    expect(withoutTls()).toEqual(othersBefore);
    expect((await run('off')).out).toContain('already off');
    expect(backups()).toHaveLength(1);
  });

  it('removes an empty dashboard_tls block when turning off', async () => {
    seedConfig();
    expect((await run('set', '--address', PROXY, '--origin', ORIGIN)).code).toBe(0);
    expect((await run('off')).code).toBe(0);
    invalidateConfigCache();
    expect(loadOrCreateUnifiedConfig().dashboard_tls).toBeUndefined();
    expect(fs.readFileSync(configFile(), 'utf8')).not.toContain('dashboard_tls');
    expect(backups()).toEqual([
      'config.yaml.bak-proxy-20261007T080910Z',
      'config.yaml.bak-proxy-20261007T080910Z-2',
    ]);
  });

  it('prints help and refuses unknown commands', async () => {
    expect((await run()).out).toContain('Usage: ai-account-center dashboard proxy');
    expect((await run('set', '--help')).out).toContain('--origin');
    const unknown = await run('enable');
    expect(unknown.code).toBe(1);
    expect(unknown.err).toContain('Unknown proxy command');
    expect((await run('status', 'extra')).code).toBe(1);
  });
});
