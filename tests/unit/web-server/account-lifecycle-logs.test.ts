/**
 * Nothing secret reaches a log (CONTRACT-registry-lifecycle 11.6 and 11.11):
 * API keys, fingerprints, device codes, verification URL queries, supervised
 * codes, confirmation tokens and CLI output stay out of the structured log and
 * the console, through the real request log, audit lines and job runner.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import express from 'express';
import fs from 'fs';
import http, { type Server } from 'http';
import type { AddressInfo } from 'net';
import os from 'os';
import path from 'path';
import { createEmptyUnifiedConfig } from '../../../src/config/unified-config-types';
import { saveUnifiedConfig } from '../../../src/config/unified-config-loader';
import { CodexProfileRegistry } from '../../../src/codex-auth/codex-profile-registry';
import { invalidateCodexAuthProfilesCache } from '../../../src/codex-auth/codex-auth-dashboard-service';
import {
  clearRecentLogEntries,
  getRecentLogEntries,
} from '../../../src/services/logging/log-buffer';
import { invalidateLoggingConfigCache } from '../../../src/services/logging/log-config';
import { requestLoggingMiddleware } from '../../../src/web-server/middleware/request-logging-middleware';
import { createAccountLifecycleRouter } from '../../../src/web-server/routes/account-lifecycle-routes';
import { AccountConfirmationStore } from '../../../src/web-server/services/account-confirmations';
import type { DashboardAccount } from '../../../src/web-server/services/account-dashboard-types';
import { keyStoreFor } from '../../../src/web-server/services/account-key-store';
import type { LifecycleEnv } from '../../../src/web-server/services/account-lifecycle-env';
import {
  auditLifecycle,
  createSignInJobRunner,
  lifecycleProviderFacts,
} from '../../../src/web-server/services/account-lifecycle-runtime';
import { ClaudeAccountLifecycle } from '../../../src/web-server/services/claude-account-lifecycle';
import { CodexAccountLifecycle } from '../../../src/web-server/services/codex-account-lifecycle';
import type { SignInProcessHandle } from '../../../src/web-server/services/signin-process';

const KEY = 'zai-FIRST-secret-0123456789';
const NEW_KEY = 'zai-SECOND-secret-9876543210';
const REJECTED_KEY = 'zai-REJECTED-secret-555555';
const THROWN_KEY = 'zai-THROWN-secret-777777';
const SECRETS = [
  KEY,
  NEW_KEY,
  REJECTED_KEY,
  THROWN_KEY,
  'sha256:',
  'PRIVATE-STATE',
  'WXYZ-98765',
  'CLI-PRIVATE-LINE',
  '4/0PRIVATE-CODE',
  'access-new',
  'refresh-new',
];

const ORIGINAL_CCS_HOME = process.env.CCS_HOME;
let root: string;
let server: Server | null = null;
const consoleText: string[] = [];
const originalConsole = { ...console };

beforeEach(() => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-lifecycle-logs-'));
  process.env.CCS_HOME = root;
  clearRecentLogEntries();
  invalidateLoggingConfigCache();
  const config = createEmptyUnifiedConfig();
  config.logging = { ...config.logging, enabled: true, level: 'debug', redact: false };
  saveUnifiedConfig(config);
  invalidateLoggingConfigCache();
  invalidateCodexAuthProfilesCache();
  for (const name of ['log', 'info', 'warn', 'error', 'debug'] as const) {
    console[name] = (...args: unknown[]) => {
      consoleText.push(
        args.map((arg) => (typeof arg === 'string' ? arg : JSON.stringify(arg))).join(' ')
      );
    };
  }
});

afterEach(async () => {
  Object.assign(console, originalConsole);
  consoleText.splice(0);
  if (server) await new Promise<void>((resolve) => server?.close(() => resolve()));
  server = null;
  if (ORIGINAL_CCS_HOME === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = ORIGINAL_CCS_HOME;
  clearRecentLogEntries();
  invalidateLoggingConfigCache();
  invalidateCodexAuthProfilesCache();
  fs.rmSync(root, { recursive: true, force: true });
});

function idToken(email: string): string {
  const part = (value: unknown) => Buffer.from(JSON.stringify(value)).toString('base64url');
  return [
    part({ alg: 'none' }),
    part({ email, 'https://api.openai.com/auth': { chatgpt_account_id: 'acct-new' } }),
    'sig',
  ].join('.');
}

function row(id: string, status: DashboardAccount['status']): DashboardAccount {
  return {
    id,
    provider: 'zai',
    providerLabel: 'Z.ai',
    label: 'Z.ai',
    email: null,
    plan: null,
    platform: 'ubuntu',
    source: 'fixture',
    status,
    message: null,
    fetchedAt: null,
    sampledAt: null,
    isActive: false,
    windows: [],
    capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
  };
}

describe('account lifecycle logs', () => {
  it('never log a key, code, URL query, token or CLI output', async () => {
    const ccsDir = path.join(root, '.ccs');
    const codexHome = path.join(root, 'native-codex');
    fs.mkdirSync(codexHome, { recursive: true });
    new CodexProfileRegistry().createProfile('gmail', {
      created: new Date().toISOString(),
      last_used: null,
      email: 'gmail@example.com',
    });
    let probe: 'ok' | 'needs_sign_in' | 'throw' = 'ok';
    const runner = createSignInJobRunner({
      spawn: (command) => {
        let onData: (chunk: string) => void = () => undefined;
        let onExit: (code: number | null, failure: null) => void = () => undefined;
        const handle: SignInProcessHandle = {
          onData: (listener) => {
            onData = listener;
          },
          onExit: (listener) => {
            onExit = listener;
          },
          write: () => false,
          kill: () => undefined,
        };
        setTimeout(() => {
          onData(
            'Open https://auth.openai.com/codex/device?state=PRIVATE-STATE\n  WXYZ-98765\nCLI-PRIVATE-LINE\n'
          );
          fs.writeFileSync(
            path.join(command.env.CODEX_HOME, 'auth.json'),
            JSON.stringify({
              tokens: {
                id_token: idToken('new@example.com'),
                access_token: 'access-new',
                refresh_token: 'refresh-new',
              },
            }),
            { mode: 0o600 }
          );
          onExit(0, null);
        }, 5);
        return handle;
      },
    });
    const facts = (context: { secureTransport?: boolean }) =>
      lifecycleProviderFacts(context, {
        codexCliAvailable: () => true,
        claudeEnabled: () => false,
      });
    const confirmations = new AccountConfirmationStore();
    const env: LifecycleEnv = {
      ccsDir: () => ccsDir,
      runner: () => runner,
      codex: () => new CodexAccountLifecycle({ codexHome, codexCli: () => '/fake/codex', env: {} }),
      claude: () =>
        new ClaudeAccountLifecycle({
          ccsDir: () => ccsDir,
          transport: {} as never,
          enabled: false,
        }),
      confirmations: () => confirmations,
      providerFacts: facts,
      getDashboard: async () => {
        throw new Error('not used');
      },
      probe: async (source) => {
        if (probe === 'throw') throw new Error(`provider said ${THROWN_KEY} is wrong`);
        return row(source.account?.id ?? 'zai:usage', probe);
      },
      refreshAdditional: async () => null,
      replaceRow: () => undefined,
      keyStore: (location) => keyStoreFor(location, { ccsDir }),
      openCursor: async () => undefined,
      onChanged: () => undefined,
      audit: auditLifecycle,
      now: Date.now,
    };
    const app = express();
    app.use(requestLoggingMiddleware);
    app.use(express.json());
    app.use((req, _res, next) => {
      Object.assign(req, { session: { authenticated: true }, sessionID: 'log-test' });
      next();
    });
    app.use('/api/accounts', createAccountLifecycleRouter({ env: () => env }));
    server = http.createServer(app);
    await new Promise<void>((resolve) => server?.listen(0, '127.0.0.1', () => resolve()));
    const base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
    const call = async (method: string, route: string, body: unknown) => {
      const response = await fetch(`${base}/api/accounts${route}`, {
        method,
        headers: { origin: base, 'content-type': 'application/json' },
        body: JSON.stringify(body),
      });
      return { status: response.status, body: (await response.json()) as Record<string, unknown> };
    };

    const added = await call('POST', '/add', { provider: 'zai', key: KEY });
    expect(added.status).toBe(201);
    const id = String((added.body.account as { id: string }).id);
    expect((await call('PUT', `/${id}/key`, { key: NEW_KEY })).status).toBe(200);
    probe = 'needs_sign_in';
    expect((await call('PUT', `/${id}/key`, { key: REJECTED_KEY })).status).toBe(422);
    probe = 'throw';
    expect((await call('PUT', `/${id}/key`, { key: THROWN_KEY })).status).toBe(500);
    probe = 'ok';
    const prepared = await call('POST', `/${id}/remove`, {});
    const token = String((prepared.body.confirmation as { token: string }).token);
    expect((await call('POST', `/${id}/remove`, { confirmationToken: token })).status).toBe(200);

    const started = await call('POST', '/add', { provider: 'codex', profileName: 'codex-4' });
    expect(started.status).toBe(202);
    const jobId = String((started.body.job as { id: string }).id);
    expect(
      (await call('POST', `/signin-jobs/${jobId}/code`, { code: '4/0PRIVATE-CODE' })).status
    ).toBe(409);
    const deadline = Date.now() + 5_000;
    while ((runner.get(jobId) as { state: string }).state !== 'succeeded') {
      if (Date.now() > deadline) throw new Error('job did not finish');
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
    await new Promise((resolve) => setTimeout(resolve, 50));

    const logged = JSON.stringify(getRecentLogEntries());
    // The log is really on: the request lines and the audit lines are there.
    expect(logged).toContain('request.completed');
    expect(logged).toContain('accounts.add');
    expect(logged).toContain('accounts.key.replaced');
    expect(logged).toContain('accounts.remove');
    expect(logged).toContain('accounts.signin.job');
    for (const text of [logged, consoleText.join('\n')]) {
      for (const secret of [...SECRETS, token]) {
        expect([secret, text.includes(secret)]).toEqual([secret, false]);
      }
    }
  });
});
