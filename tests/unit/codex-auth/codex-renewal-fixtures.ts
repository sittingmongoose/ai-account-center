/**
 * Fixtures for saved-login renewal tests: fake JWTs and refresh tokens only, a
 * temporary CCS_HOME, and injected Codex homes, home folder and process list.
 * Nothing here reads the real ~/.ccs, ~/.codex or /proc.
 */
import { createHash } from 'crypto';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodexProfileRegistry } from '../../../src/codex-auth/codex-profile-registry';
import { resolveCodexProfileDir } from '../../../src/codex-auth/codex-profile-paths';
import { invalidateCodexAuthProfilesCache } from '../../../src/codex-auth/codex-auth-dashboard-service';
import type { CodexProcessSnapshot } from '../../../src/codex-auth/codex-activation-runtime';
import type { CodexTokenFetch } from '../../../src/codex-auth/codex-token-refresh';
import type { CodexProfileRenewalOptions } from '../../../src/codex-auth/codex-profile-renewal';

export const NOW = Date.parse('2026-10-08T12:00:00.000Z');
export const DAY = 86_400_000;

export interface Account {
  email: string;
  accountId: string;
  userId: string;
}

export const ALPHA: Account = {
  email: 'alpha@example.test',
  accountId: 'acct-alpha',
  userId: 'user-alpha',
};
export const BRAVO: Account = {
  email: 'bravo@example.test',
  accountId: 'acct-bravo',
  userId: 'user-bravo',
};
export const CHARLIE: Account = {
  email: 'charlie@example.test',
  accountId: 'acct-charlie',
  userId: 'user-charlie',
};

export interface LoginSpec {
  account: Account;
  /** Family id carried as session_id (access) and sid (id token); null omits both. */
  session?: string | null;
  /** Original sign-in, seconds; null omits it. */
  authTime?: number | null;
  /** Access-token expiry in ms; null omits exp. */
  accessExp?: number | null;
  refresh: string;
  /** Unique per token generation; marks the fake signatures. */
  label: string;
  lastRefresh?: string;
  /** Workspace claimed by the access token (defaults to the account's). */
  accessAccountId?: string;
}

/** Every fake credential string handed out, for leak scans. */
export const issuedSecrets = new Set<string>();

function b64(value: unknown): string {
  return Buffer.from(JSON.stringify(value)).toString('base64url');
}

function jwt(payload: Record<string, unknown>, label: string): string {
  const signature = Buffer.from(`FAKE-SIG-${label}`).toString('base64url');
  issuedSecrets.add(signature);
  const token = `${b64({ alg: 'RS256', typ: 'JWT' })}.${b64(payload)}.${signature}`;
  issuedSecrets.add(token);
  return token;
}

export function makeTokens(spec: LoginSpec) {
  const session = spec.session === undefined ? `sess-${spec.label}` : spec.session;
  const authTime = spec.authTime === undefined ? 1_780_000_000 : spec.authTime;
  const iat = Math.floor(NOW / 1000) - 3600;
  const auth = {
    chatgpt_account_id: spec.accessAccountId ?? spec.account.accountId,
    chatgpt_user_id: spec.account.userId,
  };
  const id_token = jwt(
    {
      email: spec.account.email,
      iss: 'https://auth.openai.com',
      sub: spec.account.userId,
      iat,
      exp: iat + 3600,
      ...(session !== null ? { sid: session } : {}),
      ...(authTime !== null ? { auth_time: authTime } : {}),
      'https://api.openai.com/auth': {
        chatgpt_account_id: spec.account.accountId,
        chatgpt_user_id: spec.account.userId,
        chatgpt_plan_type: 'pro',
      },
    },
    `id-${spec.label}`
  );
  const accessExp = spec.accessExp === undefined ? NOW + 7 * DAY : spec.accessExp;
  const access_token = jwt(
    {
      iat,
      ...(accessExp !== null ? { exp: Math.floor(accessExp / 1000) } : {}),
      ...(session !== null ? { session_id: session } : {}),
      'https://api.openai.com/auth': auth,
    },
    `access-${spec.label}`
  );
  issuedSecrets.add(spec.refresh);
  issuedSecrets.add(createHash('sha256').update(spec.refresh).digest('hex'));
  return {
    id_token,
    access_token,
    refresh_token: spec.refresh,
    account_id: spec.account.accountId,
  };
}

export function loginJson(spec: LoginSpec): Record<string, unknown> {
  return {
    OPENAI_API_KEY: null,
    auth_mode: 'chatgpt',
    last_refresh: spec.lastRefresh ?? new Date(NOW - 3 * DAY).toISOString(),
    tokens: makeTokens(spec),
    unknown_extra: { kept: true },
  };
}

export function loginBuffer(spec: LoginSpec): Buffer {
  return Buffer.from(JSON.stringify(loginJson(spec), null, 2));
}

export interface FakeCall {
  url: string;
  body: Record<string, unknown>;
}

export type FakeResponse = { status: number; body: unknown } | Error;

export function fakeFetch(respond: (call: FakeCall) => FakeResponse | Promise<FakeResponse>): {
  fetch: CodexTokenFetch;
  calls: FakeCall[];
} {
  const calls: FakeCall[] = [];
  const fetch: CodexTokenFetch = async (url, init) => {
    const call = { url, body: JSON.parse(init.body) as Record<string, unknown> };
    calls.push(call);
    const response = await respond(call);
    if (response instanceof Error) throw response;
    const text = typeof response.body === 'string' ? response.body : JSON.stringify(response.body);
    return { status: response.status, text: async () => text };
  };
  return { fetch, calls };
}

/** A successful refresh for `spec`'s account with a rotated refresh token. */
export function refreshedBody(spec: LoginSpec, include = { id: true, refresh: true }) {
  const tokens = makeTokens(spec);
  return {
    ...(include.id ? { id_token: tokens.id_token } : {}),
    access_token: tokens.access_token,
    ...(include.refresh ? { refresh_token: tokens.refresh_token } : {}),
  };
}

export function codexProcess(home: string | null, pid = 4242): CodexProcessSnapshot {
  return {
    pid,
    ppid: 1,
    startTime: '100',
    state: 'S',
    args: ['/usr/local/bin/codex', 'app-server'],
    exe: '/usr/local/bin/codex',
    cwd: '/',
    env: home ? { CODEX_HOME: home } : { HOME: '/nonexistent-home' },
  };
}

export interface LogLine {
  level: string;
  event: string;
  message: string;
  context: Record<string, unknown>;
}

const savedEnv = ['CCS_HOME', 'CCS_DIR', 'CODEX_HOME', 'CCS_CODEX_PROFILE', 'CCS_CODEX_RENEWAL'];

export class RenewalSandbox {
  readonly root: string;
  readonly codexHome: string;
  readonly homeDir: string;
  readonly registry: CodexProfileRegistry;
  readonly logs: LogLine[] = [];
  processes: CodexProcessSnapshot[] = [];
  clock = NOW;
  private readonly previous: Record<string, string | undefined> = {};

  constructor() {
    for (const key of savedEnv) this.previous[key] = process.env[key];
    for (const key of savedEnv) delete process.env[key];
    this.root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-renewal-'));
    process.env.CCS_HOME = this.root;
    this.codexHome = path.join(this.root, 'shared-codex');
    this.homeDir = path.join(this.root, 'home');
    fs.mkdirSync(this.codexHome, { recursive: true });
    fs.mkdirSync(this.homeDir, { recursive: true });
    this.registry = new CodexProfileRegistry();
    invalidateCodexAuthProfilesCache();
  }

  get ccsDir(): string {
    return path.join(this.root, '.ccs');
  }

  get statusPath(): string {
    return path.join(this.ccsDir, 'codex-renewal-status.json');
  }

  profileAuth(name: string): string {
    return path.join(resolveCodexProfileDir(name), 'auth.json');
  }

  addProfile(name: string, login: LoginSpec | Buffer): void {
    this.writeProfile(name, login);
    this.registry.createProfile(name);
  }

  writeProfile(name: string, login: LoginSpec | Buffer): void {
    fs.mkdirSync(resolveCodexProfileDir(name), { recursive: true, mode: 0o700 });
    const file = this.profileAuth(name);
    const temporary = `${file}.fixture`;
    fs.writeFileSync(temporary, Buffer.isBuffer(login) ? login : loginBuffer(login), {
      mode: 0o600,
    });
    fs.renameSync(temporary, file);
  }

  writeLive(login: LoginSpec | Buffer | string): void {
    fs.writeFileSync(
      path.join(this.codexHome, 'auth.json'),
      typeof login === 'string' || Buffer.isBuffer(login) ? login : loginBuffer(login),
      { mode: 0o600 }
    );
  }

  /** A Codex auth file under the injected home folder, such as `.codex-t3/gmail`. */
  writeHome(relativeHome: string, login: LoginSpec | string): string {
    const home = path.join(this.homeDir, relativeHome);
    fs.mkdirSync(home, { recursive: true });
    fs.writeFileSync(
      path.join(home, 'auth.json'),
      typeof login === 'string' ? login : loginBuffer(login),
      { mode: 0o600 }
    );
    return home;
  }

  options(extra: Partial<CodexProfileRenewalOptions> = {}): CodexProfileRenewalOptions {
    return {
      codexHome: this.codexHome,
      homeDir: this.homeDir,
      scanProcesses: () => this.processes,
      now: () => this.clock,
      random: () => 0.5,
      sleep: async () => undefined,
      log: (level, event, message, context) => this.logs.push({ level, event, message, context }),
      env: {},
      ...extra,
    };
  }

  /** Every regular file under the sandbox with its content, for change detection. */
  files(): Map<string, Buffer> {
    const result = new Map<string, Buffer>();
    const walk = (directory: string) => {
      for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
        const full = path.join(directory, entry.name);
        if (entry.isDirectory()) walk(full);
        else if (entry.isFile()) result.set(full, fs.readFileSync(full));
      }
    };
    walk(this.root);
    return result;
  }

  cleanup(): void {
    fs.rmSync(this.root, { recursive: true, force: true });
    for (const key of savedEnv) {
      if (this.previous[key] === undefined) delete process.env[key];
      else process.env[key] = this.previous[key];
    }
    invalidateCodexAuthProfilesCache();
  }
}

/** Fails when any issued fake credential (or its digest) appears in `value`. */
export function expectNoSecrets(value: unknown): void {
  const text = typeof value === 'string' ? value : JSON.stringify(value);
  for (const secret of issuedSecrets) {
    if (text.includes(secret)) {
      throw new Error(`fake credential leaked into output (length ${secret.length})`);
    }
  }
}
