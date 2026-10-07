/**
 * `dashboard_tls` and `dashboard_auth.password_changed_at` in config.yaml
 * (CONTRACT-auth-devices 2a and 3): off by default, normalised on read, and
 * kept through a load and save. Temporary CCS_DIR only.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

import { createEmptyUnifiedConfig } from '../../../src/config/unified-config-types';
import {
  loadOrCreateUnifiedConfig,
  mutateUnifiedConfig,
  saveUnifiedConfig,
} from '../../../src/config/unified-config-loader';
import {
  getDashboardTlsSettings,
  parseDashboardTlsSettings,
  parseTrustedProxyAddresses,
  setThisComputerAddressesForTests,
} from '../../../src/web-server/services/dashboard-tls-config';
import {
  clearRecentLogEntries,
  getRecentLogEntries,
} from '../../../src/services/logging/log-buffer';
import { invalidateLoggingConfigCache } from '../../../src/services/logging/log-config';

/** The dashboard computer's own addresses, as the parser is told in these tests. */
const SELF = ['127.0.0.1', '::1', '192.168.1.10', 'fd00::10'];
const OFF_PROXY = { trustedProxyAddresses: [], trustedProxyProblem: null };

let original: string | undefined;
let folder = '';

beforeEach(() => {
  original = process.env.CCS_DIR;
  folder = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-tls-config-'));
  process.env.CCS_DIR = folder;
});

afterEach(() => {
  setThisComputerAddressesForTests(null);
  if (original === undefined) delete process.env.CCS_DIR;
  else process.env.CCS_DIR = original;
  fs.rmSync(folder, { recursive: true, force: true });
});

describe('dashboard_tls', () => {
  it('is off by default', () => {
    saveUnifiedConfig(createEmptyUnifiedConfig());
    expect(getDashboardTlsSettings()).toEqual({
      trustedProxy: null,
      ...OFF_PROXY,
      httpsListener: null,
      publicOrigin: null,
    });
    expect(fs.readFileSync(path.join(folder, 'config.yaml'), 'utf8')).not.toContain(
      'dashboard_tls'
    );
  });

  it('normalises values and ignores malformed ones', () => {
    expect(
      parseDashboardTlsSettings({
        trusted_proxy: 'tailscale-serve',
        https_listener: { enabled: true, cert_path: '/tls/c.crt', key_path: '/tls/k.key' },
        public_origin: 'https://vm.tailnet.example.ts.net/',
      })
    ).toEqual({
      trustedProxy: 'tailscale-serve',
      ...OFF_PROXY,
      httpsListener: { port: 3443, certPath: '/tls/c.crt', keyPath: '/tls/k.key' },
      publicOrigin: 'https://vm.tailnet.example.ts.net',
    });
    expect(
      parseDashboardTlsSettings({
        trusted_proxy: 'anything',
        https_listener: { enabled: false, cert_path: '/c', key_path: '/k' },
        public_origin: 'http://plain.example',
      })
    ).toEqual({ trustedProxy: null, ...OFF_PROXY, httpsListener: null, publicOrigin: null });
    for (const origin of ['https://user:pw@x.example', 'https://x.example/path', 'nope']) {
      expect(parseDashboardTlsSettings({ public_origin: origin }).publicOrigin).toBeNull();
    }
    expect(
      parseDashboardTlsSettings({
        https_listener: { enabled: true, port: 70000, cert_path: '/c', key_path: '/k' },
      }).httpsListener
    ).toBeNull();
  });

  it('parses a lan-https-proxy with exact private addresses, canonical and deduplicated', () => {
    expect(
      parseDashboardTlsSettings(
        {
          trusted_proxy: 'lan-https-proxy',
          trusted_proxy_addresses: [
            '192.168.1.20',
            '::ffff:10.0.0.20',
            'fd00::20',
            'FD00::0020',
            '192.168.1.20',
            '172.16.5.20',
          ],
          public_origin: 'https://aac.example.test',
        },
        { selfAddresses: SELF }
      )
    ).toEqual({
      trustedProxy: 'lan-https-proxy',
      trustedProxyAddresses: ['192.168.1.20', '10.0.0.20', 'fd00::20', '172.16.5.20'],
      trustedProxyProblem: null,
      httpsListener: null,
      publicOrigin: 'https://aac.example.test',
    });
  });

  it('refuses the whole list for one entry it may not hold, and turns the kind off', () => {
    const cases: Array<[unknown, string, number | null]> = [
      [undefined, 'not_a_list', null],
      ['192.168.1.20', 'not_a_list', null],
      [{}, 'not_a_list', null],
      [[], 'empty', null],
      [Array.from({ length: 9 }, (_, index) => `192.168.1.${index + 20}`), 'too_many', null],
      [['192.168.1.20', '192.168.1.0/24'], 'not_an_address', 1],
      [['fd00::/8'], 'not_an_address', 0],
      [['*'], 'not_an_address', 0],
      [['192.168.1.*'], 'not_an_address', 0],
      [[''], 'not_an_address', 0],
      [[' 192.168.1.20 '], 'not_an_address', 0],
      [['proxy.example.test'], 'not_an_address', 0],
      [['192.168.1.20:443'], 'not_an_address', 0],
      [['[fd00::20]'], 'not_an_address', 0],
      [['fe80::1%eth0'], 'not_an_address', 0],
      [['999.1.1.1'], 'not_an_address', 0],
      [['010.0.0.20'], 'not_an_address', 0],
      [['0x0a.0.0.20'], 'not_an_address', 0],
      [['167772180'], 'not_an_address', 0],
      [[119], 'not_an_address', 0],
      [[null], 'not_an_address', 0],
      [['0.0.0.0'], 'unspecified', 0],
      [['::'], 'unspecified', 0],
      [['127.0.0.1'], 'loopback', 0],
      [['127.8.9.10'], 'loopback', 0],
      [['::1'], 'loopback', 0],
      [['::ffff:127.0.0.1'], 'loopback', 0],
      [['192.168.1.20', '203.0.113.9'], 'not_private', 1],
      [['198.51.100.9'], 'not_private', 0],
      [['2001:db8::9'], 'not_private', 0],
      [['169.254.1.9'], 'not_private', 0],
      [['fe80::9'], 'not_private', 0],
      [['100.64.0.9'], 'not_private', 0],
      [['192.168.1.10'], 'this_computer', 0],
      [['::ffff:192.168.1.10'], 'this_computer', 0],
      [['192.168.1.20', 'FD00::10'], 'this_computer', 1],
    ];
    for (const [addresses, reason, index] of cases) {
      expect([addresses, parseTrustedProxyAddresses(addresses, { selfAddresses: SELF })]).toEqual([
        addresses,
        { ok: false, problem: { reason, index } },
      ]);
      expect(
        parseDashboardTlsSettings(
          {
            trusted_proxy: 'lan-https-proxy',
            trusted_proxy_addresses: addresses,
            public_origin: 'https://aac.example.test',
          },
          { selfAddresses: SELF }
        )
      ).toMatchObject({
        trustedProxy: null,
        trustedProxyAddresses: [],
        trustedProxyProblem: { reason },
      });
    }
    // The local kinds never needed the list and ignore it.
    expect(
      parseDashboardTlsSettings(
        { trusted_proxy: 'loopback-https-proxy', trusted_proxy_addresses: ['127.0.0.1'] },
        { selfAddresses: SELF }
      )
    ).toMatchObject({ trustedProxy: 'loopback-https-proxy', ...OFF_PROXY });
  });

  it("reads this computer's own addresses from its interfaces by default", () => {
    // Loopback is refused as loopback before the self check; an interface address as this computer.
    const own = Object.values(os.networkInterfaces())
      .flat()
      .map((entry) => entry?.address)
      .find(
        (address): address is string =>
          typeof address === 'string' &&
          /^(10\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.)/.test(address)
      );
    if (own) {
      expect(parseTrustedProxyAddresses([own])).toEqual({
        ok: false,
        problem: { reason: 'this_computer', index: 0 },
      });
    }
  });

  it('logs one plain line per config change when the list is refused, without the address', () => {
    setThisComputerAddressesForTests(SELF);
    const config = createEmptyUnifiedConfig();
    config.logging = { ...config.logging, enabled: true, level: 'debug', redact: false };
    (config as unknown as Record<string, unknown>).dashboard_tls = {
      trusted_proxy: 'lan-https-proxy',
      trusted_proxy_addresses: ['127.0.0.1'],
    };
    const silenced = { ...console };
    for (const name of ['log', 'info', 'warn', 'error', 'debug'] as const) {
      console[name] = () => undefined;
    }
    try {
      saveUnifiedConfig(config);
      invalidateLoggingConfigCache();
      clearRecentLogEntries();
      for (let read = 0; read < 3; read += 1) {
        expect(getDashboardTlsSettings()).toMatchObject({
          trustedProxy: null,
          trustedProxyProblem: { reason: 'loopback', index: 0 },
        });
      }
      const lines = getRecentLogEntries().filter(
        (entry) => entry.event === 'auth.tls.proxy_refused'
      );
      expect(lines).toHaveLength(1);
      expect(lines[0].message).toContain('loopback');
      expect(lines[0].context).toEqual({ reason: 'loopback', entry: 1 });
      expect(JSON.stringify(lines[0])).not.toContain('127.0.0.1');
    } finally {
      Object.assign(console, silenced);
      clearRecentLogEntries();
      invalidateLoggingConfigCache();
    }
  });

  it('keeps the lan kind and its addresses through a load and an unrelated save', () => {
    setThisComputerAddressesForTests(SELF);
    const config = createEmptyUnifiedConfig();
    (config as unknown as Record<string, unknown>).dashboard_tls = {
      trusted_proxy: 'lan-https-proxy',
      trusted_proxy_addresses: ['192.168.1.20'],
      public_origin: 'https://aac.example.test',
    };
    saveUnifiedConfig(config);
    mutateUnifiedConfig((current) => {
      current.default = undefined;
    });
    expect(loadOrCreateUnifiedConfig().dashboard_tls).toEqual({
      trusted_proxy: 'lan-https-proxy',
      trusted_proxy_addresses: ['192.168.1.20'],
      public_origin: 'https://aac.example.test',
    });
    expect(getDashboardTlsSettings()).toMatchObject({
      trustedProxy: 'lan-https-proxy',
      trustedProxyAddresses: ['192.168.1.20'],
      publicOrigin: 'https://aac.example.test',
    });
  });

  it('survives a load and an unrelated save, with password_changed_at', () => {
    const config = createEmptyUnifiedConfig();
    (config as unknown as Record<string, unknown>).dashboard_tls = {
      trusted_proxy: 'loopback-https-proxy',
      public_origin: 'https://dash.example.test',
    };
    config.dashboard_auth = {
      enabled: true,
      username: 'operator',
      password_hash: '$2b$04$abcdefghijklmnopqrstuuJ8bS1kq5v2yHq9w1Ue2o0m6Yt1yF6xK',
      session_timeout_hours: 24,
      password_changed_at: '2026-10-02T12:00:00.000Z',
    };
    saveUnifiedConfig(config);
    mutateUnifiedConfig((current) => {
      current.default = undefined;
    });
    const loaded = loadOrCreateUnifiedConfig();
    expect(loaded.dashboard_tls).toEqual({
      trusted_proxy: 'loopback-https-proxy',
      public_origin: 'https://dash.example.test',
    });
    expect(loaded.dashboard_auth?.password_changed_at).toBe('2026-10-02T12:00:00.000Z');
    expect(getDashboardTlsSettings().trustedProxy).toBe('loopback-https-proxy');
  });
});
