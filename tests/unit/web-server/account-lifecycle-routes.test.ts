/**
 * The account lifecycle routes (CONTRACT-registry-lifecycle sections 1, 5, 6,
 * 10 and 11): scope and guards, API keys, confirmation tokens, remove
 * refusals, Codex jobs, guides, re-check, label, trash and jobs. Every store is
 * in a temporary CCS folder; the collector, CLI and hosts are fakes.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import express from 'express';
import fs from 'fs';
import http, { type Server } from 'http';
import type { AddressInfo } from 'net';
import os from 'os';
import path from 'path';
import { CodexProfileRegistry } from '../../../src/codex-auth/codex-profile-registry';
import { invalidateCodexAuthProfilesCache } from '../../../src/codex-auth/codex-auth-dashboard-service';
import { createAccountLifecycleRouter } from '../../../src/web-server/routes/account-lifecycle-routes';
import { relabel } from '../../../src/web-server/services/account-lifecycle-extras';
import { AccountConfirmationStore } from '../../../src/web-server/services/account-confirmations';
import type {
  AccountDashboard,
  DashboardAccount,
} from '../../../src/web-server/services/account-dashboard-types';
import { keyStoreFor } from '../../../src/web-server/services/account-key-store';
import type { LifecycleEnv } from '../../../src/web-server/services/account-lifecycle-env';
import { lifecycleProviderFacts } from '../../../src/web-server/services/account-lifecycle-runtime';
import type { AdditionalUsageSource } from '../../../src/web-server/services/additional-usage-transport';
import { ClaudeAccountLifecycle } from '../../../src/web-server/services/claude-account-lifecycle';
import type { ClaudeHostTransport } from '../../../src/web-server/services/claude-host-transport';
import { CodexAccountLifecycle } from '../../../src/web-server/services/codex-account-lifecycle';
import { MuseAccountLifecycle } from '../../../src/web-server/services/muse-account-lifecycle';
import { buildDashboardProviders } from '../../../src/web-server/services/dashboard-provider-registry';
import { SignInJobRunner } from '../../../src/web-server/services/signin-jobs';
import { setLocalNetworkTrustResolver } from '../../../src/web-server/middleware/secure-transport';
import { parseTrustedNetworks } from '../../../src/web-server/middleware/trusted-networks';
import { AntigravityAccountLifecycle } from '../../../src/antigravity/account-lifecycle';
import { AntigravityProfileRegistry } from '../../../src/antigravity/registry';
import { claimAntigravitySignInMarker } from '../../../src/antigravity/signin-marker';
import type { NativeCredential, VerifiedIdentity } from '../../../src/antigravity/types';

const KEY = 'zai-TEST-key-0123456789-x7Qa';
const ORIGINAL_CCS_HOME = process.env.CCS_HOME;
const closers: Array<() => Promise<void>> = [];
let root: string;
let ccsDir: string;
let codexHome: string;
let agyHome: string;
/** The live native Antigravity login of the fixture (null: none on this computer). */
let agyNative: NativeCredential | null;
let agyNativeFails: boolean;

beforeEach(() => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-lifecycle-routes-'));
  process.env.CCS_HOME = root;
  ccsDir = path.join(root, '.ccs');
  codexHome = path.join(root, 'native-codex');
  agyHome = path.join(root, 'agy-home');
  agyNative = null;
  agyNativeFails = false;
  fs.mkdirSync(ccsDir, { recursive: true });
  fs.mkdirSync(codexHome);
  fs.mkdirSync(agyHome);
  fs.writeFileSync(
    path.join(ccsDir, 'account-usage-sources.json'),
    JSON.stringify({
      version: 1,
      sources: [
        { provider: 'cursor', platform: 'mac', sshHost: 'jared-mac' },
        { provider: 'qwen', platform: 'windows', sshHost: 'jared-windows' },
        { provider: 'kimi-code', platform: 'mac', sshHost: 'jared-mac' },
      ],
    })
  );
  invalidateCodexAuthProfilesCache();
});

afterEach(async () => {
  for (const close of closers.splice(0).reverse()) await close();
  setLocalNetworkTrustResolver(null);
  if (ORIGINAL_CCS_HOME === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = ORIGINAL_CCS_HOME;
  invalidateCodexAuthProfilesCache();
  fs.rmSync(root, { recursive: true, force: true });
});

function idToken(email: string): string {
  const part = (value: unknown) => Buffer.from(JSON.stringify(value)).toString('base64url');
  return [part({ alg: 'none' }), part({ email }), 'sig'].join('.');
}

function codexProfile(name: string): void {
  new CodexProfileRegistry().createProfile(name, {
    created: new Date().toISOString(),
    last_used: null,
    email: `${name}@example.com`,
  });
  const dir = path.join(ccsDir, 'codex-instances', name);
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(
    path.join(dir, 'auth.json'),
    JSON.stringify({
      tokens: { id_token: idToken(`${name}@example.com`), access_token: 'a', refresh_token: 'r' },
    })
  );
}

function activate(name: string): void {
  fs.copyFileSync(
    path.join(ccsDir, 'codex-instances', name, 'auth.json'),
    path.join(codexHome, 'auth.json')
  );
  invalidateCodexAuthProfilesCache();
}

function agyCredential(email: string, version = 1): NativeCredential {
  return {
    format: 'antigravity-consumer-json',
    bytes: Buffer.from(
      JSON.stringify({
        auth_method: 'consumer',
        token: { access_token: `access:${email}:${version}`, refresh_token: `refresh:${email}` },
      })
    ),
  };
}

function agyIdentity(value: NativeCredential): VerifiedIdentity {
  const email = (JSON.parse(value.bytes.toString('utf8')).token.access_token as string).split(
    ':'
  )[1];
  return {
    email,
    subject: `subject-${email}`,
    plan: null,
    verifiedAt: new Date().toISOString(),
    source: 'provider-userinfo',
  };
}

async function agyProfile(id: string): Promise<void> {
  // The registry needs the private CCS folder the product always has (0700).
  fs.chmodSync(ccsDir, 0o700);
  const registry = new AntigravityProfileRegistry(ccsDir);
  const value = agyCredential(`${id}@example.com`);
  await registry.withLock(async () =>
    registry.saveCredential(id, 'ubuntu', value, agyIdentity(value), Date.now())
  );
}

/** Make `id`'s saved login the live native one (null: no native login file). */
function agyLive(id: string | null): void {
  const file = path.join(agyHome, '.gemini', 'antigravity-cli', 'antigravity-oauth-token');
  if (id === null) {
    agyNative = null;
    fs.rmSync(file, { force: true });
    return;
  }
  agyNative = agyCredential(`${id}@example.com`);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, 'placeholder; the fake reader answers', { mode: 0o600 });
}

function row(id: string, extra: Partial<DashboardAccount> = {}): DashboardAccount {
  const provider = (
    id.startsWith('plan-') ? 'opencode-go' : id.split(':')[0]
  ) as DashboardAccount['provider'];
  return {
    id,
    provider,
    providerLabel: provider,
    label: provider,
    email: null,
    plan: null,
    platform: 'ubuntu',
    source: 'fixture',
    status: 'ok',
    message: null,
    fetchedAt: null,
    sampledAt: null,
    isActive: false,
    windows: [],
    capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
    lifecycle: { state: 'ready', jobId: null },
    hidden: false,
    ...extra,
  };
}

interface Fixture {
  base: string;
  env: LifecycleEnv;
  audits: Array<[string, Record<string, unknown>]>;
  probes: AdditionalUsageSource[];
  opened: string[];
  changes: () => number;
  setProbe: (status: DashboardAccount['status']) => void;
  /** Hold every probe until the promise resolves (null: answer at once). */
  setProbeGate: (gate: Promise<void> | null) => void;
  advance: (ms: number) => void;
  request: (
    method: string,
    route: string,
    body?: unknown,
    headers?: Record<string, string>
  ) => Promise<{ status: number; headers: Headers; body: Record<string, unknown> }>;
}

async function fixture(
  options: {
    claudeEnabled?: boolean;
    antigravityFlow?: 'preflight_failed' | 'tool_missing' | null;
    museEnabled?: boolean;
  } = {}
): Promise<Fixture> {
  const audits: Array<[string, Record<string, unknown>]> = [];
  const probes: AdditionalUsageSource[] = [];
  const opened: string[] = [];
  let changes = 0;
  let probeStatus: DashboardAccount['status'] = 'ok';
  let probeGate: Promise<void> | null = null;
  let now = Date.parse('2026-10-02T08:00:00Z');
  const runner = new SignInJobRunner({
    spawn: () => ({
      onData: () => undefined,
      onExit: () => undefined,
      write: () => false,
      kill: () => undefined,
    }),
  });
  const hosts: ClaudeHostTransport = {
    create: async (host, input) => ({
      launcherName: `Claude (${input.profileId})`,
      launcherPath: `/fake/${host}/${input.profileId}`,
      profilePath: `/fake/${host}/Claude-${input.profileId}`,
      sshHost: input.sshHost,
    }),
    undoCreate: async () => undefined,
    appState: async () => 'stopped',
    sessionState: async () => 'signed-out',
    trash: async () => 'moved',
    restore: async () => undefined,
    purge: async () => undefined,
  };
  const claudeEnabled = options.claudeEnabled === true;
  const museEnabled = options.museEnabled === true;
  const facts = (context: { secureTransport?: boolean }) =>
    lifecycleProviderFacts(context, {
      codexCliAvailable: () => true,
      claudeEnabled: () => claudeEnabled,
      antigravityFlow: () =>
        options.antigravityFlow === undefined ? 'preflight_failed' : options.antigravityFlow,
      museEnabled: () => museEnabled,
    });
  const antigravity = new AntigravityAccountLifecycle({
    ccsDir: () => ccsDir,
    home: () => agyHome,
    validateCredential: async (value) => agyIdentity(value),
    readNativeCredential: async () => {
      if (agyNativeFails || !agyNative) throw new Error('native store unavailable');
      return agyNative;
    },
  });
  const env: LifecycleEnv = {
    ccsDir: () => ccsDir,
    runner: () => runner,
    codex: () => new CodexAccountLifecycle({ codexHome, codexCli: () => '/fake/codex', env: {} }),
    claude: () =>
      new ClaudeAccountLifecycle({
        ccsDir: () => ccsDir,
        transport: hosts,
        enabled: claudeEnabled,
        now: () => now,
      }),
    antigravity: () => antigravity,
    // Hermetic supervised-flow builder: no bubblewrap, no staging on the host.
    antigravityJobFlow: (profileName, mode) => ({
      provider: 'antigravity',
      kind: 'supervised-cli',
      mode,
      accountId: mode === 'signin-again' ? `antigravity:profile:${profileName}` : null,
      profileName,
      platform: 'ubuntu',
      allowedOrigins: ['https://accounts.google.com'],
      timeoutMs: 60_000,
      prepare: async () => ({ file: '/bin/true', args: [], env: {}, pty: false }),
      complete: async () => ({
        accountId: `antigravity:profile:${profileName}`,
        email: `${profileName}@example.test`,
        plan: 'free',
      }),
      cleanup: async () => undefined,
    }),
    muse: () => new MuseAccountLifecycle({ enabled: museEnabled }),
    confirmations: (() => {
      const store = new AccountConfirmationStore(() => now);
      return () => store;
    })(),
    providerFacts: facts,
    getDashboard: async (context): Promise<AccountDashboard> => {
      const accounts = [
        ...new CodexProfileRegistry().listProfiles().map((name) =>
          row(`codex:${name}`, {
            capabilities: { codexProfile: name, claudeProfileId: null, claudePlatforms: [] },
          })
        ),
        row('claude:party', {
          capabilities: { codexProfile: null, claudeProfileId: 'party', claudePlatforms: ['mac'] },
        }),
        row('zai:usage'),
        row('cursor:usage', { platform: 'mac' }),
        row('muse:usage', { platform: 'mac' }),
        row('plan-opencode-go-console-mac-0123456789ab'),
        ...antigravity.listProfiles().map((profile) =>
          row(`antigravity:profile:${profile.id}`, {
            email: profile.email,
            label: profile.email,
            capabilities: {
              codexProfile: null,
              claudeProfileId: null,
              claudePlatforms: [],
              antigravityProfileId: profile.id,
              antigravityHostIds: ['ubuntu'],
            },
          })
        ),
      ];
      return {
        schemaVersion: 1,
        updatedAt: new Date(now).toISOString(),
        providers: buildDashboardProviders(accounts, [], [], facts(context)),
        accounts,
        codexAutoSwitch: {
          enabled: false,
          thresholdPercent: 5,
          thresholdUsedPercent: 95,
          pollIntervalSeconds: 60,
          outcome: 'disabled',
          message: '',
          activationInProgress: false,
        },
      };
    },
    probe: async (source) => {
      probes.push(source);
      if (probeGate) await probeGate;
      return { ...row(source.account?.id ?? `${source.provider}:usage`), status: probeStatus };
    },
    refreshAdditional: async (id) => row(id, { status: 'cached' }),
    replaceRow: () => undefined,
    keyStore: (location) => keyStoreFor(location, { ccsDir }),
    openCursor: async (host) => {
      opened.push(host);
    },
    onChanged: () => {
      changes += 1;
    },
    audit: (event, data) => audits.push([event, data]),
    now: () => now,
  };
  const app = express();
  app.use(express.json());
  app.use((req, _res, next) => {
    // The peer the route sees: `x-test-peer`, or the real loopback peer. Set on every
    // request, because a kept-alive socket carries the last value to the next one.
    const peer = req.headers['x-test-peer'];
    Object.defineProperty(req.socket, 'remoteAddress', {
      value: typeof peer === 'string' && peer ? peer : '127.0.0.1',
      configurable: true,
    });
    const session = req.headers['x-test-session'];
    if (typeof session === 'string' && session) {
      Object.assign(req, { session: { authenticated: true }, sessionID: session });
    }
    if (req.headers['x-test-device'] === 'true') Object.assign(req, { auth: { kind: 'device' } });
    next();
  });
  app.use('/api/accounts', createAccountLifecycleRouter({ env: () => env }));
  app.use((_req, res) => res.status(404).json({ error: 'API endpoint was not found.' }));
  const server: Server = http.createServer(app);
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', () => resolve()));
  closers.push(() => new Promise<void>((resolve) => server.close(() => resolve())));
  const base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  return {
    base,
    env,
    audits,
    probes,
    opened,
    changes: () => changes,
    setProbe: (status) => {
      probeStatus = status;
    },
    setProbeGate: (gate) => {
      probeGate = gate;
    },
    advance: (ms) => {
      now += ms;
    },
    request: async (method, route, body, headers = {}) => {
      const response = await fetch(`${base}/api/accounts${route}`, {
        method,
        headers: {
          'x-test-session': 'a',
          origin: base,
          ...(body === undefined ? {} : { 'content-type': 'application/json' }),
          ...headers,
        },
        body:
          body === undefined ? undefined : typeof body === 'string' ? body : JSON.stringify(body),
      });
      const text = await response.text();
      return {
        status: response.status,
        headers: response.headers,
        body: text ? (JSON.parse(text) as Record<string, unknown>) : {},
      };
    },
  };
}

const PLAIN = { 'x-forwarded-for': '192.168.1.20' };

function files(dir: string): string[] {
  if (!fs.existsSync(dir)) return [];
  return fs.readdirSync(dir, { recursive: true }).map(String).sort();
}

describe('lifecycle route scope and guards', () => {
  it('needs a browser session, the dashboard origin, JSON, a strict body and no query', async () => {
    const f = await fixture();
    const routes: Array<[string, string, unknown]> = [
      ['GET', '/registry', undefined],
      ['POST', '/add', { provider: 'zai', key: KEY }],
      ['POST', '/zai:usage/signin-again', {}],
      ['PUT', '/zai:usage/key', { key: KEY }],
      ['POST', '/zai:usage/remove', {}],
      ['POST', '/cursor:usage/open', { platform: 'mac' }],
      ['POST', '/zai:usage/recheck', {}],
      ['PATCH', '/zai:usage', { label: 'Work' }],
      ['GET', '/trash', undefined],
      ['POST', '/trash/tr_0123456789abcdef/restore', {}],
      ['POST', '/trash/tr_0123456789abcdef/purge', {}],
      ['GET', '/signin-jobs/job_0123456789abcdef', undefined],
      ['POST', '/signin-jobs/job_0123456789abcdef/cancel', {}],
      ['POST', '/signin-jobs/job_0123456789abcdef/code', { code: 'x' }],
    ];
    for (const [method, route, body] of routes) {
      const anonymous = await f.request(method, route, body, { 'x-test-session': '' });
      expect([route, anonymous.status, anonymous.body.code]).toEqual([route, 401, 'auth_required']);
      expect(anonymous.headers.get('cache-control')).toBe('no-store');
      const device = await f.request(method, route, body, { 'x-test-device': 'true' });
      expect([route, device.status, device.body.code]).toEqual([route, 403, 'device_scope']);
      const query = await f.request(method, `${route}?PRIVATE=1`, body);
      expect([route, query.status, query.body.code]).toEqual([route, 400, 'unexpected_query']);
      if (method === 'GET') continue;
      const foreign = await f.request(method, route, body, { origin: 'https://evil.example' });
      expect([route, foreign.status, foreign.body.code]).toEqual([route, 403, 'origin_required']);
      const text = await f.request(method, route, JSON.stringify(body), {
        'content-type': 'text/plain',
      });
      expect([route, text.status, text.body.code]).toEqual([route, 415, 'json_required']);
      const large = await f.request(method, route, { pad: 'x'.repeat(9000) });
      expect([route, large.status, large.body.code]).toEqual([route, 400, 'invalid_body']);
    }
    expect(f.changes()).toBe(0);
    expect(files(path.join(ccsDir, 'account-usage'))).toEqual([]);
    expect(fs.existsSync(path.join(ccsDir, 'account-usage-accounts.json'))).toBe(false);
  });

  it('leaves other /api/accounts paths to the 404 and rejects malformed ids', async () => {
    const f = await fixture();
    // A bare PATCH could be any other path under /api/accounts.
    expect((await f.request('PATCH', '/nope', {})).status).toBe(404);
    // The action routes own their path shape: any malformed id there is 400, after the guard.
    for (const [method, route] of [
      ['POST', '/nope/remove'],
      ['PUT', '/dashboard/key'],
      ['POST', '/nope/signin-again'],
      ['POST', '/nope/open'],
      ['POST', '/nope/recheck'],
      ['POST', '/codex:..%2f..%2fx/remove'],
    ]) {
      const malformed = await f.request(
        method,
        route,
        route.endsWith('/open') ? { platform: 'mac' } : {}
      );
      expect([route, malformed.status, malformed.body.code]).toEqual([
        route,
        400,
        'invalid_account',
      ]);
      const anonymous = await f.request(method, route, {}, { 'x-test-session': '' });
      expect([route, anonymous.status]).toEqual([route, 401]);
    }
    const malformed = await f.request('POST', '/codex:..%2f..%2fx/remove', {});
    expect([malformed.status, malformed.body.code]).toEqual([400, 'invalid_account']);
    const unknown = await f.request('POST', '/zai:acct:00000000/remove', {});
    expect([unknown.status, unknown.body.code]).toEqual([404, 'unknown_account']);
    expect(JSON.stringify([malformed.body, unknown.body])).not.toContain('00000000');
  });
});

describe('API keys', () => {
  it('adds a key over a secure transport and returns only last4 and fingerprint', async () => {
    const f = await fixture();
    const v1 = fs.readFileSync(path.join(ccsDir, 'account-usage-sources.json'));
    const response = await f.request('POST', '/add', { provider: 'zai', key: KEY, label: 'Work' });
    expect(response.status).toBe(201);
    const account = response.body.account as Record<string, unknown>;
    expect(account).toMatchObject({
      provider: 'zai',
      label: 'Work',
      platform: 'ubuntu',
      credential: { kind: 'aac-key', last4: 'x7Qa', storedOn: 'ubuntu' },
      actions: { replaceKey: true, remove: true, recheck: true, signInAgain: false },
      removeRefusal: null,
    });
    expect(account.id).toMatch(/^zai:acct:[a-f0-9]{8}$/);
    expect(response.body.check).toBe('ok');
    const keyId = String(account.id).slice('zai:acct:'.length);
    expect(f.probes).toEqual([
      {
        provider: 'zai',
        platform: 'ubuntu',
        account: { id: account.id, label: 'Work', credential: { kind: 'aac-key', keyId } },
      },
    ]);
    const keyFile = path.join(ccsDir, 'account-usage', 'keys', `zai-${keyId}.json`);
    expect(fs.statSync(keyFile).mode & 0o777).toBe(0o600);
    const registryText = fs.readFileSync(path.join(ccsDir, 'account-usage-accounts.json'), 'utf8');
    const registry = JSON.parse(registryText);
    expect(registry.accounts.map((entry: { id: string }) => entry.id)).toContain('zai:usage');
    expect(registry.accounts.map((entry: { id: string }) => entry.id)).toContain(account.id);
    expect(fs.readFileSync(path.join(ccsDir, 'account-usage-sources.json'))).toEqual(v1);
    for (const text of [JSON.stringify(response.body), registryText, JSON.stringify(f.audits)]) {
      expect(text).not.toContain(KEY);
    }
    expect(JSON.stringify(f.audits)).not.toContain('sha256:');
    expect(f.audits).toEqual([['accounts.add', { provider: 'zai', kind: 'api-key' }]]);
    expect(f.changes()).toBe(1);
    const duplicate = await f.request('POST', '/add', { provider: 'zai', key: KEY });
    expect([duplicate.status, duplicate.body.code]).toEqual([409, 'duplicate_key']);
  });

  it('refuses a key over plain HTTP from the LAN before reading it', async () => {
    const f = await fixture();
    const response = await f.request('POST', '/add', { provider: 'zai', key: KEY }, PLAIN);
    expect([response.status, response.body.code]).toEqual([403, 'secure_transport_required']);
    expect(JSON.stringify(response.body)).not.toContain(KEY);
    expect(files(path.join(ccsDir, 'account-usage'))).toEqual([]);
    expect(f.probes).toEqual([]);
  });

  it('deletes a rejected key and its entry, keeps an unverified one', async () => {
    const f = await fixture();
    f.setProbe('needs_sign_in');
    const rejected = await f.request('POST', '/add', { provider: 'opencode-go', key: KEY });
    expect([rejected.status, rejected.body.code]).toEqual([422, 'key_rejected']);
    expect(files(path.join(ccsDir, 'account-usage', 'keys'))).toEqual([]);
    const registry = JSON.parse(
      fs.readFileSync(path.join(ccsDir, 'account-usage-accounts.json'), 'utf8')
    );
    expect(registry.accounts.some((entry: { id: string }) => entry.id.includes(':acct:'))).toBe(
      false
    );
    f.setProbe('error');
    const unverified = await f.request('POST', '/add', { provider: 'opencode-go', key: KEY });
    expect([unverified.status, unverified.body.check]).toEqual([201, 'unverified']);
  });

  it('replaces a key only when the new one is not rejected, and only for AAC keys', async () => {
    const f = await fixture();
    const added = await f.request('POST', '/add', { provider: 'zai', key: KEY });
    const id = String((added.body.account as { id: string }).id);
    const keyFile = path.join(ccsDir, 'account-usage', 'keys', `zai-${id.slice(9)}.json`);
    const before = fs.readFileSync(keyFile, 'utf8');
    f.setProbe('needs_sign_in');
    const rejected = await f.request('PUT', `/${id}/key`, { key: 'zai-NEW-key-9999' });
    expect([rejected.status, rejected.body.code]).toEqual([422, 'key_rejected']);
    expect(fs.readFileSync(keyFile, 'utf8')).toBe(before);
    expect(files(path.join(ccsDir, 'account-usage', 'keys'))).toEqual([path.basename(keyFile)]);
    f.setProbe('ok');
    const replaced = await f.request('PUT', `/${id}/key`, { key: 'zai-NEW-key-9999' });
    expect(replaced.status).toBe(200);
    expect(replaced.body).toMatchObject({
      check: 'ok',
      account: { credential: { last4: '9999' } },
    });
    expect(files(path.join(ccsDir, 'account-usage', 'keys'))).toEqual([path.basename(keyFile)]);
    const discover = await f.request('PUT', '/zai:usage/key', { key: 'zai-NEW-key-9999' });
    expect([discover.status, discover.body.code]).toEqual([409, 'not_aac_owned']);
    const plain = await f.request('PUT', `/${id}/key`, { key: 'zai-NEW-key-9999' }, PLAIN);
    expect([plain.status, plain.body.code]).toEqual([403, 'secure_transport_required']);
  });
});

describe('trusted local network (CONTRACT-auth-devices 2a rule 4)', () => {
  const LAN = { 'x-test-peer': '192.168.50.20' };
  const MAPPED = { 'x-test-peer': '::ffff:10.6.0.2' };
  const PUBLIC = { 'x-test-peer': '203.0.113.9' };
  const trust = (enabled: boolean) =>
    setLocalNetworkTrustResolver(() => ({
      enabled,
      networks: parseTrustedNetworks(undefined).networks,
    }));

  it('accepts key add and replace, job create and code submit from a private peer only while on', async () => {
    const f = await fixture();
    codexProfile('gmail');
    const own = await f.request('POST', '/add', { provider: 'zai', key: KEY });
    const id = String((own.body.account as { id: string }).id);
    const sensitive = async (headers: Record<string, string>, suffix: string) => ({
      add: await f.request(
        'POST',
        '/add',
        { provider: 'kimi-code', key: `${KEY}${suffix}` },
        headers
      ),
      replace: await f.request('PUT', `/${id}/key`, { key: `zai-NEW-key-${suffix}` }, headers),
      job: await f.request(
        'POST',
        '/add',
        { provider: 'codex', profileName: `codex-${suffix}` },
        headers
      ),
      code: await f.request(
        'POST',
        '/signin-jobs/job_00000000000000aa/code',
        { code: 'abc' },
        headers
      ),
    });
    const refusedEverywhere = (result: Awaited<ReturnType<typeof sensitive>>) => {
      for (const [name, response] of Object.entries(result)) {
        expect([name, response.status, response.body.code]).toEqual([
          name,
          403,
          'secure_transport_required',
        ]);
      }
    };

    // Off (the default): the private peer is plain HTTP.
    refusedEverywhere(await sensitive(LAN, '1'));
    trust(false);
    refusedEverywhere(await sensitive(MAPPED, '2'));
    expect(f.probes).toHaveLength(1);

    trust(true);
    refusedEverywhere(await sensitive(PUBLIC, '3'));
    const lan = await sensitive(LAN, '4');
    expect(lan.add.status).toBe(201);
    expect(lan.replace.status).toBe(200);
    expect(lan.job.status).toBe(202);
    // Past the transport gate: this job id was never issued.
    expect([lan.code.status, lan.code.body.code]).toEqual([404, 'unknown_job']);
    const job = lan.job.body.job as { id: string };
    const cancelled = await f.request('POST', `/signin-jobs/${job.id}/cancel`, {}, MAPPED);
    expect(cancelled.status).toBe(200);
    const mapped = await f.request(
      'POST',
      '/add',
      { provider: 'opencode-go', key: `${KEY}5` },
      MAPPED
    );
    expect(mapped.status).toBe(201);

    // Turned off again: refused at once.
    trust(false);
    refusedEverywhere(await sensitive(LAN, '6'));
  });

  it('offers API-key sign-in to a trusted private peer only', async () => {
    const f = await fixture();
    trust(true);
    const started = await f.request(
      'POST',
      '/add',
      { provider: 'codex', profileName: 'codex-7' },
      LAN
    );
    expect(started.status).toBe(202);
    const listing = await f.request('GET', '/registry', undefined, LAN);
    expect(listing.status).toBe(200);
    const plain = await f.request('GET', '/registry', undefined, PUBLIC);
    expect(plain.status).toBe(200);
    const signIn = (body: Record<string, unknown>) =>
      (body.providers as Array<{ id: string; signIn: { unavailableReason: string | null } }>).find(
        (provider) => provider.id === 'zai'
      )?.signIn.unavailableReason;
    expect(signIn(listing.body)).toBeNull();
    expect(signIn(plain.body)).toBe('secure_transport_required');
  });
});

describe('remove with a confirmation token', () => {
  it('prepares, commits once, and refuses a reused, foreign or stale token', async () => {
    const f = await fixture();
    const added = await f.request('POST', '/add', { provider: 'zai', key: KEY });
    const id = String((added.body.account as { id: string }).id);
    const prepared = await f.request('POST', `/${id}/remove`, {});
    expect(prepared.status).toBe(200);
    const confirmation = prepared.body.confirmation as Record<string, unknown>;
    expect(confirmation.effects).toEqual(['The stored key is deleted from Ubuntu.']);
    expect(String(confirmation.token)).toMatch(/^[A-Za-z0-9_-]{43}$/);
    const foreign = await f.request(
      'POST',
      `/${id}/remove`,
      { confirmationToken: confirmation.token },
      { 'x-test-session': 'b' }
    );
    expect([foreign.status, foreign.body.code]).toEqual([409, 'confirmation_stale']);
    // The foreign attempt used the token up.
    const again = await f.request('POST', `/${id}/remove`, {
      confirmationToken: confirmation.token,
    });
    expect([again.status, again.body.code]).toEqual([409, 'confirmation_stale']);
    const fresh = (await f.request('POST', `/${id}/remove`, {})).body.confirmation as {
      token: string;
    };
    const removed = await f.request('POST', `/${id}/remove`, { confirmationToken: fresh.token });
    expect(removed.body).toEqual({ removed: true, trashId: null, purgeAfter: null });
    expect(files(path.join(ccsDir, 'account-usage', 'keys'))).toEqual([]);
    const second = await f.request('POST', `/${id}/remove`, { confirmationToken: fresh.token });
    expect([second.status, second.body.code]).toEqual([409, 'confirmation_stale']);
    expect(f.audits.at(-1)).toEqual([
      'accounts.remove',
      { provider: 'zai', kind: 'api-key', trashed: false },
    ]);
    const expired = (await f.request('POST', '/zai:usage/remove', {})).body.confirmation as {
      token: string;
    };
    f.advance(121_000);
    const late = await f.request('POST', '/zai:usage/remove', { confirmationToken: expired.token });
    expect([late.status, late.body.code]).toEqual([409, 'confirmation_stale']);
  });

  it('refuses the active, default and last Codex profile at prepare and at commit', async () => {
    const f = await fixture();
    codexProfile('gmail');
    const last = await f.request('POST', '/codex:gmail/remove', {});
    expect([last.status, last.body.code]).toEqual([409, 'last_account']);
    codexProfile('party');
    codexProfile('spare');
    new CodexProfileRegistry().setDefault('gmail');
    activate('party');
    for (const [name, code] of [
      ['party', 'account_active'],
      ['gmail', 'account_default'],
    ]) {
      const refused = await f.request('POST', `/codex:${name}/remove`, {});
      expect([name, refused.status, refused.body.code]).toEqual([name, 409, code]);
    }
    const prepared = (await f.request('POST', '/codex:spare/remove', {})).body.confirmation as {
      token: string;
      effects: string[];
    };
    expect(prepared.effects[0]).toContain('Codex login');
    activate('spare');
    const commit = await f.request('POST', '/codex:spare/remove', {
      confirmationToken: prepared.token,
    });
    expect([commit.status, commit.body.code]).toEqual([409, 'account_active']);
    expect(new CodexProfileRegistry().listProfiles().sort()).toEqual(['gmail', 'party', 'spare']);
    expect(f.audits.filter(([event]) => event === 'accounts.remove.refused')).toContainEqual([
      'accounts.remove.refused',
      { provider: 'codex', code: 'account_active' },
    ]);
    activate('party');
    const ready = (await f.request('POST', '/codex:spare/remove', {})).body.confirmation as {
      token: string;
    };
    // A state change between the two calls (another account activated) makes it stale.
    activate('gmail');
    const stale = await f.request('POST', '/codex:spare/remove', {
      confirmationToken: ready.token,
    });
    expect([stale.status, stale.body.code]).toEqual([409, 'confirmation_stale']);
    const token = (
      (await f.request('POST', '/codex:spare/remove', {})).body.confirmation as { token: string }
    ).token;
    const done = await f.request('POST', '/codex:spare/remove', { confirmationToken: token });
    expect(done.body).toEqual({ removed: true, trashId: null, purgeAfter: null });
    expect(new CodexProfileRegistry().listProfiles().sort()).toEqual(['gmail', 'party']);
  });

  it('names the live Codex login by workspace, and by email when a binding is unreadable', async () => {
    const f = await fixture();
    const part = (value: unknown) => Buffer.from(JSON.stringify(value)).toString('base64url');
    const bound = (email: string, workspace: string): string =>
      JSON.stringify({
        tokens: {
          id_token: [
            part({ alg: 'none' }),
            part({
              email,
              'https://api.openai.com/auth': {
                chatgpt_account_id: workspace,
                chatgpt_user_id: 'user-1',
              },
            }),
            'sig',
          ].join('.'),
          access_token: 'a',
          refresh_token: 'r',
          account_id: workspace,
        },
      });
    const saveLogin = (name: string, content: string) =>
      fs.writeFileSync(path.join(ccsDir, 'codex-instances', name, 'auth.json'), content);
    codexProfile('spare');
    codexProfile('personal');
    codexProfile('work');
    new CodexProfileRegistry().setDefault('spare');
    saveLogin('personal', bound('same@example.com', 'ws-personal'));
    saveLogin('work', bound('same@example.com', 'ws-work'));
    activate('work');
    // One email, two workspaces: only the live workspace's profile is refused.
    const live = await f.request('POST', '/codex:work/remove', {});
    expect([live.status, live.body.code]).toEqual([409, 'account_active']);
    const again = await f.request('POST', '/codex:work/signin-again', {});
    expect([again.status, again.body.code]).toEqual([409, 'account_active']);
    expect((await f.request('POST', '/codex:personal/remove', {})).status).toBe(200);
    // A live login without a readable workspace binding names no profile in the summary;
    // the refusals still follow the removal guard, where the email decides.
    fs.writeFileSync(
      path.join(codexHome, 'auth.json'),
      JSON.stringify({
        tokens: { id_token: idToken('same@example.com'), access_token: 'a', refresh_token: 'r' },
      })
    );
    invalidateCodexAuthProfilesCache();
    for (const name of ['personal', 'work']) {
      const refused = await f.request('POST', `/codex:${name}/remove`, {});
      expect([name, refused.status, refused.body.code]).toEqual([name, 409, 'account_active']);
    }
    const spare = await f.request('POST', '/codex:spare/remove', {});
    expect([spare.status, spare.body.code]).toEqual([409, 'account_default']);
  });

  it('answers 404 for an Antigravity profile not saved', async () => {
    const f = await fixture();
    const missing = await f.request('POST', '/antigravity:profile:party/remove', {});
    expect([missing.status, missing.body.code]).toEqual([404, 'unknown_account']);
  });

  it('removes a console wallet by deleting its stored source, never the browser session', async () => {
    const f = await fixture();
    const id = 'plan-opencode-go-console-mac-0123456789ab';
    const sourceFile = path.join(ccsDir, 'opencode-console-wallet-source.json');
    fs.writeFileSync(
      sourceFile,
      JSON.stringify({ version: 1, platform: 'mac', sshHost: 'fixture-mac' })
    );
    const ask = await f.request('POST', `/${id}/remove`, {});
    expect(ask.status).toBe(200);
    const confirmation = ask.body.confirmation as { token: string; effects: string[] };
    expect(confirmation.effects).toEqual([
      'The console wallet is no longer read by the dashboard.',
      'Its sign-in in the browser is not changed.',
    ]);
    const commit = await f.request('POST', `/${id}/remove`, {
      confirmationToken: confirmation.token,
    });
    expect(commit.body).toEqual({ removed: true, trashId: null, purgeAfter: null });
    expect(fs.existsSync(sourceFile)).toBe(false);
    // A second commit with the same token is stale.
    const again = await f.request('POST', `/${id}/remove`, {
      confirmationToken: confirmation.token,
    });
    expect([again.status, again.body.code]).toEqual([409, 'confirmation_stale']);
    expect(f.audits.find(([event]) => event === 'accounts.remove')?.[1]).toMatchObject({
      provider: 'opencode-go',
      kind: 'browser-session',
      trashed: false,
    });
  });

  it('wallet Remove is stale when its stored source changes between prepare and commit', async () => {
    const f = await fixture();
    const id = 'plan-opencode-go-console-mac-0123456789ab';
    const sourceFile = path.join(ccsDir, 'opencode-console-wallet-source.json');
    fs.writeFileSync(
      sourceFile,
      JSON.stringify({ version: 1, platform: 'mac', sshHost: 'fixture-mac' })
    );
    const ask = await f.request('POST', `/${id}/remove`, {});
    const token = (ask.body.confirmation as { token: string }).token;
    fs.writeFileSync(
      sourceFile,
      JSON.stringify({ version: 1, platform: 'mac', sshHost: 'other-mac' })
    );
    const commit = await f.request('POST', `/${id}/remove`, { confirmationToken: token });
    expect([commit.status, commit.body.code]).toEqual([409, 'confirmation_stale']);
    expect(fs.existsSync(sourceFile)).toBe(true);
  });
});

async function until(check: () => boolean, ms = 5_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!check()) {
    if (Date.now() > deadline) throw new Error('timed out');
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

function registryIds(): string[] {
  return JSON.parse(
    fs.readFileSync(path.join(ccsDir, 'account-usage-accounts.json'), 'utf8')
  ).accounts.map((entry: { id: string }) => entry.id);
}

describe('remove races and failures', () => {
  it('serializes Remove behind a running Replace key: no key is ever written back', async () => {
    const f = await fixture();
    const added = await f.request('POST', '/add', { provider: 'zai', key: KEY });
    const id = String((added.body.account as { id: string }).id);
    const keysDir = path.join(ccsDir, 'account-usage', 'keys');
    const token = (
      (await f.request('POST', `/${id}/remove`, {})).body.confirmation as {
        token: string;
      }
    ).token;
    let open: () => void = () => undefined;
    f.setProbeGate(
      new Promise<void>((resolve) => {
        open = resolve;
      })
    );
    const probesBefore = f.probes.length;
    const replacing = f.request('PUT', `/${id}/key`, { key: 'zai-NEW-key-9999' });
    await until(() => f.probes.length > probesBefore);
    let removedAt = 0;
    const removing = f
      .request('POST', `/${id}/remove`, { confirmationToken: token })
      .then((response) => {
        removedAt = Date.now();
        return response;
      });
    await new Promise((resolve) => setTimeout(resolve, 100));
    // The remove waits in the provider's key queue while the probe runs.
    expect(removedAt).toBe(0);
    expect(registryIds()).toContain(id);
    open();
    f.setProbeGate(null);
    const [replaced, removed] = await Promise.all([replacing, removing]);
    expect(replaced.status).toBe(200);
    expect(removed.body).toEqual({ removed: true, trashId: null, purgeAfter: null });
    expect(files(keysDir)).toEqual([]);
    expect(registryIds()).not.toContain(id);
    // The other order: a Replace key queued behind a Remove finds no account and writes nothing.
    const second = await f.request('POST', '/add', { provider: 'zai', key: 'zai-SECOND-key-0001' });
    const secondId = String((second.body.account as { id: string }).id);
    const secondToken = (
      (await f.request('POST', `/${secondId}/remove`, {})).body.confirmation as { token: string }
    ).token;
    const [gone, late] = await Promise.all([
      f.request('POST', `/${secondId}/remove`, { confirmationToken: secondToken }),
      f.request('PUT', `/${secondId}/key`, { key: 'zai-THIRD-key-0002' }),
    ]);
    expect(gone.status).toBe(200);
    expect([200, 404]).toContain(late.status);
    expect(files(keysDir)).toEqual([]);
    expect(registryIds()).not.toContain(secondId);
  });

  it('puts the entry back when the key cannot be deleted, so nothing changed', async () => {
    const f = await fixture();
    const added = await f.request('POST', '/add', { provider: 'zai', key: KEY });
    const id = String((added.body.account as { id: string }).id);
    const before = registryIds();
    const keysDir = path.join(ccsDir, 'account-usage', 'keys');
    const keyFiles = files(keysDir);
    const token = (
      (await f.request('POST', `/${id}/remove`, {})).body.confirmation as {
        token: string;
      }
    ).token;
    const realStore = f.env.keyStore;
    f.env.keyStore = (location) => {
      const store = realStore(location);
      return store
        ? Object.assign(Object.create(Object.getPrototypeOf(store)), store, {
            delete: async () => {
              throw new Error('disk error at /private/path');
            },
          })
        : null;
    };
    const failed = await f.request('POST', `/${id}/remove`, { confirmationToken: token });
    expect([failed.status, failed.body.code]).toEqual([500, 'remove_failed']);
    expect(JSON.stringify(failed.body)).not.toContain('/private/path');
    expect(registryIds()).toEqual(before);
    expect(files(keysDir)).toEqual(keyFiles);
  });

  it('refuses when a refusal check throws, at prepare and at commit', async () => {
    const f = await fixture();
    codexProfile('gmail');
    codexProfile('party');
    codexProfile('spare');
    let broken = true;
    f.env.codex = () =>
      new CodexAccountLifecycle({
        codexHome,
        codexCli: () => '/fake/codex',
        env: {},
        activeProfile: async () => {
          if (broken) throw new Error('summary unreadable');
          return null;
        },
      });
    const prepare = await f.request('POST', '/codex:spare/remove', {});
    expect([prepare.status, prepare.body.code]).toEqual([500, 'remove_failed']);
    broken = false;
    const token = (
      (await f.request('POST', '/codex:spare/remove', {})).body.confirmation as {
        token: string;
      }
    ).token;
    broken = true;
    const commit = await f.request('POST', '/codex:spare/remove', { confirmationToken: token });
    // At commit the fingerprint or the refusal check throws: either way nothing is removed.
    expect(commit.status).toBe(500);
    expect(new CodexProfileRegistry().listProfiles().sort()).toEqual(['gmail', 'party', 'spare']);
    expect(f.audits).toContainEqual([
      'accounts.remove.refused',
      { provider: 'codex', code: 'remove_failed' },
    ]);
    expect(f.audits.some(([event]) => event === 'accounts.remove')).toBe(false);
  });

  it('answers too_many_accounts at the 64-account total', async () => {
    const f = await fixture();
    const entry = (provider: string, index: number, credential: Record<string, string>) => ({
      id: `${provider}:acct:${String(index).padStart(8, '0')}`,
      provider,
      platform: 'ubuntu',
      sshHost: null,
      label: null,
      credential,
      createdAt: null,
      createdBy: 'dashboard',
    });
    const hex = (index: number) => String(index).padStart(8, '0');
    const accounts = [
      ...Array.from({ length: 16 }, (_, i) =>
        entry('kimi-code', i, { kind: 'aac-key', keyId: hex(i) })
      ),
      ...Array.from({ length: 16 }, (_, i) =>
        entry('opencode-go', i, { kind: 'aac-key', keyId: hex(i) })
      ),
      ...Array.from({ length: 16 }, (_, i) =>
        entry('muse', i, { kind: 'config-home', homeId: hex(i) })
      ),
      // zai stays one under its own limit of 16, so only the total of 64 is reached.
      ...Array.from({ length: 15 }, (_, i) => entry('zai', i, { kind: 'aac-key', keyId: hex(i) })),
      entry('antigravity', 0, { kind: 'antigravity-profile', profileId: 'party' }),
    ];
    fs.writeFileSync(
      path.join(ccsDir, 'account-usage-accounts.json'),
      JSON.stringify({ version: 2, accounts }),
      { mode: 0o600 }
    );
    expect(registryIds()).toHaveLength(64);
    const before = fs.readFileSync(path.join(ccsDir, 'account-usage-accounts.json'));
    const cursor = await f.request('POST', '/add', { provider: 'cursor' });
    expect([cursor.status, cursor.body.code]).toEqual([409, 'too_many_accounts']);
    const key = await f.request('POST', '/add', { provider: 'zai', key: KEY });
    expect([key.status, key.body.code]).toEqual([409, 'too_many_accounts']);
    expect(fs.readFileSync(path.join(ccsDir, 'account-usage-accounts.json'))).toEqual(before);
    expect(files(path.join(ccsDir, 'account-usage', 'keys'))).toEqual([]);
  });

  it('answers 404 when an account is removed between the label lookup and the write', async () => {
    const f = await fixture();
    const added = await f.request('POST', '/add', { provider: 'zai', key: KEY });
    const id = String((added.body.account as { id: string }).id);
    // The lookup sees the account; the write sees the registry after a Remove committed.
    const other = path.join(root, 'after-remove', '.ccs');
    fs.mkdirSync(path.dirname(other), { recursive: true });
    fs.cpSync(ccsDir, other, { recursive: true });
    const registryFile = path.join(other, 'account-usage-accounts.json');
    const registry = JSON.parse(fs.readFileSync(registryFile, 'utf8'));
    registry.accounts = registry.accounts.filter((entry: { id: string }) => entry.id !== id);
    fs.writeFileSync(registryFile, JSON.stringify(registry), { mode: 0o600 });
    let calls = 0;
    const env = { ...f.env, ccsDir: () => (calls++ === 0 ? ccsDir : other) };
    await expect(
      relabel(env, id, { label: 'Late' }, { secure: true, sessionKey: 'x' })
    ).rejects.toMatchObject({ status: 404, code: 'unknown_account' });
    expect(JSON.parse(fs.readFileSync(registryFile, 'utf8'))).toEqual(registry);
  });
});

describe('sign-in and guides', () => {
  it('starts a Codex device-code job and allows one per provider', async () => {
    const f = await fixture();
    codexProfile('gmail');
    const started = await f.request('POST', '/add', { provider: 'codex', profileName: 'codex-4' });
    expect(started.status).toBe(202);
    const job = started.body.job as Record<string, unknown>;
    expect(job).toMatchObject({
      provider: 'codex',
      kind: 'device-code',
      mode: 'add',
      state: 'starting',
    });
    const again = await f.request('POST', '/add', { provider: 'codex', profileName: 'codex-5' });
    expect([again.status, again.body.code, again.body.jobId]).toEqual([409, 'job_running', job.id]);
    const taken = await f.request('POST', '/add', { provider: 'codex', profileName: 'gmail' });
    expect([taken.status, taken.body.code]).toEqual([409, 'id_in_use']);
    const plain = await f.request(
      'POST',
      '/add',
      { provider: 'codex', profileName: 'codex-6' },
      PLAIN
    );
    expect(plain.status).toBe(403);
    expect(plain.body).toMatchObject({
      code: 'secure_transport_required',
      fallback: { kind: 'terminal', host: 'ubuntu' },
    });
    const read = await f.request('GET', `/signin-jobs/${job.id}`);
    expect(read.body).toMatchObject({ id: job.id, state: 'starting' });
    const code = await f.request('POST', `/signin-jobs/${job.id}/code`, { code: 'abc' });
    expect([code.status, code.body.code]).toEqual([409, 'code_not_expected']);
    const malformed = await f.request('POST', `/signin-jobs/${job.id}/code`, { code: 'a b' });
    expect([malformed.status, malformed.body.code]).toEqual([400, 'invalid_body']);
    const cancelled = await f.request('POST', `/signin-jobs/${job.id}/cancel`, {});
    expect(cancelled.body).toMatchObject({ state: 'cancelled' });
    const restarted = await f.request('GET', '/signin-jobs/job_00000000000000ff');
    expect(restarted.body).toMatchObject({ state: 'failed', error: { code: 'server_restarted' } });
    expect((await f.request('GET', '/signin-jobs/not-a-job')).status).toBe(404);
  });

  it('refuses Sign in again on the live active Codex profile and guides the app providers', async () => {
    const f = await fixture();
    codexProfile('gmail');
    codexProfile('party');
    activate('party');
    const active = await f.request('POST', '/codex:party/signin-again', {});
    expect([active.status, active.body.code]).toEqual([409, 'account_active']);
    const started = await f.request('POST', '/codex:gmail/signin-again', {});
    expect(started.status).toBe(202);
    expect(started.body.job).toMatchObject({ mode: 'signin-again', accountId: 'codex:gmail' });
    expect((await f.request('POST', '/cursor:usage/signin-again', {})).body).toEqual({
      guide: { kind: 'open-app', platforms: ['mac'] },
    });
    expect((await f.request('POST', '/qwen:usage/signin-again', {})).body).toEqual({
      guide: { kind: 'browser-extension', platform: 'windows' },
    });
    expect(
      (await f.request('POST', '/plan-opencode-go-console-mac-0123456789ab/signin-again', {})).body
    ).toEqual({ guide: { kind: 'browser-extension', platform: 'mac' } });
    const key = await f.request('POST', '/zai:usage/signin-again', {});
    expect([key.status, key.body.code]).toEqual([409, 'use_replace_key']);
    const muse = await f.request('POST', '/muse:usage/signin-again', {});
    expect([muse.status, muse.body.code]).toEqual([409, 'not_implemented']);
    const antigravity = await f.request('POST', '/add', {
      provider: 'antigravity',
      profileName: 'x',
    });
    expect([antigravity.status, antigravity.body.code]).toEqual([409, 'preflight_failed']);
    expect(antigravity.body.fallback).toEqual({
      kind: 'terminal',
      host: 'ubuntu',
      command: 'ai-account-center antigravity signin x',
    });
    const command = await f.request('GET', '/signin-command?provider=antigravity&profile=party');
    expect([command.status, command.body]).toEqual([
      200,
      { host: 'ubuntu', command: 'ai-account-center antigravity signin party' },
    ]);
    const badCommand = await f.request('GET', '/signin-command?provider=codex&profile=party');
    expect(badCommand.status).toBe(400);
  });

  it('serves Muse Sign in again as a device-code job only with CCS_MUSE_SIGNIN=on', async () => {
    fs.writeFileSync(
      path.join(ccsDir, 'account-usage-sources.json'),
      JSON.stringify({
        version: 1,
        sources: [{ provider: 'muse', platform: 'mac', sshHost: 'jared-mac' }],
      })
    );
    const off = await fixture();
    const refused = await off.request('POST', '/muse:usage/signin-again', {});
    expect([refused.status, refused.body.code]).toEqual([409, 'not_implemented']);
    const on = await fixture({ museEnabled: true });
    const started = await on.request('POST', '/muse:usage/signin-again', {});
    expect(started.status).toBe(202);
    expect(started.body.job).toMatchObject({
      provider: 'muse',
      kind: 'device-code',
      mode: 'signin-again',
      accountId: 'muse:usage',
      platform: 'mac',
      state: 'starting',
    });
    const again = await on.request('POST', '/muse:usage/signin-again', {});
    expect([again.status, again.body.code, again.body.jobId]).toEqual([
      409,
      'job_running',
      (started.body.job as { id: string }).id,
    ]);
    const plain = await on.request('POST', '/muse:usage/signin-again', {}, PLAIN);
    expect([plain.status, plain.body.code]).toEqual([403, 'secure_transport_required']);
    expect(plain.body.fallback).toEqual({ kind: 'terminal', host: 'mac', command: 'muse login' });
  });

  it('adds Cursor or Qwen back only at zero accounts, on their configured host', async () => {
    const f = await fixture();
    const single = await f.request('POST', '/add', { provider: 'cursor' });
    expect([single.status, single.body.code]).toEqual([409, 'single_account_provider']);
    const token = (
      (await f.request('POST', '/cursor:usage/remove', {})).body.confirmation as {
        token: string;
      }
    ).token;
    expect(
      (await f.request('POST', '/cursor:usage/remove', { confirmationToken: token })).status
    ).toBe(200);
    const added = await f.request('POST', '/add', { provider: 'cursor' });
    expect(added.status).toBe(201);
    expect(added.body.account).toMatchObject({
      id: 'cursor:usage',
      platform: 'mac',
      lifecycle: { state: 'pending_sign_in', jobId: null },
      credential: { kind: 'discover' },
    });
    const registry = JSON.parse(
      fs.readFileSync(path.join(ccsDir, 'account-usage-accounts.json'), 'utf8')
    );
    expect(
      registry.accounts.find((entry: { id: string }) => entry.id === 'cursor:usage')
    ).toMatchObject({
      platform: 'mac',
      sshHost: 'jared-mac',
      createdBy: null,
    });
  });
});

describe('registry, re-check, open, label and trash', () => {
  it('lists providers, accounts with actions and refusals, jobs without codes on plain HTTP', async () => {
    const f = await fixture();
    codexProfile('gmail');
    codexProfile('party');
    new CodexProfileRegistry().setDefault('gmail');
    activate('party');
    await f.request('POST', '/codex:gmail/signin-again', {});
    const secure = await f.request('GET', '/registry');
    expect(secure.status).toBe(200);
    expect((secure.body.providers as unknown[]).length).toBe(9);
    const accounts = secure.body.accounts as Array<Record<string, unknown>>;
    const byId = (id: string) => accounts.find((account) => account.id === id);
    expect(byId('codex:party')).toMatchObject({
      removeRefusal: 'account_active',
      actions: { signInAgain: false, remove: true },
    });
    expect(byId('codex:gmail')).toMatchObject({ removeRefusal: 'account_default' });
    expect(byId('zai:usage')).toMatchObject({
      credential: { kind: 'discover' },
      actions: { replaceKey: false, remove: true, recheck: true },
    });
    expect(byId('cursor:usage')).toMatchObject({ actions: { open: ['mac'], signInAgain: true } });
    expect(byId('muse:usage')).toMatchObject({ actions: { signInAgain: false, recheck: true } });
    const museOn = await fixture({ museEnabled: true });
    const museRegistry = await museOn.request('GET', '/registry');
    const museAccounts = museRegistry.body.accounts as Array<Record<string, unknown>>;
    expect(museAccounts.find((account) => account.id === 'muse:usage')).toMatchObject({
      actions: { signInAgain: true, recheck: true },
    });
    expect(
      (museRegistry.body.providers as Array<{ id: string; signIn: { available: boolean } }>).find(
        (entry) => entry.id === 'muse'
      )?.signIn.available
    ).toBe(true);
    expect(byId('claude:party')).toMatchObject({ actions: { remove: false, open: ['mac'] } });
    expect(byId('plan-opencode-go-console-mac-0123456789ab')).toMatchObject({
      actions: { signInAgain: true, remove: true },
    });
    expect(secure.body.trash).toEqual([]);
    expect((secure.body.jobs as unknown[]).length).toBe(1);
    const plain = await f.request('GET', '/registry', undefined, PLAIN);
    const providers = plain.body.providers as Array<{
      id: string;
      signIn: { unavailableReason: string };
    }>;
    expect(providers.find((entry) => entry.id === 'zai')?.signIn.unavailableReason).toBe(
      'secure_transport_required'
    );
    expect((plain.body.jobs as Array<{ verification: unknown }>)[0].verification).toBeNull();
  });

  it('re-checks one account at most once per 10 s', async () => {
    const f = await fixture();
    const first = await f.request('POST', '/zai:usage/recheck', {});
    expect(first.status).toBe(200);
    expect(first.body.account).toMatchObject({ id: 'zai:usage', status: 'cached' });
    const second = await f.request('POST', '/zai:usage/recheck', {});
    expect([second.status, second.body.code]).toEqual([429, 'rate_limited']);
    expect(second.headers.get('retry-after')).toBe('10');
    f.advance(10_000);
    expect((await f.request('POST', '/zai:usage/recheck', {})).status).toBe(200);
    const codex = await f.request('POST', '/codex:nobody/recheck', {});
    expect(codex.status).toBe(404);
  });

  it('opens Cursor on the Mac only, and relabels additional accounts', async () => {
    const f = await fixture();
    expect((await f.request('POST', '/cursor:usage/open', { platform: 'mac' })).body).toEqual({
      opened: true,
    });
    expect(f.opened).toEqual(['jared-mac']);
    const windows = await f.request('POST', '/cursor:usage/open', { platform: 'windows' });
    expect([windows.status, windows.body.code]).toEqual([409, 'not_configured']);
    const label = await f.request('PATCH', '/zai:usage', { label: 'Work' });
    expect(label.body.account).toMatchObject({ id: 'zai:usage', label: 'Work' });
    const bad = await f.request('PATCH', '/zai:usage', { label: 'bad\u202Elabel' });
    expect([bad.status, bad.body.code]).toEqual([400, 'invalid_body']);
    expect((await f.request('PATCH', '/zai:usage', { label: null })).body.account).toMatchObject({
      label: 'zai',
    });
  });

  it('keeps Claude add, remove, restore and purge off until host steps are enabled', async () => {
    const off = await fixture();
    const add = await off.request('POST', '/add', { provider: 'claude', profileId: 'work2', email: 'work2@example.com' });
    expect([add.status, add.body.code]).toEqual([409, 'not_implemented']);
    const restore = await off.request('POST', '/trash/tr_0123456789abcdef/restore', {});
    expect([restore.status, restore.body.code]).toEqual([409, 'not_implemented']);
    const purge = await off.request('POST', '/trash/tr_0123456789abcdef/purge', {});
    expect([purge.status, purge.body.code]).toEqual([409, 'not_implemented']);
  });

  it("removes a computer's default Claude profile only after its account email is typed", async () => {
    const inventory = {
      version: 1,
      profiles: [
        {
          id: 'home',
          email: 'home@example.com',
          mac: {
            launcherName: 'Claude',
            launcherPath: '/Applications/Claude.app',
            profilePath: '/fake/mac/Claude',
            isDefault: true,
            sshHost: 'jared-mac',
          },
          windows: {
            launcherName: 'h',
            profilePath: 'C:\\x\\Claude-home',
            sshHost: 'jared-windows',
          },
        },
      ],
    };
    const f = await fixture({ claudeEnabled: true });
    fs.writeFileSync(path.join(ccsDir, 'claude-desktop-profiles.json'), JSON.stringify(inventory));
    const asked = await f.request('POST', '/claude:home/remove', {});
    expect(asked.status).toBe(200);
    expect(asked.body.confirmation.expectsTyped).toBe('email');
    expect(asked.body.confirmation.effects.join(' ')).toContain('default Claude profile');
    const token = asked.body.confirmation.token;
    // no email, a wrong email and a bare token all refuse without consuming the token
    expect(
      (await f.request('POST', '/claude:home/remove', { confirmationToken: token })).status
    ).toBe(400);
    const mistyped = await f.request('POST', '/claude:home/remove', {
      confirmationToken: token,
      confirm: 'someone-else@example.com',
    });
    expect([mistyped.status, mistyped.body.code]).toEqual([400, 'invalid_body']);
    // the reviewed email, case-insensitively, removes into the trash like any profile
    const removed = await f.request('POST', '/claude:home/remove', {
      confirmationToken: token,
      confirm: '  HOME@example.com ',
    });
    expect(removed.status).toBe(200);
    expect(removed.body.removed).toBe(true);
    expect(removed.body.trashId).toBeTruthy();
  });

  it('keeps a default Claude profile without an email protected', async () => {
    const f = await fixture({ claudeEnabled: true });
    fs.writeFileSync(
      path.join(ccsDir, 'claude-desktop-profiles.json'),
      JSON.stringify({
        version: 1,
        profiles: [
          {
            id: 'home',
            email: null,
            mac: {
              launcherName: 'Claude',
              launcherPath: '/Applications/Claude.app',
              profilePath: '/fake/mac/Claude',
              isDefault: true,
              sshHost: 'jared-mac',
            },
          },
        ],
      })
    );
    const refused = await f.request('POST', '/claude:home/remove', {});
    expect([refused.status, refused.body.code]).toEqual([409, 'account_protected']);
    expect(f.audits).toContainEqual([
      'accounts.remove.refused',
      { provider: 'claude', code: 'account_protected' },
    ]);
  });

  it('adds, removes into the trash and restores a Claude profile with fake hosts', async () => {
    const f = await fixture({ claudeEnabled: true });
    fs.writeFileSync(
      path.join(ccsDir, 'claude-desktop-profiles.json'),
      JSON.stringify({
        version: 1,
        profiles: [
          {
            id: 'party',
            email: 'party@example.com',
            mac: {
              launcherName: 'p',
              launcherPath: '/a',
              profilePath: '/fake/mac/Claude-party',
              sshHost: 'jared-mac',
            },
            windows: {
              launcherName: 'p',
              profilePath: 'C:\\x\\Claude-party',
              sshHost: 'jared-windows',
            },
          },
        ],
      })
    );
    const added = await f.request('POST', '/add', {
      provider: 'claude',
      profileId: 'work2',
      label: 'Work 2',
      email: 'work2@example.com',
    });
    expect(added.status).toBe(201);
    expect(added.body).toMatchObject({
      account: { id: 'claude:work2', label: 'Work 2', lifecycle: { state: 'pending_sign_in' } },
      launchers: { mac: 'created', windows: 'created' },
    });
    const prepared = (await f.request('POST', '/claude:party/remove', {})).body.confirmation as {
      token: string;
      effects: string[];
    };
    expect(prepared.effects[0]).toContain('trash on Mac and Windows for 30 days');
    const removed = await f.request('POST', '/claude:party/remove', {
      confirmationToken: prepared.token,
    });
    expect(removed.body).toMatchObject({ removed: true, purgeAfter: '2026-11-01T08:00:00Z' });
    const trashId = String(removed.body.trashId);
    expect((await f.request('GET', '/trash')).body.entries).toEqual([
      expect.objectContaining({
        trashId,
        provider: 'claude',
        label: 'party@example.com',
        state: 'trashed',
      }),
    ]);
    const restorePrepared = (await f.request('POST', `/trash/${trashId}/restore`, {})).body
      .confirmation as { token: string };
    const restored = await f.request('POST', `/trash/${trashId}/restore`, {
      confirmationToken: restorePrepared.token,
    });
    expect(restored.body).toEqual({ restored: true, accountId: 'claude:party' });
    expect(f.audits.map(([event]) => event)).toEqual([
      'accounts.add',
      'accounts.remove',
      'accounts.trash.restore',
    ]);
  });

  it('re-checks a pending Claude profile against the host it was opened on', async () => {
    const f = await fixture({ claudeEnabled: true });
    fs.writeFileSync(
      path.join(ccsDir, 'claude-desktop-profiles.json'),
      JSON.stringify({
        version: 1,
        profiles: [
          {
            id: 'party',
            email: 'party@example.com',
            mac: {
              launcherName: 'p',
              launcherPath: '/a',
              profilePath: '/fake/mac/Claude-party',
              sshHost: 'jared-mac',
            },
            windows: {
              launcherName: 'p',
              profilePath: 'C:\\x\\Claude-party',
              sshHost: 'jared-windows',
            },
          },
        ],
      })
    );
    const added = await f.request('POST', '/add', {
      provider: 'claude',
      profileId: 'work2',
      email: 'work2@example.com',
    });
    expect(added.status).toBe(201);
    // The fake host reports no session: still pending, with its assertion kept.
    const waiting = await f.request('POST', '/claude:work2/recheck', { platform: 'mac' });
    expect(waiting.status).toBe(200);
    expect(waiting.body.account).toMatchObject({
      id: 'claude:work2',
      status: 'needs_sign_in',
      email: 'work2@example.com',
    });
    expect((await f.request('POST', '/claude:work2/recheck', {})).status).toBe(400);
    expect(
      (await f.request('POST', '/claude:work2/recheck', { platform: 'mac', email: 'nope' }))
        .status
    ).toBe(400);
    expect((await f.request('POST', '/claude:nobody/recheck', { platform: 'mac' })).status).toBe(
      404
    );
    // A listed id is already signed in, so it answers found.
    const listed = await f.request('POST', '/claude:party/recheck', { platform: 'mac' });
    expect(listed.status).toBe(200);
    expect(listed.body.account).toMatchObject({ id: 'claude:party', status: 'ok' });
  });

  it('purges one trash entry now with a typed DELETE confirmation', async () => {
    const f = await fixture({ claudeEnabled: true });
    fs.writeFileSync(
      path.join(ccsDir, 'claude-desktop-profiles.json'),
      JSON.stringify({
        version: 1,
        profiles: [
          {
            id: 'party',
            email: 'party@example.com',
            mac: {
              launcherName: 'p',
              launcherPath: '/a',
              profilePath: '/fake/mac/Claude-party',
              sshHost: 'jared-mac',
            },
            windows: {
              launcherName: 'p',
              profilePath: 'C:\\x\\Claude-party',
              sshHost: 'jared-windows',
            },
          },
        ],
      })
    );
    const prepared = (await f.request('POST', '/claude:party/remove', {})).body.confirmation as {
      token: string;
    };
    const removed = await f.request('POST', '/claude:party/remove', {
      confirmationToken: prepared.token,
    });
    const trashId = String(removed.body.trashId);
    const ask = await f.request('POST', `/trash/${trashId}/purge`, {});
    expect(ask.status).toBe(200);
    const confirmation = ask.body.confirmation as {
      token: string;
      effects: string[];
      expectsTyped: string;
    };
    expect(confirmation.effects).toEqual([
      'Its Claude data is deleted for good on Mac and Windows.',
      'This cannot be undone. Type DELETE to confirm.',
    ]);
    expect(confirmation.expectsTyped).toBe('DELETE');
    // A mistyped confirmation keeps the token for a retry.
    const mistyped = await f.request('POST', `/trash/${trashId}/purge`, {
      confirmationToken: confirmation.token,
      confirm: 'delete',
    });
    expect([mistyped.status, mistyped.body.code]).toEqual([400, 'invalid_body']);
    const purged = await f.request('POST', `/trash/${trashId}/purge`, {
      confirmationToken: confirmation.token,
      confirm: 'DELETE',
    });
    expect(purged.body).toEqual({ purged: true, trashId });
    expect((await f.request('GET', '/trash')).body.entries).toEqual([]);
    const again = await f.request('POST', `/trash/${trashId}/purge`, {
      confirmationToken: confirmation.token,
      confirm: 'DELETE',
    });
    expect([again.status, again.body.code]).toEqual([404, 'unknown_trash']);
    expect(f.audits.map(([event]) => event)).toContain('accounts.trash.purge');
  });

  it('purge answers 404 for a malformed or missing trash id', async () => {
    const f = await fixture({ claudeEnabled: true });
    const malformed = await f.request('POST', '/trash/nope/purge', {});
    expect([malformed.status, malformed.body.code]).toEqual([400, 'invalid_account']);
    const missing = await f.request('POST', '/trash/tr_0123456789abcdef/purge', {});
    expect([missing.status, missing.body.code]).toEqual([404, 'unknown_trash']);
  });
});

describe('Antigravity profiles', () => {
  it('lists saved profiles with Sign in again, Remove and the live-login refusal', async () => {
    const f = await fixture();
    await agyProfile('gmail');
    await agyProfile('party');
    const registry = new AntigravityProfileRegistry(ccsDir);
    await registry.withLock(async () =>
      registry.completeTransaction('gmail', new Date().toISOString())
    );
    const listing = await f.request('GET', '/registry');
    const accounts = listing.body.accounts as Array<Record<string, unknown>>;
    const byId = (id: string) => accounts.find((account) => account.id === id);
    expect(byId('antigravity:profile:gmail')).toMatchObject({
      provider: 'antigravity',
      credential: null,
      removeRefusal: 'account_active',
      actions: { signInAgain: false, replaceKey: false, remove: true, open: [], recheck: false },
    });
    expect(byId('antigravity:profile:party')).toMatchObject({
      removeRefusal: null,
      actions: { signInAgain: true, remove: true },
    });
    const providers = listing.body.providers as Array<{
      id: string;
      signIn: { available: boolean; unavailableReason: string | null };
      capabilities: { remove: boolean };
    }>;
    expect(providers.find((entry) => entry.id === 'antigravity')).toMatchObject({
      signIn: { available: false, unavailableReason: 'preflight_failed' },
      capabilities: { remove: true },
    });
  });

  it('removes a saved snapshot with a confirmation and keeps the live login, history and others', async () => {
    const f = await fixture();
    await agyProfile('gmail');
    await agyProfile('party');
    agyLive('gmail');
    const prepared = await f.request('POST', '/antigravity:profile:party/remove', {});
    expect(prepared.status).toBe(200);
    const confirmation = prepared.body.confirmation as { token: string; effects: string[] };
    expect(confirmation.effects).toEqual([
      'The saved Antigravity login of this profile is deleted from Ubuntu.',
      'The live Antigravity login, its history and the other profiles are not changed.',
    ]);
    const committed = await f.request('POST', '/antigravity:profile:party/remove', {
      confirmationToken: confirmation.token,
    });
    expect([committed.status, committed.body]).toEqual([
      200,
      { removed: true, trashId: null, purgeAfter: null },
    ]);
    expect(new AntigravityProfileRegistry(ccsDir).listProfiles().map((p) => p.id)).toEqual([
      'gmail',
    ]);
    expect(fs.existsSync(path.join(ccsDir, 'antigravity-instances', 'party'))).toBe(false);
    expect(f.audits).toContainEqual([
      'accounts.remove',
      { provider: 'antigravity', kind: 'supervised-cli', trashed: false },
    ]);
    expect(f.changes()).toBe(1);
  });

  it('refuses the live native login at prepare and at commit, and refuses when it cannot be checked', async () => {
    const f = await fixture();
    await agyProfile('gmail');
    await agyProfile('party');
    agyLive(null);
    const prepared = await f.request('POST', '/antigravity:profile:gmail/remove', {});
    expect(prepared.status).toBe(200);
    agyLive('gmail');
    const committed = await f.request('POST', '/antigravity:profile:gmail/remove', {
      confirmationToken: (prepared.body.confirmation as { token: string }).token,
    });
    expect([committed.status, committed.body.code]).toEqual([409, 'account_active']);
    const again = await f.request('POST', '/antigravity:profile:gmail/remove', {});
    expect([again.status, again.body.code]).toEqual([409, 'account_active']);
    agyNativeFails = true;
    const unknown = await f.request('POST', '/antigravity:profile:party/remove', {});
    expect([unknown.status, unknown.body.code]).toEqual([500, 'remove_failed']);
    expect(new AntigravityProfileRegistry(ccsDir).listProfiles().length).toBe(2);
    expect(f.audits).toContainEqual([
      'accounts.remove.refused',
      { provider: 'antigravity', code: 'account_active' },
    ]);
  });

  it('refuses Remove while a switch holds the registry lock', async () => {
    const f = await fixture();
    await agyProfile('gmail');
    await agyProfile('party');
    const lock = path.join(ccsDir, 'antigravity-profiles', '.transaction-lock');
    fs.mkdirSync(lock, { mode: 0o700 });
    const refused = await f.request('POST', '/antigravity:profile:party/remove', {});
    expect([refused.status, refused.body.code]).toEqual([409, 'activation_running']);
  });

  it('answers Add with the terminal command after its refusals, also on plain HTTP', async () => {
    const f = await fixture();
    await agyProfile('gmail');
    const invalid = await f.request('POST', '/add', {
      provider: 'antigravity',
      profileName: 'A b',
    });
    expect([invalid.status, invalid.body.code]).toEqual([400, 'invalid_body']);
    const extra = await f.request('POST', '/add', {
      provider: 'antigravity',
      profileName: 'party',
      key: 'x',
    });
    expect(extra.status).toBe(400);
    const taken = await f.request('POST', '/add', {
      provider: 'antigravity',
      profileName: 'gmail',
    });
    expect([taken.status, taken.body.code]).toEqual([409, 'id_in_use']);
    for (const headers of [{}, PLAIN]) {
      const add = await f.request(
        'POST',
        '/add',
        { provider: 'antigravity', profileName: 'party' },
        headers
      );
      expect([add.status, add.body.code, add.body.fallback]).toEqual([
        409,
        'preflight_failed',
        {
          kind: 'terminal',
          host: 'ubuntu',
          command: 'ai-account-center antigravity signin party',
        },
      ]);
      expect(add.body.error).toBe(
        'Sign in to this provider from a terminal on Ubuntu with the command shown.'
      );
    }
    const missing = await fixture({ antigravityFlow: 'tool_missing' });
    const noCli = await missing.request('POST', '/add', {
      provider: 'antigravity',
      profileName: 'party',
    });
    expect([noCli.status, noCli.body.code, noCli.body.fallback]).toEqual([
      409,
      'tool_missing',
      undefined,
    ]);
  });

  it('answers Sign in again with the terminal command, never for the live login', async () => {
    const f = await fixture();
    await agyProfile('gmail');
    await agyProfile('party');
    agyLive('gmail');
    const live = await f.request('POST', '/antigravity:profile:gmail/signin-again', {});
    expect([live.status, live.body.code]).toEqual([409, 'account_active']);
    const other = await f.request('POST', '/antigravity:profile:party/signin-again', {});
    expect([other.status, other.body.code, other.body.fallback]).toEqual([
      409,
      'preflight_failed',
      { kind: 'terminal', host: 'ubuntu', command: 'ai-account-center antigravity signin party' },
    ]);
    const unknown = await f.request('POST', '/antigravity:profile:nobody/signin-again', {});
    expect([unknown.status, unknown.body.code]).toEqual([404, 'unknown_account']);
  });

  it('starts a supervised Antigravity job for Add and Sign in again when the flow is available', async () => {
    const f = await fixture({ antigravityFlow: null });
    const add = await f.request('POST', '/add', { provider: 'antigravity', profileName: 'party' });
    expect(add.status).toBe(202);
    expect(add.body.job).toMatchObject({
      provider: 'antigravity',
      kind: 'supervised-cli',
      mode: 'add',
      state: 'starting',
    });
    // One Antigravity sign-in at a time.
    const second = await f.request('POST', '/add', { provider: 'antigravity', profileName: 'work' });
    expect([second.status, second.body.code]).toEqual([409, 'job_running']);

    // On plain HTTP the pasted code cannot cross: 403 with the terminal fallback.
    const insecure = await f.request(
      'POST',
      '/add',
      { provider: 'antigravity', profileName: 'solo' },
      PLAIN
    );
    expect(insecure.status).toBe(403);
    expect(insecure.body).toMatchObject({
      code: 'secure_transport_required',
      fallback: {
        kind: 'terminal',
        host: 'ubuntu',
        command: 'ai-account-center antigravity signin solo',
      },
    });

    // Sign in again: the live login is refused, another saved profile starts a job.
    const g = await fixture({ antigravityFlow: null });
    await agyProfile('gmail');
    await agyProfile('party');
    agyLive('party');
    const live = await g.request('POST', '/antigravity:profile:party/signin-again', {});
    expect([live.status, live.body.code]).toEqual([409, 'account_active']);
    const again = await g.request('POST', '/antigravity:profile:gmail/signin-again', {});
    expect(again.status).toBe(202);
    expect(again.body.job).toMatchObject({
      provider: 'antigravity',
      kind: 'supervised-cli',
      mode: 'signin-again',
      accountId: 'antigravity:profile:gmail',
      state: 'starting',
    });
  });

  it('refuses Sign in again and Remove while a terminal sign-in for the profile runs', async () => {
    const f = await fixture();
    await agyProfile('gmail');
    await agyProfile('party');
    agyLive('gmail');
    const marker = claimAntigravitySignInMarker(ccsDir, 'party');
    expect(marker).not.toBeNull();
    const again = await f.request('POST', '/antigravity:profile:party/signin-again', {});
    expect([again.status, again.body.code]).toEqual([409, 'signin_running']);
    const removal = await f.request('POST', '/antigravity:profile:party/remove', {});
    expect([removal.status, removal.body.code]).toEqual([409, 'signin_running']);
    marker!.release();
    const prepared = await f.request('POST', '/antigravity:profile:party/remove', {});
    expect(prepared.status).toBe(200);
  });

  it('records Remove files left for review in the audit log', async () => {
    const f = await fixture();
    await agyProfile('gmail');
    await agyProfile('party');
    agyLive('gmail');
    const foreign = path.join(ccsDir, 'antigravity-instances', 'party', 'ubuntu', 'notes.txt');
    fs.writeFileSync(foreign, 'kept', { mode: 0o600 });
    const prepared = await f.request('POST', '/antigravity:profile:party/remove', {});
    const committed = await f.request('POST', '/antigravity:profile:party/remove', {
      confirmationToken: (prepared.body.confirmation as { token: string }).token,
    });
    expect(committed.status).toBe(200);
    expect(fs.readFileSync(foreign, 'utf8')).toBe('kept');
    expect(f.audits).toContainEqual([
      'accounts.remove.left_in_place',
      { provider: 'antigravity', count: 1 },
    ]);
  });

  it('serves the terminal command only for a valid profile name and a browser session', async () => {
    const f = await fixture();
    const bad = await f.request('GET', '/signin-command?provider=antigravity&profile=Bad%20Name');
    expect(bad.status).toBe(400);
    const extra = await f.request('GET', '/signin-command?provider=antigravity&profile=party&x=1');
    expect(extra.status).toBe(400);
    const device = await f.request(
      'GET',
      '/signin-command?provider=antigravity&profile=party',
      undefined,
      { 'x-test-device': 'true' }
    );
    expect([device.status, device.body.code]).toEqual([403, 'device_scope']);
  });
});
