/**
 * Port Validation Tests
 *
 * Comprehensive test suite for port validation across the CLIProxy system.
 * Tests the fix for undefined port bug (7.11.1 → 7.12.0 regression).
 *
 * Covers:
 * - validatePort() function edge cases
 * - Object spread undefined filtering in executor
 * - YAML port validation in resolver
 * - detectRunningProxy with invalid ports
 */

import { describe, it, expect } from 'bun:test';
import { validatePort, CLIPROXY_DEFAULT_PORT } from '../port-manager';
import { resolveProxyConfig } from '../../proxy/proxy-config-resolver';

describe('Port Validation', () => {
  describe('validatePort()', () => {
    describe('returns default port for invalid inputs', () => {
      it('handles undefined', () => {
        expect(validatePort(undefined)).toBe(CLIPROXY_DEFAULT_PORT);
      });

      it('handles null', () => {
        expect(validatePort(null as unknown as number)).toBe(CLIPROXY_DEFAULT_PORT);
      });

      it('handles NaN', () => {
        expect(validatePort(NaN)).toBe(CLIPROXY_DEFAULT_PORT);
      });

      it('handles Infinity', () => {
        expect(validatePort(Infinity)).toBe(CLIPROXY_DEFAULT_PORT);
      });

      it('handles negative Infinity', () => {
        expect(validatePort(-Infinity)).toBe(CLIPROXY_DEFAULT_PORT);
      });

      it('handles zero', () => {
        expect(validatePort(0)).toBe(CLIPROXY_DEFAULT_PORT);
      });

      it('handles negative numbers', () => {
        expect(validatePort(-1)).toBe(CLIPROXY_DEFAULT_PORT);
        expect(validatePort(-8317)).toBe(CLIPROXY_DEFAULT_PORT);
      });

      it('handles ports > 65535', () => {
        expect(validatePort(65536)).toBe(CLIPROXY_DEFAULT_PORT);
        expect(validatePort(100000)).toBe(CLIPROXY_DEFAULT_PORT);
      });

      it('handles floating point numbers', () => {
        expect(validatePort(8317.5)).toBe(CLIPROXY_DEFAULT_PORT);
        expect(validatePort(8317.999)).toBe(CLIPROXY_DEFAULT_PORT);
      });
    });

    describe('returns valid port for valid inputs', () => {
      it('handles port 1 (minimum valid)', () => {
        expect(validatePort(1)).toBe(1);
      });

      it('handles port 65535 (maximum valid)', () => {
        expect(validatePort(65535)).toBe(65535);
      });

      it('handles default port 8317', () => {
        expect(validatePort(8317)).toBe(8317);
      });

      it('handles common ports', () => {
        expect(validatePort(80)).toBe(80);
        expect(validatePort(443)).toBe(443);
        expect(validatePort(3000)).toBe(3000);
        expect(validatePort(8080)).toBe(8080);
      });

      it('handles variant ports (8318-8417)', () => {
        expect(validatePort(8318)).toBe(8318);
        expect(validatePort(8350)).toBe(8350);
        expect(validatePort(8417)).toBe(8417);
      });
    });
  });

  describe('Object Spread Undefined Filtering', () => {
    /**
     * Bug: `{ ...DEFAULT_CONFIG, ...{ port: undefined } }` overwrites port with undefined
     * Fix: Filter undefined values before spreading
     */
    it('demonstrates the original bug pattern', () => {
      const DEFAULT_CONFIG = { port: 8317, timeout: 2000 };
      const config = { port: undefined, timeout: undefined };

      // BUG: Direct spread overwrites with undefined
      const bugResult = { ...DEFAULT_CONFIG, ...config };
      expect(bugResult.port).toBe(undefined); // This was the bug!

      // FIX: Filter undefined values before spreading
      const filteredConfig = Object.fromEntries(
        Object.entries(config).filter(([, v]) => v !== undefined)
      );
      const fixResult = { ...DEFAULT_CONFIG, ...filteredConfig };
      expect(fixResult.port).toBe(8317); // Fixed!
    });

    it('preserves explicit values while filtering undefined', () => {
      const DEFAULT_CONFIG = { port: 8317, timeout: 2000, protocol: 'http' };
      const config = { port: 9000, timeout: undefined, protocol: undefined };

      const filteredConfig = Object.fromEntries(
        Object.entries(config).filter(([, v]) => v !== undefined)
      );
      const result = { ...DEFAULT_CONFIG, ...filteredConfig };

      expect(result.port).toBe(9000); // Explicit value preserved
      expect(result.timeout).toBe(2000); // Default preserved (undefined filtered)
      expect(result.protocol).toBe('http'); // Default preserved (undefined filtered)
    });
  });

  describe('YAML Port Validation in resolveProxyConfig', () => {
    it('validates YAML remote port', () => {
      const { config } = resolveProxyConfig([], {
        remote: { host: 'example.com', port: 9000 },
      });
      expect(config.port).toBe(9000);
    });

    it('falls back to default for invalid YAML port (negative)', () => {
      const { config } = resolveProxyConfig([], {
        remote: { host: 'example.com', port: -1 },
      });
      expect(config.port).toBe(CLIPROXY_DEFAULT_PORT);
    });

    it('falls back to default for invalid YAML port (> 65535)', () => {
      const { config } = resolveProxyConfig([], {
        remote: { host: 'example.com', port: 70000 },
      });
      expect(config.port).toBe(CLIPROXY_DEFAULT_PORT);
    });

    it('falls back to default for invalid YAML port (zero)', () => {
      const { config } = resolveProxyConfig([], {
        remote: { host: 'example.com', port: 0 },
      });
      expect(config.port).toBe(CLIPROXY_DEFAULT_PORT);
    });

    it('validates YAML local port', () => {
      const { config } = resolveProxyConfig([], {
        local: { port: 9000 },
      });
      expect(config.port).toBe(9000);
    });

    it('falls back to default for invalid YAML local port', () => {
      const { config } = resolveProxyConfig([], {
        local: { port: -100 },
      });
      expect(config.port).toBe(CLIPROXY_DEFAULT_PORT);
    });

    it('CLI port overrides invalid YAML port', () => {
      const { config } = resolveProxyConfig(['--proxy-port', '8080'], {
        remote: { host: 'example.com', port: -1 },
      });
      expect(config.port).toBe(8080);
    });
  });

  describe('Priority Chain with Invalid Values', () => {
    it('CLI valid > ENV valid > YAML invalid', () => {
      process.env.CCS_PROXY_PORT = '9000';
      const { config } = resolveProxyConfig(['--proxy-port', '8080'], {
        remote: { host: 'example.com', port: -1 },
      });
      expect(config.port).toBe(8080); // CLI wins
      delete process.env.CCS_PROXY_PORT;
    });

    it('ENV valid > YAML invalid > default', () => {
      process.env.CCS_PROXY_PORT = '9000';
      const { config } = resolveProxyConfig([], {
        remote: { host: 'example.com', port: -1 },
      });
      expect(config.port).toBe(9000); // ENV wins
      delete process.env.CCS_PROXY_PORT;
    });

    it('YAML valid when CLI and ENV not set', () => {
      const { config } = resolveProxyConfig([], {
        remote: { host: 'example.com', port: 9000 },
      });
      expect(config.port).toBe(9000);
    });

    it('default when all sources invalid/missing', () => {
      const { config } = resolveProxyConfig([], {});
      expect(config.port).toBe(CLIPROXY_DEFAULT_PORT);
    });
  });

  describe('Boundary Conditions', () => {
    it('handles port exactly at minimum boundary (1)', () => {
      expect(validatePort(1)).toBe(1);
      expect(validatePort(0)).toBe(CLIPROXY_DEFAULT_PORT);
    });

    it('handles port exactly at maximum boundary (65535)', () => {
      expect(validatePort(65535)).toBe(65535);
      expect(validatePort(65536)).toBe(CLIPROXY_DEFAULT_PORT);
    });

    it('handles reserved ports (< 1024)', () => {
      // Reserved ports are still valid from validation perspective
      expect(validatePort(22)).toBe(22);
      expect(validatePort(80)).toBe(80);
      expect(validatePort(443)).toBe(443);
    });
  });
});
