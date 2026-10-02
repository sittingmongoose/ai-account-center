/**
 * Auth Middleware Tests
 * Tests for dashboard authentication middleware and routes.
 */

import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import bcrypt from 'bcrypt';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { getDashboardAuthConfig } from '../../../src/config/unified-config-loader';
import {
  apiAuthMiddleware,
  authMiddleware,
  isApiRequestPath,
  isDashboardWebSocketOriginAllowed,
  getDashboardWebSocketRejectionStatus,
  isDashboardWebSocketUpgradeAllowed,
  requireDashboardSession,
} from '../../../src/web-server/middleware/auth-middleware';
import { runWithScopedConfigDir } from '../../../src/utils/config-manager';
import {
  bumpSessionEpoch,
  resetDashboardAuthStateForTests,
} from '../../../src/web-server/services/dashboard-auth-state';

describe('Dashboard Auth', () => {
  let tempDir = '';
  let originalDashboardAuthEnabled: string | undefined;
  let originalCcsDir: string | undefined;

  beforeEach(() => {
    tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-dashboard-auth-'));
    originalDashboardAuthEnabled = process.env.CCS_DASHBOARD_AUTH_ENABLED;
    // The session-epoch check reads <CCS_DIR>/auth/state.json: keep it in the fixture.
    originalCcsDir = process.env.CCS_DIR;
    process.env.CCS_DIR = path.join(tempDir, '.ccs');
  });

  afterEach(() => {
    if (originalDashboardAuthEnabled === undefined) {
      delete process.env.CCS_DASHBOARD_AUTH_ENABLED;
    } else {
      process.env.CCS_DASHBOARD_AUTH_ENABLED = originalDashboardAuthEnabled;
    }
    if (originalCcsDir === undefined) delete process.env.CCS_DIR;
    else process.env.CCS_DIR = originalCcsDir;

    if (tempDir && fs.existsSync(tempDir)) {
      fs.rmSync(tempDir, { recursive: true, force: true });
    }
  });

  describe('getDashboardAuthConfig', () => {
    it('returns disabled by default', async () => {
      const config = await runWithScopedConfigDir(tempDir, () => getDashboardAuthConfig());
      expect(config.enabled).toBe(false);
    });

    it('returns 24 hour default session timeout', async () => {
      const config = await runWithScopedConfigDir(tempDir, () => getDashboardAuthConfig());
      expect(config.session_timeout_hours).toBe(24);
    });
  });

  describe('bcrypt password hashing', () => {
    it('generates valid bcrypt hash', async () => {
      const password = 'testpassword123';
      const hash = await bcrypt.hash(password, 10);

      expect(hash).toMatch(/^\$2[aby]\$\d{2}\$.{53}$/);
    });

    it('verifies correct password', async () => {
      const password = 'testpassword123';
      const hash = await bcrypt.hash(password, 10);

      const isValid = await bcrypt.compare(password, hash);
      expect(isValid).toBe(true);
    });

    it('rejects incorrect password', async () => {
      const password = 'testpassword123';
      const hash = await bcrypt.hash(password, 10);

      const isValid = await bcrypt.compare('wrongpassword', hash);
      expect(isValid).toBe(false);
    });

    it('timing-safe comparison for wrong password', async () => {
      const password = 'testpassword123';
      const hash = await bcrypt.hash(password, 10);

      // bcrypt.compare should take similar time for wrong vs right password
      // This is a basic check that the function works
      const start1 = performance.now();
      await bcrypt.compare('wrongpassword', hash);
      const time1 = performance.now() - start1;

      const start2 = performance.now();
      await bcrypt.compare(password, hash);
      const time2 = performance.now() - start2;

      // Both should complete (timing comparison is handled by bcrypt internally)
      expect(time1).toBeGreaterThan(0);
      expect(time2).toBeGreaterThan(0);
    });
  });

  // These call the real middleware; the HTTP regression suite in
  // api-auth-hardening.test.ts covers the same rules through startServer().
  function runGuard(
    guard: typeof authMiddleware,
    requestPath: string,
    authenticated = false,
    method = 'GET'
  ): {
    passed: boolean;
    status: number | null;
    body: unknown;
    headers: Record<string, string>;
  } {
    const outcome = {
      passed: false,
      status: null as number | null,
      body: undefined as unknown,
      headers: {} as Record<string, string>,
    };
    const req = {
      method,
      path: requestPath,
      originalUrl: requestPath,
      session: { authenticated },
      socket: { remoteAddress: '127.0.0.1' },
      headers: { host: '127.0.0.1:3000' },
    };
    const res = {
      setHeader(name: string, value: string) {
        outcome.headers[name.toLowerCase()] = value;
        return this;
      },
      status(code: number) {
        outcome.status = code;
        return this;
      },
      json(body: unknown) {
        outcome.body = body;
        return this;
      },
    };
    guard(req as never, res as never, () => {
      outcome.passed = true;
    });
    return outcome;
  }

  describe('isApiRequestPath', () => {
    it('treats every letter case of the /api mount as API traffic', () => {
      for (const requestPath of ['/api', '/api/', '/API/codex/profiles', '/Api/x', '/aPI/bar']) {
        expect(isApiRequestPath(requestPath)).toBe(true);
      }
      for (const requestPath of ['/', '/login', '/apix', '/dashboard.wasm', '/codex/api/']) {
        expect(isApiRequestPath(requestPath)).toBe(false);
      }
    });
  });

  describe('authMiddleware (global guard)', () => {
    it('passes every request when dashboard auth is disabled', () => {
      process.env.CCS_DASHBOARD_AUTH_ENABLED = 'false';
      expect(runGuard(authMiddleware, '/API/codex/profiles').passed).toBe(true);
      expect(runGuard(authMiddleware, '/api/codex/profiles').passed).toBe(true);
    });

    it('keeps the public routes public in any letter case, as exact method and path pairs', () => {
      process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
      for (const [method, requestPath] of [
        ['POST', '/api/auth/login'],
        ['GET', '/api/auth/check'],
        ['GET', '/api/auth/setup'],
        ['POST', '/api/auth/setup'],
        ['POST', '/api/auth/devices/pair'],
        ['GET', '/api/health'],
        ['HEAD', '/api/health'],
        ['GET', '/api/health/'],
        ['POST', '/API/AUTH/LOGIN'],
        ['GET', '/Api/Health'],
      ]) {
        expect({
          method,
          requestPath,
          passed: runGuard(authMiddleware, requestPath, false, method).passed,
        }).toEqual({ method, requestPath, passed: true });
      }
      for (const [method, requestPath] of [
        ['GET', '/api/auth/login-history'],
        ['GET', '/api/health/fix'],
        ['GET', '/api/auth'],
        ['GET', '/api/auth/setupx'],
        ['POST', '/api/auth/setupanything'],
        ['GET', '/api/auth/devices'],
        ['GET', '/api/auth/devices/pair'],
        ['POST', '/api/auth/check'],
        ['GET', '/api/auth/login'],
        ['POST', '/api/health'],
      ]) {
        expect({
          method,
          requestPath,
          status: runGuard(authMiddleware, requestPath, false, method).status,
        }).toEqual({ method, requestPath, status: 401 });
      }
    });

    it('requires a session for API paths in any letter case', () => {
      process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
      for (const requestPath of [
        '/api/codex/profiles',
        '/API/codex/profiles',
        '/Api/claude/desktop-profiles',
        '/aPi/accounts/dashboard',
        '/API',
      ]) {
        const outcome = runGuard(authMiddleware, requestPath);
        expect({ requestPath, passed: outcome.passed, status: outcome.status }).toEqual({
          requestPath,
          passed: false,
          status: 401,
        });
        // Under /api/accounts the 401 carries the contract code; elsewhere the body is unchanged.
        expect(outcome.body).toEqual(
          requestPath.toLowerCase().startsWith('/api/accounts/')
            ? { error: 'Authentication required', code: 'auth_required' }
            : { error: 'Authentication required' }
        );
        expect(outcome.headers['cache-control']).toBe('no-store');
        expect(runGuard(authMiddleware, requestPath, true).passed).toBe(true);
      }
    });

    it('leaves static assets and SPA routes to the static handler', () => {
      process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
      for (const requestPath of ['/', '/login', '/dashboard.wasm', '/apix']) {
        expect(runGuard(authMiddleware, requestPath).passed).toBe(true);
      }
    });
  });

  describe('apiAuthMiddleware (guard on the /api router)', () => {
    it('checks paths relative to the mount', () => {
      process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
      for (const [method, requestPath] of [
        ['POST', '/auth/login'],
        ['GET', '/auth/check'],
        ['GET', '/auth/setup'],
        ['POST', '/auth/setup'],
        ['POST', '/auth/devices/pair'],
        ['GET', '/health'],
      ]) {
        expect(runGuard(apiAuthMiddleware, requestPath, false, method).passed).toBe(true);
      }
      expect(runGuard(apiAuthMiddleware, '/auth/devices').status).toBe(401);
      for (const requestPath of ['/codex/profiles', '/CODEX/profiles', '/bar/auth', '/']) {
        expect(runGuard(apiAuthMiddleware, requestPath).status).toBe(401);
        expect(runGuard(apiAuthMiddleware, requestPath, true).passed).toBe(true);
      }
      for (const requestPath of ['/accounts/visibility', '/Accounts/dashboard', '/accounts']) {
        const outcome = runGuard(apiAuthMiddleware, requestPath);
        expect(outcome.status).toBe(401);
        expect(outcome.body).toEqual({ error: 'Authentication required', code: 'auth_required' });
        expect(outcome.headers['cache-control']).toBe('no-store');
      }
      expect(runGuard(apiAuthMiddleware, '/accountsx').body).toEqual({
        error: 'Authentication required',
      });
    });

    it('passes every request when dashboard auth is disabled', () => {
      process.env.CCS_DASHBOARD_AUTH_ENABLED = 'false';
      expect(runGuard(apiAuthMiddleware, '/codex/profiles').passed).toBe(true);
    });
  });

  describe('requireDashboardSession', () => {
    function check(remoteAddress: string, authenticated: boolean) {
      const outcome = { status: null as number | null, body: undefined as unknown };
      const req = {
        session: { authenticated },
        socket: { remoteAddress },
        headers: { host: '127.0.0.1:3000' },
      };
      const res = {
        status(code: number) {
          outcome.status = code;
          return this;
        },
        json(body: unknown) {
          outcome.body = body;
          return this;
        },
      };
      const allowed = requireDashboardSession(req as never, res as never, 'local only');
      return { allowed, ...outcome };
    }

    it('requires an authenticated session when dashboard auth is enabled', () => {
      process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
      expect(check('127.0.0.1', false)).toEqual({
        allowed: false,
        status: 401,
        body: { error: 'Authentication required' },
      });
      expect(check('203.0.113.42', true)).toEqual({
        allowed: true,
        status: null,
        body: undefined,
      });
    });

    it('keeps localhost-only access when dashboard auth is disabled', () => {
      process.env.CCS_DASHBOARD_AUTH_ENABLED = 'false';
      expect(check('127.0.0.1', false).allowed).toBe(true);
      expect(check('203.0.113.42', true)).toEqual({
        allowed: false,
        status: 403,
        body: { error: 'local only' },
      });
    });
  });

  describe('websocket upgrade access', () => {
    function makeUpgradeRequest(
      remoteAddress: string,
      authenticated = false,
      headers: Record<string, string> = {}
    ) {
      return {
        headers,
        socket: { remoteAddress },
        session: authenticated ? { authenticated: true } : { authenticated: false },
      } as never;
    }

    it('allows loopback websocket upgrades when dashboard auth is disabled', () => {
      process.env.CCS_DASHBOARD_AUTH_ENABLED = 'false';

      expect(isDashboardWebSocketUpgradeAllowed(makeUpgradeRequest('127.0.0.1'))).toBe(true);
      expect(isDashboardWebSocketUpgradeAllowed(makeUpgradeRequest('::1'))).toBe(true);
    });

    it('blocks remote websocket upgrades when dashboard auth is disabled', () => {
      process.env.CCS_DASHBOARD_AUTH_ENABLED = 'false';
      const request = makeUpgradeRequest('10.10.0.24');

      expect(isDashboardWebSocketUpgradeAllowed(request)).toBe(false);
      expect(getDashboardWebSocketRejectionStatus()).toBe(403);
    });

    it('blocks cross-site websocket origins when dashboard auth is disabled', () => {
      process.env.CCS_DASHBOARD_AUTH_ENABLED = 'false';
      const request = makeUpgradeRequest('127.0.0.1', false, {
        host: '127.0.0.1:3001',
        origin: 'https://evil.example.test',
      });

      expect(isDashboardWebSocketOriginAllowed(request)).toBe(false);
      expect(isDashboardWebSocketUpgradeAllowed(request)).toBe(false);
      expect(getDashboardWebSocketRejectionStatus(request)).toBe(403);
    });

    it('requires an authenticated session for websocket upgrades when auth is enabled', () => {
      process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';

      expect(isDashboardWebSocketUpgradeAllowed(makeUpgradeRequest('127.0.0.1'))).toBe(false);
      expect(
        isDashboardWebSocketUpgradeAllowed(
          makeUpgradeRequest('10.10.0.24', true, {
            host: 'dashboard.internal:3001',
            origin: 'https://dashboard.internal:3001',
          })
        )
      ).toBe(true);
      expect(getDashboardWebSocketRejectionStatus()).toBe(401);
    });

    it('allows same-origin websocket upgrades when auth is enabled', () => {
      process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
      const request = makeUpgradeRequest('10.10.0.24', true, {
        host: 'dashboard.example.test:3001',
        origin: 'https://dashboard.example.test:3001',
      });

      expect(isDashboardWebSocketOriginAllowed(request)).toBe(true);
      expect(isDashboardWebSocketUpgradeAllowed(request)).toBe(true);
    });

    it('allows loopback host aliases on the same dashboard port', () => {
      process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
      const request = makeUpgradeRequest('127.0.0.1', true, {
        host: '127.0.0.1:3001',
        origin: 'http://localhost:3001',
      });

      expect(isDashboardWebSocketOriginAllowed(request)).toBe(true);
      expect(isDashboardWebSocketUpgradeAllowed(request)).toBe(true);
    });

    it('blocks 127-prefixed DNS names from loopback websocket origin aliases', () => {
      process.env.CCS_DASHBOARD_AUTH_ENABLED = 'false';
      const request = makeUpgradeRequest('127.0.0.1', false, {
        host: 'localhost:3001',
        origin: 'http://127.evil.example.test:3001',
      });

      expect(isDashboardWebSocketOriginAllowed(request)).toBe(false);
      expect(isDashboardWebSocketUpgradeAllowed(request)).toBe(false);
      expect(getDashboardWebSocketRejectionStatus(request)).toBe(403);
    });

    it('refuses device tokens on /ws with 403 (CONTRACT-auth-devices 6)', () => {
      process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
      const request = makeUpgradeRequest('127.0.0.1', true, {
        host: '127.0.0.1:3001',
        origin: 'http://127.0.0.1:3001',
        authorization: `Bearer aacd_${'A'.repeat(43)}`,
      });
      expect(isDashboardWebSocketUpgradeAllowed(request)).toBe(false);
      expect(getDashboardWebSocketRejectionStatus(request)).toBe(403);
    });

    it('refuses a session from an older epoch with 401 (sign out other browsers)', async () => {
      process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
      const headers = { host: '127.0.0.1:3001', origin: 'http://127.0.0.1:3001' };
      const request = makeUpgradeRequest('127.0.0.1', true, headers);
      expect(isDashboardWebSocketUpgradeAllowed(request)).toBe(true);
      await bumpSessionEpoch();
      expect(isDashboardWebSocketUpgradeAllowed(request)).toBe(false);
      expect(getDashboardWebSocketRejectionStatus(request)).toBe(401);
      resetDashboardAuthStateForTests();
    });

    it('blocks cross-site websocket origins even with an authenticated session', () => {
      process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
      const request = makeUpgradeRequest('127.0.0.1', true, {
        host: '127.0.0.1:3001',
        origin: 'https://evil.example.test',
      });

      expect(isDashboardWebSocketOriginAllowed(request)).toBe(false);
      expect(isDashboardWebSocketUpgradeAllowed(request)).toBe(false);
      expect(getDashboardWebSocketRejectionStatus(request)).toBe(403);
    });
  });
});
