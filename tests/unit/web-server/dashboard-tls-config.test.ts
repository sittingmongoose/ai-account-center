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
} from '../../../src/web-server/services/dashboard-tls-config';

let original: string | undefined;
let folder = '';

beforeEach(() => {
  original = process.env.CCS_DIR;
  folder = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-tls-config-'));
  process.env.CCS_DIR = folder;
});

afterEach(() => {
  if (original === undefined) delete process.env.CCS_DIR;
  else process.env.CCS_DIR = original;
  fs.rmSync(folder, { recursive: true, force: true });
});

describe('dashboard_tls', () => {
  it('is off by default', () => {
    saveUnifiedConfig(createEmptyUnifiedConfig());
    expect(getDashboardTlsSettings()).toEqual({
      trustedProxy: null,
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
      httpsListener: { port: 3443, certPath: '/tls/c.crt', keyPath: '/tls/k.key' },
      publicOrigin: 'https://vm.tailnet.example.ts.net',
    });
    expect(
      parseDashboardTlsSettings({
        trusted_proxy: 'anything',
        https_listener: { enabled: false, cert_path: '/c', key_path: '/k' },
        public_origin: 'http://plain.example',
      })
    ).toEqual({ trustedProxy: null, httpsListener: null, publicOrigin: null });
    for (const origin of ['https://user:pw@x.example', 'https://x.example/path', 'nope']) {
      expect(parseDashboardTlsSettings({ public_origin: origin }).publicOrigin).toBeNull();
    }
    expect(
      parseDashboardTlsSettings({
        https_listener: { enabled: true, port: 70000, cert_path: '/c', key_path: '/k' },
      }).httpsListener
    ).toBeNull();
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
