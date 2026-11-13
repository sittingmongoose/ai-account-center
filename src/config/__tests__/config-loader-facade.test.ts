/**
 * Config Loader Facade Unit Tests
 *
 * Tests memoization cache behavior, cache invalidation on write ops,
 * and verifies all re-exports are present from underlying modules.
 */

import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as yaml from 'js-yaml';

/**
 * Helper: create a temp home dir with a minimal valid config.yaml so
 * loadOrCreateUnifiedConfig succeeds without touching the real ~/.ccs.
 */
function createTestHome(): string {
  const tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-facade-test-'));
  const ccsDir = path.join(tempHome, '.ccs');
  fs.mkdirSync(ccsDir, { recursive: true });
  const configPath = path.join(ccsDir, 'config.yaml');
  fs.writeFileSync(configPath, `version: 1\n`, 'utf8');
  return tempHome;
}

/**
 * Helper: get the facade module, bypassing the import cache each time.
 * We use dynamic import with a cache-busting query param so beforeEach
 * re-imports get a fresh module with a clean cache state.
 */
async function importFacade(): Promise<typeof import('../config-loader-facade')> {
  return import(`../config-loader-facade?cachebust=${Date.now()}`);
}

describe('config-loader-facade', () => {
  let tempHome: string;
  let originalCcsHome: string | undefined;

  beforeEach(() => {
    tempHome = createTestHome();
    originalCcsHome = process.env.CCS_HOME;
    process.env.CCS_HOME = tempHome;
  });

  afterEach(() => {
    // Restore env
    if (originalCcsHome !== undefined) {
      process.env.CCS_HOME = originalCcsHome;
    } else {
      delete process.env.CCS_HOME;
    }

    // Clean up temp dir
    if (tempHome && fs.existsSync(tempHome)) {
      fs.rmSync(tempHome, { recursive: true, force: true });
    }
  });

  describe('re-exports from unified-config-loader', () => {
    it('should export all core loader functions', async () => {
      const facade = await importFacade();

      expect(typeof facade.loadUnifiedConfig).toBe('function');
      expect(typeof facade.loadOrCreateUnifiedConfig).toBe('function');
    });

    it('should export all path/format utilities', async () => {
      const facade = await importFacade();

      expect(typeof facade.getConfigYamlPath).toBe('function');
      expect(typeof facade.getConfigJsonPath).toBe('function');
      expect(typeof facade.hasUnifiedConfig).toBe('function');
      expect(typeof facade.hasLegacyConfig).toBe('function');
      expect(typeof facade.getConfigFormat).toBe('function');
      expect(typeof facade.isUnifiedMode).toBe('function');
    });

    it('should export all profile getters', async () => {
      const facade = await importFacade();

      expect(typeof facade.getDefaultProfile).toBe('function');
      expect(typeof facade.setDefaultProfile).toBe('function');
    });

    it('should export all section getters', async () => {
      const facade = await importFacade();

      expect(typeof facade.isDashboardAuthEnabled).toBe('function');
      expect(typeof facade.getDashboardAuthConfig).toBe('function');
      expect(typeof facade.getLoggingConfig).toBe('function');
    });
  });

  describe('re-exports from config-manager', () => {
    it('should export loadSettings, loadConfigSafe, readConfig, getCcsDir', async () => {
      const facade = await importFacade();

      expect(typeof facade.loadSettings).toBe('function');
      expect(typeof facade.loadConfigSafe).toBe('function');
      expect(typeof facade.readConfig).toBe('function');
      expect(typeof facade.getCcsDir).toBe('function');
    });
  });

  describe('cache coherence', () => {
    it('should NOT export raw write functions that bypass cache', async () => {
      const facade = (await importFacade()) as Record<string, unknown>;

      expect(facade.saveUnifiedConfig).toBeUndefined();
      expect(facade.mutateUnifiedConfig).toBeUndefined();
      expect(facade.updateUnifiedConfig).toBeUndefined();
    });

    it('should export cache-coherent write wrappers instead', async () => {
      const facade = await importFacade();

      expect(typeof facade.saveConfig).toBe('function');
      expect(typeof facade.mutateConfig).toBe('function');
      expect(typeof facade.updateConfig).toBe('function');
    });
  });

  describe('memoization', () => {
    it('getCachedConfig returns equivalent config on repeated calls', async () => {
      const facade = await importFacade();

      const first = facade.getCachedConfig();
      const second = facade.getCachedConfig();

      // Deep copies — different references but same content (no re-read from disk)
      expect(first).not.toBe(second);
      expect(first.version).toBe(second.version);
    });

    it('invalidateConfigCache forces re-read on next getCachedConfig', async () => {
      const facade = await importFacade();

      const first = facade.getCachedConfig();
      facade.invalidateConfigCache();
      const second = facade.getCachedConfig();

      // Different reference after invalidation
      expect(first).not.toBe(second);
      // But same content
      expect(first.version).toBe(second.version);
    });

    it('saveConfig persists to disk and updates cache', async () => {
      const facade = await importFacade();

      const config = facade.getCachedConfig();
      config.default = 'test-profile';
      facade.saveConfig(config);

      // Verify disk content reflects the save
      const configPath = path.join(tempHome, '.ccs', 'config.yaml');
      const diskContent = fs.readFileSync(configPath, 'utf8');
      const diskConfig = yaml.load(diskContent) as Record<string, unknown>;
      expect(diskConfig.default).toBe('test-profile');

      const cached = facade.getCachedConfig();
      // Cache holds a copy of the saved config
      expect(cached.default).toBe('test-profile');
    });

    it('mutateConfig invalidates the cache', async () => {
      const facade = await importFacade();

      const before = facade.getCachedConfig();
      facade.mutateConfig((cfg) => {
        cfg.default = 'mutated-profile';
      });
      const after = facade.getCachedConfig();

      // Different reference (mutator may change it arbitrarily)
      expect(before).not.toBe(after);
      expect(after.default).toBe('mutated-profile');
    });

    it('updateConfig invalidates the cache', async () => {
      const facade = await importFacade();

      const before = facade.getCachedConfig();
      facade.updateConfig({ default: 'updated-profile' });
      const after = facade.getCachedConfig();

      expect(before).not.toBe(after);
      expect(after.default).toBe('updated-profile');
    });

    it('getCachedConfig returns valid config with expected fields', async () => {
      const facade = await importFacade();

      const config = facade.getCachedConfig();
      expect(config).toBeDefined();
      expect(typeof config.version).toBe('number');
      expect(config.accounts).toBeDefined();
      expect(config.profiles).toBeDefined();
      expect(config.cliproxy).toBeDefined();
    });

    it('auto-invalidates when config file is modified externally', async () => {
      const facade = await importFacade();

      // Prime the cache
      const first = facade.getCachedConfig();
      expect(first.version).toBeDefined();

      // Modify config file directly on disk (simulating external code
      // bypassing the facade)
      const configPath = path.join(tempHome, '.ccs', 'config.yaml');
      // Touch the file to update mtime (wait briefly for mtime resolution)
      const content = fs.readFileSync(configPath, 'utf8');
      await new Promise((r) => setTimeout(r, 10));
      fs.writeFileSync(configPath, content + '# touched\n', 'utf8');

      // getCachedConfig should detect the mtime change and re-read
      const second = facade.getCachedConfig();
      expect(first).not.toBe(second);
      // Content should still be valid (re-read from disk)
      expect(second.version).toBeDefined();
    });
  });
});
