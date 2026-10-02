/** Retained private account schema. */

import * as fs from 'fs';

import * as os from 'os';

import * as path from 'path';

import { afterEach, beforeEach, describe, expect, it } from 'bun:test';

function createTestHome(): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-pool-routing-test-'));
  const ccsDir = path.join(dir, '.ccs');
  fs.mkdirSync(ccsDir, { recursive: true });
  // Minimal config.yaml — loadOrCreateUnifiedConfig will expand it on first read
  fs.writeFileSync(path.join(ccsDir, 'config.yaml'), 'version: 1\n', 'utf8');
  return dir;
}

describe('Account registry compatibility schema', () => {
  let tempHome: string;
  let originalCcsHome: string | undefined;
  beforeEach(() => {
    tempHome = createTestHome();
    originalCcsHome = process.env.CCS_HOME;
    process.env.CCS_HOME = tempHome;
  });
  afterEach(() => {
    if (originalCcsHome !== undefined) {
      process.env.CCS_HOME = originalCcsHome;
    } else {
      delete process.env.CCS_HOME;
    }
    fs.rmSync(tempHome, { recursive: true, force: true });
  });
  describe('CLIProxyPoolRoutingConfig schema', () => {
    it('pool_routing.enabled defaults to absent (falsy) for a new user config', async () => {
      const { loadOrCreateUnifiedConfig } = await import(
        `../../config/config-loader-facade?p3schema=${Date.now()}`
      );
      const cfg = loadOrCreateUnifiedConfig();
      expect(cfg.cliproxy?.pool_routing?.enabled).toBeUndefined();
    });

    it('pool_routing.prompt_dismissed defaults to absent', async () => {
      const { loadOrCreateUnifiedConfig } = await import(
        `../../config/config-loader-facade?p3dismissed=${Date.now()}`
      );
      const cfg = loadOrCreateUnifiedConfig();
      expect(cfg.cliproxy?.pool_routing?.prompt_dismissed).toBeUndefined();
    });

    it('pool_routing.max_retry_credentials can be set and read back', async () => {
      const { loadOrCreateUnifiedConfig, mutateConfig, invalidateConfigCache } = await import(
        `../../config/config-loader-facade?p3retry=${Date.now()}`
      );
      mutateConfig((cfg: { cliproxy?: { pool_routing?: Record<string, unknown> } }) => {
        cfg.cliproxy = cfg.cliproxy ?? {};
        cfg.cliproxy.pool_routing = { enabled: true, max_retry_credentials: 5 };
      });
      invalidateConfigCache();
      const cfg = loadOrCreateUnifiedConfig();
      expect(cfg.cliproxy?.pool_routing?.max_retry_credentials).toBe(5);
    });
  });
});
