import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
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
import { createEmptyUnifiedConfig } from '../../../src/config/unified-config-types';
import { getTierLockForProvider } from '../../../src/config/schemas/quota';
import { runWithScopedCcsHome } from '../../../src/utils/config-manager';

describe('retained quota-tier config persistence', () => {
  let tempHome = '';

  beforeEach(() => {
    tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-tier-lock-config-'));
    invalidateConfigCache();
  });

  afterEach(() => {
    invalidateConfigCache();
    fs.rmSync(tempHome, { recursive: true, force: true });
    tempHome = '';
  });

  it('saves a named tier lock for one provider without locking other providers', async () => {
    await runWithScopedCcsHome(tempHome, () => {
      const config = structuredClone(createEmptyUnifiedConfig());
      if (!config.quota_management)
        throw new Error('Quota configuration defaults were not created');
      config.quota_management.manual.tier_lock = { agy: 'ultra' };
      saveConfig(config);

      const reloaded = loadUnifiedConfig();
      expect(getConfigYamlPath()).toBe(path.join(tempHome, '.ccs', 'config.yaml'));
      expect(reloaded?.quota_management?.manual.tier_lock).toEqual({ agy: 'ultra' });
      expect(getTierLockForProvider(reloaded?.quota_management?.manual, 'agy')).toBe('ultra');
      expect(getTierLockForProvider(reloaded?.quota_management?.manual, 'codex')).toBeNull();
      expect(getTierLockForProvider(reloaded?.quota_management?.manual, 'gemini')).toBeNull();
    });
  });

  it('persists clearing one provider lock while retaining another provider lock', async () => {
    await runWithScopedCcsHome(tempHome, () => {
      const config = structuredClone(createEmptyUnifiedConfig());
      if (!config.quota_management)
        throw new Error('Quota configuration defaults were not created');
      config.quota_management.manual.tier_lock = { agy: 'pro', codex: 'free' };
      saveConfig(config);
      expect(loadUnifiedConfig()?.quota_management?.manual.tier_lock).toEqual({
        agy: 'pro',
        codex: 'free',
      });

      mutateConfig((current) => {
        const tierLocks = current.quota_management?.manual.tier_lock;
        if (!tierLocks) throw new Error('Saved provider tier locks were not reloaded');
        tierLocks.agy = null;
      });

      const reloaded = loadUnifiedConfig();
      expect(reloaded?.quota_management?.manual.tier_lock).toEqual({ agy: null, codex: 'free' });
      expect(getTierLockForProvider(reloaded?.quota_management?.manual, 'agy')).toBeNull();
      expect(getTierLockForProvider(reloaded?.quota_management?.manual, 'codex')).toBe('free');
    });
  });

  it('retains the per-provider map across an unrelated config mutation and reload', async () => {
    await runWithScopedCcsHome(tempHome, () => {
      const tierLocks = {
        agy: 'ultra',
        claude: 'pro',
        codex: 'free',
        gemini: 'unknown',
        ghcp: 'pro',
      };
      const config = structuredClone(createEmptyUnifiedConfig());
      if (!config.quota_management)
        throw new Error('Quota configuration defaults were not created');
      config.quota_management.manual.tier_lock = tierLocks;
      saveConfig(config);
      expect(loadUnifiedConfig()?.quota_management?.manual.tier_lock).toEqual(tierLocks);

      mutateConfig((current) => {
        current.preferences.theme = 'dark';
      });

      const reloaded = loadUnifiedConfig();
      expect(reloaded?.preferences.theme).toBe('dark');
      expect(reloaded?.quota_management?.manual.tier_lock).toEqual(tierLocks);
      for (const [provider, tier] of Object.entries(tierLocks)) {
        expect(getTierLockForProvider(reloaded?.quota_management?.manual, provider)).toBe(tier);
      }
    });
  });
});
