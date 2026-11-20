import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import { spawnSync } from 'node:child_process';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  getConfigYamlPath,
  invalidateConfigCache,
  loadUnifiedConfig,
  mutateConfig,
  saveConfig,
} from '../../../src/config/config-loader-facade';
import { acquireLock, releaseLock } from '../../../src/config/loader/io-locks';
import { claimWriterLease } from '../../../src/shared/private-file-lease';
import { createEmptyUnifiedConfig } from '../../../src/config/unified-config-types';
import { runWithScopedCcsHome } from '../../../src/utils/config-manager';

describe('dashboard authentication configuration persistence', () => {
  let tempHome = '';

  beforeEach(() => {
    tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-dashboard-auth-persistence-'));
    invalidateConfigCache();
  });

  afterEach(() => {
    invalidateConfigCache();
    fs.rmSync(tempHome, { recursive: true, force: true });
    tempHome = '';
  });

  it('preserves disabled authentication and its configured values across an unrelated mutation', async () => {
    await runWithScopedCcsHome(tempHome, () => {
      const configuredAuth = {
        enabled: true,
        username: 'fixture-dashboard-admin',
        password_hash: 'fixture-hash-not-a-real-credential',
        session_timeout_hours: 7,
      };
      const config = createEmptyUnifiedConfig();
      config.dashboard_auth = configuredAuth;
      saveConfig(config);

      const enabledReload = loadUnifiedConfig();
      expect(enabledReload?.dashboard_auth).toEqual(configuredAuth);
      if (!enabledReload?.dashboard_auth) throw new Error('Authentication fixture was not saved');

      enabledReload.dashboard_auth.enabled = false;
      saveConfig(enabledReload);
      const disabledAuth = { ...configuredAuth, enabled: false };
      const disabledReload = loadUnifiedConfig();
      expect(disabledReload?.dashboard_auth).toEqual(disabledAuth);
      expect(getConfigYamlPath()).toBe(path.join(tempHome, '.ccs', 'config.yaml'));
      expect(fs.existsSync(getConfigYamlPath())).toBe(true);

      // The locked mutation reloads from disk and fills defaults before saving.
      // A lost disabled block would reset the custom credentials and timeout here.
      mutateConfig((current) => {
        current.preferences.theme = 'dark';
      });

      const finalReload = loadUnifiedConfig();
      expect(finalReload?.preferences.theme).toBe('dark');
      // The save fills the derived lifetime: legacy 7 hours map to 1 day.
      expect(finalReload?.dashboard_auth).toEqual({ ...disabledAuth, session_lifetime_days: 1 });
    });
  });

  it.skipIf(process.platform !== 'linux')(
    'serializes TS takeover of an interrupted config save with the native reclaimer',
    async () => {
      await runWithScopedCcsHome(tempHome, () => {
        saveConfig(createEmptyUnifiedConfig());
        const root = path.dirname(getConfigYamlPath());
        const marker = path.join(root, 'config.yaml.lock');
        const exited = spawnSync('sh', ['-c', 'exit 0']);
        expect(exited.status).toBe(0);
        const stale = [exited.pid, Date.now(), 'interrupted-write'].join('\n');
        fs.writeFileSync(marker, stale, { mode: 0o600 });
        const held = claimWriterLease(root, path.join(root, 'config.yaml.reclaim.lock'));
        try {
          expect(acquireLock()).toBeNull();
          expect(fs.readFileSync(marker, 'utf8')).toBe(stale);
        } finally {
          held.close();
        }
        const token = acquireLock();
        try {
          expect(token).toBeTruthy();
          expect(acquireLock()).toBeNull();
        } finally {
          if (token) releaseLock(token);
        }
      });
    }
  );

  it('does not invent an authentication block when directly saving a configuration without one', async () => {
    await runWithScopedCcsHome(tempHome, () => {
      const config = createEmptyUnifiedConfig();
      delete config.dashboard_auth;
      saveConfig(config);

      const firstReload = loadUnifiedConfig();
      expect(firstReload).not.toBeNull();
      expect(firstReload).not.toHaveProperty('dashboard_auth');
      if (!firstReload) throw new Error('Configuration fixture was not saved');

      firstReload.preferences.theme = 'dark';
      saveConfig(firstReload);

      const finalReload = loadUnifiedConfig();
      expect(finalReload?.preferences.theme).toBe('dark');
      expect(finalReload).not.toHaveProperty('dashboard_auth');
    });
  });
});
