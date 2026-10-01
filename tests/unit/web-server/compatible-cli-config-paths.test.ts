import { describe, expect, it } from 'bun:test';
import path from 'path';
import {
  resolveCodexConfigPaths,
  resolveDroidConfigPaths,
} from '../../../src/web-server/services/compatible-cli-config-paths';

describe('read-only native history paths', () => {
  it('keeps the Codex home override and TOML history location', () => {
    const resolved = resolveCodexConfigPaths({
      env: { CODEX_HOME: './fixture-codex-home' },
      homeDir: '/Users/tester',
    });
    const baseDir = path.resolve('./fixture-codex-home');
    expect(resolved).toEqual({
      baseDir,
      baseDirDisplay: '$CODEX_HOME',
      configPath: path.join(baseDir, 'config.toml'),
      configDisplayPath: '$CODEX_HOME/config.toml',
    });
  });

  it('uses the supplied Codex home when no override exists', () => {
    const resolved = resolveCodexConfigPaths({ env: {}, homeDir: '/fixture/home' });
    expect(resolved.baseDir).toBe(path.resolve('/fixture/home/.codex'));
    expect(resolved.configDisplayPath).toBe('~/.codex/config.toml');
  });

  it('keeps isolated Factory settings and legacy locations on Unix', () => {
    expect(
      resolveDroidConfigPaths({
        platform: 'darwin',
        env: { CCS_HOME: '/fixture/ccs-home' },
        homeDir: '/Users/tester',
      })
    ).toEqual({
      settingsPath: path.join('/fixture/ccs-home', '.factory', 'settings.json'),
      settingsDisplayPath: '~/.factory/settings.json',
      legacyConfigPath: path.join('/fixture/ccs-home', '.factory', 'config.json'),
      legacyConfigDisplayPath: '~/.factory/config.json',
    });
  });

  it('keeps the supplied Windows Factory home without accessing the filesystem', () => {
    const resolved = resolveDroidConfigPaths({
      platform: 'win32',
      env: {},
      homeDir: 'C:/Users/tester',
    });
    expect(resolved.settingsPath).toBe(path.join('C:/Users/tester', '.factory', 'settings.json'));
    expect(resolved.legacyConfigPath).toBe(path.join('C:/Users/tester', '.factory', 'config.json'));
  });
});
