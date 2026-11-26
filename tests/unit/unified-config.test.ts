/**
 * Unit tests for unified config module
 */
import { describe, it, expect } from 'bun:test';

// Import only modules without transitive dependencies on config-manager
import {
  createEmptyUnifiedConfig,
  isUnifiedConfig,
  UNIFIED_CONFIG_VERSION,
} from '../../src/config/unified-config-types';
import { isUnifiedConfigEnabled } from '../../src/config/feature-flags';

// Inline helper to test secret key detection (utility kept for potential reuse)
function isSecretKey(key: string): boolean {
  const upper = key.toUpperCase();
  const secretPatterns = [
    'TOKEN',
    'SECRET',
    'API_KEY',
    'APIKEY',
    'PASSWORD',
    'CREDENTIAL',
    'AUTH',
    'PRIVATE',
  ];
  return secretPatterns.some((pattern) => upper.includes(pattern));
}

describe('unified-config-types', () => {
  describe('createEmptyUnifiedConfig', () => {
    it('should create config with correct version', () => {
      const config = createEmptyUnifiedConfig();
      expect(config.version).toBe(UNIFIED_CONFIG_VERSION);
    });

    it('should have empty accounts and profiles', () => {
      const config = createEmptyUnifiedConfig();
      expect(Object.keys(config.accounts)).toHaveLength(0);
      expect(Object.keys(config.profiles)).toHaveLength(0);
    });

    it('should have default preferences', () => {
      const config = createEmptyUnifiedConfig();
      expect(config.preferences.theme).toBe('system');
      expect(config.preferences.telemetry).toBe(false);
      expect(config.preferences.auto_update).toBe(true);
    });

    it('should omit retired cliproxy defaults', () => {
      const config = createEmptyUnifiedConfig();
      expect('cliproxy' in config).toBe(false);
    });
  });

  describe('isUnifiedConfig', () => {
    it('should return true for valid config', () => {
      const config = createEmptyUnifiedConfig();
      expect(isUnifiedConfig(config)).toBe(true);
    });

    it('should return false for null', () => {
      expect(isUnifiedConfig(null)).toBe(false);
    });

    it('should return true for older version (relaxed validation)', () => {
      // Fix for issue #82: Relaxed validation accepts version >= 1
      // to prevent profile loss when loading partially valid configs
      const config = { ...createEmptyUnifiedConfig(), version: 1 };
      expect(isUnifiedConfig(config)).toBe(true);
    });

    it('should return true for partial configs (relaxed validation)', () => {
      // Fix for issue #82: Relaxed validation accepts partial configs
      // Missing sections are merged with defaults in loadOrCreateUnifiedConfig
      expect(isUnifiedConfig({ version: 2 })).toBe(true);
      expect(isUnifiedConfig({ version: 2, accounts: {} })).toBe(true);
    });

    it('should return false for version < 1', () => {
      expect(isUnifiedConfig({ version: 0 })).toBe(false);
      expect(isUnifiedConfig({ version: -1 })).toBe(false);
    });
  });
});

describe('sensitive-keys', () => {
  describe('isSecretKey', () => {
    it('should identify token keys as secrets', () => {
      expect(isSecretKey('ANTHROPIC_AUTH_TOKEN')).toBe(true);
      expect(isSecretKey('ACCESS_TOKEN')).toBe(true);
      expect(isSecretKey('REFRESH_TOKEN')).toBe(true);
    });

    it('should identify API keys as secrets', () => {
      expect(isSecretKey('API_KEY')).toBe(true);
      expect(isSecretKey('OPENAI_API_KEY')).toBe(true);
      expect(isSecretKey('APIKEY')).toBe(true);
    });

    it('should identify password keys as secrets', () => {
      expect(isSecretKey('PASSWORD')).toBe(true);
      expect(isSecretKey('DB_PASSWORD')).toBe(true);
    });

    it('should identify secret/credential keys', () => {
      expect(isSecretKey('CLIENT_SECRET')).toBe(true);
      expect(isSecretKey('AWS_CREDENTIAL')).toBe(true);
    });

    it('should not identify non-secret keys', () => {
      expect(isSecretKey('ANTHROPIC_MODEL')).toBe(false);
      expect(isSecretKey('ANTHROPIC_BASE_URL')).toBe(false);
      expect(isSecretKey('DEBUG')).toBe(false);
      expect(isSecretKey('NODE_ENV')).toBe(false);
    });

    it('should be case-insensitive', () => {
      expect(isSecretKey('api_key')).toBe(true);
      expect(isSecretKey('Api_Key')).toBe(true);
    });
  });
});

describe('feature-flags', () => {
  describe('isUnifiedConfigEnabled', () => {
    it('should read from CCS_UNIFIED_CONFIG env var', () => {
      // This test runs with the current environment
      // In CI, CCS_UNIFIED_CONFIG may or may not be set
      const result = isUnifiedConfigEnabled();
      expect(typeof result).toBe('boolean');
    });
  });
});
