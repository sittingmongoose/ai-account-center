/** Retained private account schema and cross-lane identity safety. */

import * as fs from 'fs';

import * as os from 'os';

import * as path from 'path';

import { afterEach, beforeEach, describe, expect, it, spyOn } from 'bun:test';

import * as claudeDetector from '../../utils/claude-detector';

import { checkCrossLaneEmailOverlap } from '../accounts/account-safety-cross-lane';

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

describe('Phase 3: cross-lane email overlap guard', () => {
  let tempHome: string;
  let originalCcsHome: string | undefined;
  let stderrOutput: string[];

  beforeEach(() => {
    tempHome = createTestHome();
    originalCcsHome = process.env.CCS_HOME;
    process.env.CCS_HOME = tempHome;
    stderrOutput = [];
  });

  afterEach(() => {
    if (originalCcsHome !== undefined) {
      process.env.CCS_HOME = originalCcsHome;
    } else {
      delete process.env.CCS_HOME;
    }
    fs.rmSync(tempHome, { recursive: true, force: true });
  });

  // ── 12. Same email → warning ────────────────────────────────────────────
  it('warns when CLIProxy agy account email matches native Claude email', () => {
    // spyOn the statically-imported module so the same instance is used by
    // checkCrossLaneEmailOverlap (which also imports from the same module).
    const spy = spyOn(claudeDetector, 'getClaudeAuthStatus').mockReturnValue({
      loggedIn: true,
      email: 'test@example.com',
      authMethod: 'claude.ai',
      apiProvider: null,
      orgId: null,
      orgName: null,
      subscriptionType: null,
    });

    const consoleErrorSpy = spyOn(console, 'error').mockImplementation((msg?: unknown) => {
      if (typeof msg === 'string') stderrOutput.push(msg);
    });

    checkCrossLaneEmailOverlap('agy', 'test@example.com');

    const combined = stderrOutput.join('\n');
    expect(combined).toContain('cross-lane email overlap');

    spy.mockRestore();
    consoleErrorSpy.mockRestore();
  });

  // ── 13. Different email → no warning ────────────────────────────────────
  it('does not warn when emails differ', () => {
    const spy = spyOn(claudeDetector, 'getClaudeAuthStatus').mockReturnValue({
      loggedIn: true,
      email: 'other@example.com',
      authMethod: 'claude.ai',
      apiProvider: null,
      orgId: null,
      orgName: null,
      subscriptionType: null,
    });

    const consoleErrorSpy = spyOn(console, 'error').mockImplementation((msg?: unknown) => {
      if (typeof msg === 'string') stderrOutput.push(msg);
    });

    checkCrossLaneEmailOverlap('agy', 'different@example.com');

    expect(stderrOutput.join('\n')).not.toContain('cross-lane');

    spy.mockRestore();
    consoleErrorSpy.mockRestore();
  });

  // ── 14. Not logged in → silent ──────────────────────────────────────────
  it('is silent when native Claude is not logged in', () => {
    const spy = spyOn(claudeDetector, 'getClaudeAuthStatus').mockReturnValue({
      loggedIn: false,
      email: null,
      authMethod: null,
      apiProvider: null,
      orgId: null,
      orgName: null,
      subscriptionType: null,
    });

    const consoleErrorSpy = spyOn(console, 'error').mockImplementation((msg?: unknown) => {
      if (typeof msg === 'string') stderrOutput.push(msg);
    });

    checkCrossLaneEmailOverlap('claude', 'test@example.com');

    expect(stderrOutput.join('\n')).not.toContain('cross-lane');

    spy.mockRestore();
    consoleErrorSpy.mockRestore();
  });

  it('is silent when getClaudeAuthStatus throws (CLI not installed)', () => {
    const spy = spyOn(claudeDetector, 'getClaudeAuthStatus').mockImplementation(() => {
      throw new Error('Command not found: claude');
    });

    const consoleErrorSpy = spyOn(console, 'error').mockImplementation((msg?: unknown) => {
      if (typeof msg === 'string') stderrOutput.push(msg);
    });

    // Must not throw
    expect(() => checkCrossLaneEmailOverlap('claude', 'test@example.com')).not.toThrow();
    expect(stderrOutput.join('\n')).not.toContain('cross-lane');

    spy.mockRestore();
    consoleErrorSpy.mockRestore();
  });

  // ── Account-profile lane enumeration (isolated CLAUDE_CONFIG_DIR lanes) ──
  // The ambient ~/.claude check alone misses the isolated lanes of CCS account
  // profiles — exactly the multi-account population the guard protects. These
  // tests prove the guard also reads each profile lane's .claude.json email.

  /** Write a profiles.json account entry + its instance-lane .claude.json email. */
  function seedAccountLane(ccsDir: string, name: string, email: string | null): void {
    const profilesPath = path.join(ccsDir, 'profiles.json');
    let payload: { version: string; profiles: Record<string, unknown>; default: string | null };
    try {
      payload = JSON.parse(fs.readFileSync(profilesPath, 'utf-8'));
    } catch {
      payload = { version: '2.0.0', profiles: {}, default: null };
    }
    payload.profiles[name] = {
      type: 'account',
      created: new Date().toISOString(),
      last_used: null,
    };
    fs.writeFileSync(profilesPath, JSON.stringify(payload, null, 2), 'utf8');

    const instanceDir = path.join(ccsDir, 'instances', name);
    fs.mkdirSync(instanceDir, { recursive: true });
    if (email !== null) {
      fs.writeFileSync(
        path.join(instanceDir, '.claude.json'),
        JSON.stringify({ oauthAccount: { emailAddress: email } }, null, 2),
        'utf8'
      );
    }
  }

  it('warns when a CCS account-profile lane email matches, even though ambient ~/.claude is logged out', () => {
    const ccsDir = path.join(tempHome, '.ccs');
    seedAccountLane(ccsDir, 'work', 'lane@example.com');

    // Ambient default lane is logged OUT — old guard would stay silent.
    const spy = spyOn(claudeDetector, 'getClaudeAuthStatus').mockReturnValue({
      loggedIn: false,
      email: null,
      authMethod: null,
      apiProvider: null,
      orgId: null,
      orgName: null,
      subscriptionType: null,
    });
    const consoleErrorSpy = spyOn(console, 'error').mockImplementation((msg?: unknown) => {
      if (typeof msg === 'string') stderrOutput.push(msg);
    });

    checkCrossLaneEmailOverlap('agy', 'lane@example.com');

    const combined = stderrOutput.join('\n');
    expect(combined).toContain('cross-lane email overlap');
    // The matching profile lane is named so the user knows which lane overlaps.
    expect(combined).toContain('profile "work"');

    spy.mockRestore();
    consoleErrorSpy.mockRestore();
  });

  it('does not warn when no account-profile lane email matches', () => {
    const ccsDir = path.join(tempHome, '.ccs');
    seedAccountLane(ccsDir, 'work', 'someone-else@example.com');

    const spy = spyOn(claudeDetector, 'getClaudeAuthStatus').mockReturnValue({
      loggedIn: false,
      email: null,
      authMethod: null,
      apiProvider: null,
      orgId: null,
      orgName: null,
      subscriptionType: null,
    });
    const consoleErrorSpy = spyOn(console, 'error').mockImplementation((msg?: unknown) => {
      if (typeof msg === 'string') stderrOutput.push(msg);
    });

    checkCrossLaneEmailOverlap('agy', 'mine@example.com');

    expect(stderrOutput.join('\n')).not.toContain('cross-lane');

    spy.mockRestore();
    consoleErrorSpy.mockRestore();
  });

  it('silently skips an account-profile lane with a missing/unreadable .claude.json', () => {
    const ccsDir = path.join(tempHome, '.ccs');
    // Account profile exists but its lane has no .claude.json (email === null).
    seedAccountLane(ccsDir, 'broken', null);

    const spy = spyOn(claudeDetector, 'getClaudeAuthStatus').mockReturnValue({
      loggedIn: false,
      email: null,
      authMethod: null,
      apiProvider: null,
      orgId: null,
      orgName: null,
      subscriptionType: null,
    });
    const consoleErrorSpy = spyOn(console, 'error').mockImplementation((msg?: unknown) => {
      if (typeof msg === 'string') stderrOutput.push(msg);
    });

    // Must not warn and must not throw on the unreadable lane.
    expect(() => checkCrossLaneEmailOverlap('agy', 'lane@example.com')).not.toThrow();
    expect(stderrOutput.join('\n')).not.toContain('cross-lane');

    spy.mockRestore();
    consoleErrorSpy.mockRestore();
  });
});
