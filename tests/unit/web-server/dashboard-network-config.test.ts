/**
 * `dashboard_network` in config.yaml (CONTRACT-auth-devices 2a, rule 4): off by
 * default, read as written by hand, kept through a load and an unrelated save,
 * and re-read after a write. Temporary CCS_DIR only.
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
import { invalidateConfigCache } from '../../../src/config/config-loader-facade';
import { DEFAULT_TRUSTED_NETWORKS } from '../../../src/web-server/middleware/trusted-networks';
import {
  getDashboardNetworkSettings,
  invalidateDashboardNetworkSettings,
} from '../../../src/web-server/services/dashboard-network-config';

let original: string | undefined;
let folder = '';

beforeEach(() => {
  original = process.env.CCS_DIR;
  folder = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-network-config-'));
  process.env.CCS_DIR = folder;
  invalidateConfigCache();
  invalidateDashboardNetworkSettings();
});

afterEach(() => {
  if (original === undefined) delete process.env.CCS_DIR;
  else process.env.CCS_DIR = original;
  invalidateConfigCache();
  invalidateDashboardNetworkSettings();
  fs.rmSync(folder, { recursive: true, force: true });
});

describe('dashboard_network', () => {
  it('is off by default, with the private ranges plus loopback', () => {
    saveUnifiedConfig(createEmptyUnifiedConfig());
    expect(loadOrCreateUnifiedConfig().dashboard_network).toBeUndefined();
    expect(getDashboardNetworkSettings()).toMatchObject({
      trustLocalNetwork: false,
      trustedNetworks: [...DEFAULT_TRUSTED_NETWORKS],
      rejectedEntries: 0,
    });
  });

  it('reads a block written by hand, and leaves out entries that are not ranges', () => {
    saveUnifiedConfig(createEmptyUnifiedConfig());
    const file = path.join(folder, 'config.yaml');
    fs.appendFileSync(
      file,
      [
        'dashboard_network:',
        '  trust_local_network: true',
        '  trusted_networks:',
        '    - 192.168.10.0/24',
        '    - 10.6.0.0/24',
        '    - not-a-range',
        '',
      ].join('\n')
    );
    invalidateConfigCache();
    expect(getDashboardNetworkSettings()).toMatchObject({
      trustLocalNetwork: true,
      trustedNetworks: ['192.168.10.0/24', '10.6.0.0/24'],
      rejectedEntries: 1,
    });
  });

  it('survives a load and an unrelated save, and is re-read after a write', () => {
    const config = createEmptyUnifiedConfig();
    config.dashboard_network = { trust_local_network: true, trusted_networks: ['10.6.0.0/24'] };
    saveUnifiedConfig(config);
    mutateUnifiedConfig((current) => {
      current.default = undefined;
    });
    expect(loadOrCreateUnifiedConfig().dashboard_network).toEqual({
      trust_local_network: true,
      trusted_networks: ['10.6.0.0/24'],
    });
    expect(fs.readFileSync(path.join(folder, 'config.yaml'), 'utf8')).toContain(
      'trusted_networks defaults to'
    );
    invalidateConfigCache();
    expect(getDashboardNetworkSettings().trustLocalNetwork).toBe(true);
    mutateUnifiedConfig((current) => {
      current.dashboard_network = { ...current.dashboard_network, trust_local_network: false };
    });
    invalidateConfigCache();
    invalidateDashboardNetworkSettings();
    expect(getDashboardNetworkSettings()).toMatchObject({
      trustLocalNetwork: false,
      trustedNetworks: ['10.6.0.0/24'],
    });
  });
});
