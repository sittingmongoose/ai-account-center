/**
 * Tests: config schemas re-export backward compatibility.
 *
 * Verifies that active AAC types, constants, and functions exported from
 * unified-config-types.ts stay accessible via both barrels.
 */

import { describe, it, expect } from 'bun:test';

import * as barrel from '../../unified-config-types';
import * as schemas from '../index';

import type {
  UnifiedConfig,
  AccountConfig,
  ProfileConfig,
  DashboardAuthConfig,
  AutoQuotaConfig,
  RuntimeMonitorConfig,
  ManualQuotaConfig,
  QuotaManagementMode,
  QuotaManagementConfig,
  LoggingLevel,
  LoggingConfig,
  PreferencesConfig,
} from '../../unified-config-types';

describe('config schemas backward compatibility', () => {
  it('re-exports active config types', () => {
    const prefs: PreferencesConfig = { theme: 'system', telemetry: false, auto_update: true };
    const level: LoggingLevel = 'info';
    const mode: QuotaManagementMode = 'auto';
    const typeChecks: [
      UnifiedConfig | null,
      AccountConfig | null,
      ProfileConfig | null,
      DashboardAuthConfig | null,
      AutoQuotaConfig | null,
      RuntimeMonitorConfig | null,
      ManualQuotaConfig | null,
      QuotaManagementConfig | null,
      LoggingConfig | null,
    ] = [null, null, null, null, null, null, null, null, null];
    expect({ prefs, level, mode, count: typeChecks.length }).toEqual({
      prefs: { theme: 'system', telemetry: false, auto_update: true },
      level: 'info',
      mode: 'auto',
      count: 9,
    });
  });

  it('re-exports UNIFIED_CONFIG_VERSION', () => {
    expect(barrel.UNIFIED_CONFIG_VERSION).toBe(14);
    expect(schemas.UNIFIED_CONFIG_VERSION).toBe(14);
  });

  const defaultConstants = [
    'DEFAULT_LOGGING_CONFIG',
    'DEFAULT_DASHBOARD_AUTH_CONFIG',
    'DEFAULT_AUTO_QUOTA_CONFIG',
    'DEFAULT_MANUAL_QUOTA_CONFIG',
    'DEFAULT_RUNTIME_MONITOR_CONFIG',
    'DEFAULT_QUOTA_MANAGEMENT_CONFIG',
  ] as const;

  for (const name of defaultConstants) {
    it(`re-exports ${name}`, () => {
      expect(barrel[name]).toBeDefined();
      expect(barrel[name]).toEqual(schemas[name]);
    });
  }

  it('re-exports createEmptyUnifiedConfig without retired CLIProxy sections', () => {
    expect(typeof barrel.createEmptyUnifiedConfig).toBe('function');
    expect(typeof schemas.createEmptyUnifiedConfig).toBe('function');

    const config = barrel.createEmptyUnifiedConfig();
    expect(config.version).toBe(14);
    expect(config.accounts).toEqual({});
    expect(config.profiles).toEqual({});
    expect(Object.hasOwn(config, 'cliproxy')).toBe(false);
    expect(Object.hasOwn(config, 'cliproxy_server')).toBe(false);
    expect(Object.hasOwn(config, 'proxy')).toBe(false);
    expect(config.logging).toBeDefined();
    expect(config.preferences).toBeDefined();
    expect(config.quota_management).toBeDefined();
    expect(config.dashboard_auth).toBeDefined();
  });

  it('re-exports isUnifiedConfig', () => {
    expect(typeof barrel.isUnifiedConfig).toBe('function');
    expect(typeof schemas.isUnifiedConfig).toBe('function');

    expect(barrel.isUnifiedConfig({ version: 13 })).toBe(true);
    expect(barrel.isUnifiedConfig(null)).toBe(false);
    expect(barrel.isUnifiedConfig({})).toBe(false);
    expect(barrel.isUnifiedConfig({ version: 0 })).toBe(false);
    expect(barrel.isUnifiedConfig({ version: 1 })).toBe(true);
    expect(barrel.isUnifiedConfig('not an object')).toBe(false);
  });

  const expectedRuntimeExports = [
    'UNIFIED_CONFIG_VERSION',
    'createEmptyUnifiedConfig',
    'isUnifiedConfig',
    'DEFAULT_LOGGING_CONFIG',
    'DEFAULT_DASHBOARD_AUTH_CONFIG',
    'DEFAULT_AUTO_QUOTA_CONFIG',
    'DEFAULT_MANUAL_QUOTA_CONFIG',
    'DEFAULT_RUNTIME_MONITOR_CONFIG',
    'DEFAULT_QUOTA_MANAGEMENT_CONFIG',
  ] as const;

  for (const name of expectedRuntimeExports) {
    it(`barrel exports "${name}"`, () => {
      expect(name in barrel).toBe(true);
    });
  }
});
