import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import { execFileSync } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { CodexQuotaResult } from '../../../src/cliproxy/quota/quota-types';
import type { DashboardAccount } from '../../../src/web-server/services/account-dashboard-types';
import {
  codexUsageIdentity,
  codexUsageTarget,
  peekCodexUsage,
  recoverCodexUsage,
  type CodexUsageRecovery,
} from '../../../src/web-server/services/usage-failover-codex';
import {
  getCodexProfileQuotaRows,
  getCodexProfileRetryBlocked,
  resetNativeQuotaState,
} from '../../../src/web-server/usage/native-quota-collector';

const EMAIL = 'fixture@example.com';
const WORKSPACE = 'fixture-workspace';
const PRIVATE_TOKEN = 'fixture-private-access-token';

function jwt(payload: unknown): string {
  return [
    Buffer.from(JSON.stringify({ alg: 'RS256' })).toString('base64url'),
    Buffer.from(JSON.stringify(payload)).toString('base64url'),
    Buffer.from('fixture-signature').toString('base64url'),
  ].join('.');
}

function auth(user = 'fixture-user', token = PRIVATE_TOKEN) {
  return {
    tokens: {
      access_token: token,
      account_id: WORKSPACE,
      id_token: jwt({
        sub: 'fixture-subject',
        iss: 'https://auth.openai.com',
        email: EMAIL,
        'https://api.openai.com/auth': {
          chatgpt_account_id: WORKSPACE,
          chatgpt_user_id: user,
          chatgpt_plan_type: 'pro',
        },
      }),
    },
  };
}

function account(): DashboardAccount {
  const stamp = new Date().toISOString();
  return {
    id: 'codex:saved',
    provider: 'codex',
    providerLabel: 'Codex',
    label: 'Saved work login',
    email: EMAIL,
    plan: 'pro',
    platform: 'ubuntu',
    source: 'Codex saved login on Ubuntu',
    status: 'ok',
    message: null,
    fetchedAt: stamp,
    sampledAt: stamp,
    isActive: true,
    windows: [
      {
        key: 'five_hour',
        label: '5-hour usage',
        usedPercent: 0,
        remainingPercent: 100,
        resetAt: null,
        windowMinutes: 300,
        used: null,
        limit: null,
        unit: null,
      },
    ],
    capabilities: { codexProfile: 'saved', claudeProfileId: null, claudePlatforms: [] },
  };
}

function recovery(): CodexUsageRecovery & {
  recover: ReturnType<typeof mock>;
  peek: ReturnType<typeof mock>;
} {
  return {
    recover: mock(async (_target, row) => row),
    peek: mock((_target, row) => row),
  };
}

describe('Codex account failover auth binding', () => {
  let root: string;
  let ccs: string;
  let file: string;
  let originalCcsHome: string | undefined;
  let originalCcsDir: string | undefined;

  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-codex-failover-'));
    ccs = path.join(root, '.ccs');
    file = path.join(ccs, 'codex-instances', 'saved', 'auth.json');
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, JSON.stringify(auth()));
    originalCcsHome = process.env.CCS_HOME;
    originalCcsDir = process.env.CCS_DIR;
    process.env.CCS_HOME = root;
    delete process.env.CCS_DIR;
    resetNativeQuotaState();
  });

  afterEach(() => {
    resetNativeQuotaState();
    if (originalCcsHome === undefined) delete process.env.CCS_HOME;
    else process.env.CCS_HOME = originalCcsHome;
    if (originalCcsDir === undefined) delete process.env.CCS_DIR;
    else process.env.CCS_DIR = originalCcsDir;
    fs.rmSync(root, { recursive: true, force: true });
    mock.restore();
  });

  it('binds a local saved profile without exposing auth or using a profile name as identity', () => {
    const first = codexUsageTarget(ccs, { name: 'saved' });
    expect(first?.accountId).toBe('codex:saved');
    expect(first?.expectedEmail).toBe(EMAIL);
    expect(first?.primarySources[0]?.sshHost).toBeNull();
    expect(first?.primarySources[0]?.selector).toBe('codex-profile:saved');
    expect(JSON.stringify(first)).not.toContain(PRIVATE_TOKEN);
    expect(JSON.stringify(first)).not.toContain('fixture-user');
    fs.mkdirSync(path.join(ccs, 'codex-instances', 'renamed'));
    fs.copyFileSync(file, path.join(ccs, 'codex-instances', 'renamed', 'auth.json'));
    const renamed = codexUsageTarget(ccs, 'renamed');
    expect(renamed?.expectedIdentity).toEqual(first?.expectedIdentity);
    expect(renamed?.binding).not.toEqual(first?.binding);
  });

  it('matches the actual Python source inventory identity, including non-ASCII principal claims', () => {
    fs.writeFileSync(file, JSON.stringify(auth('fixture-user-\u00e9\ud83d\ude80')));
    const target = codexUsageTarget(ccs, 'saved');
    const output = execFileSync(
      process.platform === 'win32' ? 'python' : 'python3',
      ['-B', path.resolve('scripts/account-usage/failover_usage.py')],
      {
        input: JSON.stringify({
          version: 1,
          action: 'inventory',
          provider: 'codex',
          homeDir: root,
          ccsDir: ccs,
        }),
        encoding: 'utf8',
        timeout: 5000,
        windowsHide: true,
      }
    );
    const result = JSON.parse(output);
    expect(result.candidates).toHaveLength(1);
    expect(result.candidates[0].identity).toEqual(target?.expectedIdentity);
  });

  it('changes the source binding on a same-account token rotation', () => {
    const before = codexUsageTarget(ccs, 'saved');
    fs.writeFileSync(file, JSON.stringify(auth('fixture-user', 'fixture-rotated-token')));
    const after = codexUsageTarget(ccs, 'saved');
    expect(before?.expectedIdentity).toEqual(after?.expectedIdentity);
    expect(before?.binding).not.toEqual(after?.binding);
  });

  it('rejects inconsistent workspace, bearer principal, missing credentials, and path traversal', () => {
    const inconsistent = auth();
    inconsistent.tokens.account_id = 'other-workspace';
    expect(codexUsageIdentity(inconsistent.tokens)).toBeNull();
    const bearerMismatch = auth();
    bearerMismatch.tokens.access_token = jwt({ sub: 'different-person' });
    expect(codexUsageIdentity(bearerMismatch.tokens)).toBeNull();
    expect(codexUsageTarget(ccs, '../saved')).toBeNull();
    fs.unlinkSync(file);
    expect(codexUsageTarget(ccs, 'saved')).toBeNull();
  });

  it('requires person identity as well as workspace when comparing different logins', () => {
    expect(codexUsageIdentity(auth().tokens)).not.toEqual(
      codexUsageIdentity(auth('another-person').tokens)
    );
  });

  it('uses an explicit synthetic home for the bare default', () => {
    const defaultFile = path.join(root, '.codex', 'auth.json');
    fs.mkdirSync(path.dirname(defaultFile));
    fs.writeFileSync(defaultFile, JSON.stringify(auth()));
    const target = codexUsageTarget(ccs, 'default', { homeDir: root });
    expect(target?.accountId).toBe('codex:default');
    expect(target?.primarySources[0]?.selector).toBe('codex-profile:default');
  });

  it('keeps the native primary and activation capabilities when the binding is unchanged', async () => {
    const target = codexUsageTarget(ccs, 'saved');
    const row = account();
    const service = recovery();
    expect(await recoverCodexUsage(target, row, service, () => false)).toBe(row);
    expect(service.recover).toHaveBeenCalledTimes(1);
    expect(peekCodexUsage(target, row, service)).toBe(row);
    expect(service.peek).toHaveBeenCalledTimes(1);
    expect(row.capabilities.codexProfile).toBe('saved');
    expect(row.isActive).toBe(true);
  });

  it('does not seed an old primary result after auth changes during the native read', async () => {
    const target = codexUsageTarget(ccs, 'saved');
    fs.writeFileSync(file, JSON.stringify(auth('another-person')));
    const service = recovery();
    const result = await recoverCodexUsage(target, account(), service, () => false);
    expect(service.recover).not.toHaveBeenCalled();
    expect(result.status).toBe('unavailable');
    expect(result.windows).toEqual([]);
    expect(result.sampledAt).toBeNull();
    expect(result.email).toBeNull();
    expect(result.id).toBe('codex:saved');
    expect(peekCodexUsage(target, account(), service).windows).toEqual([]);
    expect(service.peek).not.toHaveBeenCalled();
  });

  it('discards a peer result if the local login changes while recovering', async () => {
    const target = codexUsageTarget(ccs, 'saved');
    const service = recovery();
    service.recover.mockImplementation(async () => {
      fs.writeFileSync(file, JSON.stringify(auth('another-person')));
      return { ...account(), source: 'Verified quota on Mac (fallback)' };
    });
    const result = await recoverCodexUsage(target, account(), service, () => false);
    expect(result.windows).toEqual([]);
    expect(result.fetchedAt).toBeNull();
  });

  it('also checks auth after a synchronous cached read', () => {
    const target = codexUsageTarget(ccs, 'saved');
    const service = recovery();
    service.peek.mockImplementation(() => {
      fs.unlinkSync(file);
      return account();
    });
    expect(peekCodexUsage(target, account(), service).windows).toEqual([]);
  });

  it('does not recover a different account row with a reused target', async () => {
    const target = codexUsageTarget(ccs, 'saved');
    const service = recovery();
    const other = { ...account(), email: 'another-person@example.com' };
    expect((await recoverCodexUsage(target, other, service, () => false)).windows).toEqual([]);
    expect(service.recover).not.toHaveBeenCalled();
  });

  it('only peeks when native refresh is pending or account retry is blocked', async () => {
    const target = codexUsageTarget(ccs, 'saved');
    const service = recovery();
    const blocked = mock(() => true);
    await recoverCodexUsage(target, account(), service, blocked);
    expect(blocked).toHaveBeenCalledWith('saved');
    expect(service.recover).not.toHaveBeenCalled();
    expect(service.peek).toHaveBeenCalledTimes(1);
  });

  it('preserves a valid native reading if recovery itself fails', async () => {
    const row = account();
    const service = recovery();
    service.recover.mockRejectedValue(new Error('fixture unavailable'));
    expect(await recoverCodexUsage(codexUsageTarget(ccs, 'saved'), row, service, () => false)).toBe(
      row
    );
  });

  it('exposes pending and cooldown state without allowing failover around it', async () => {
    let settle!: (value: CodexQuotaResult) => void;
    const pending = new Promise<CodexQuotaResult>((resolve) => {
      settle = resolve;
    });
    const token = { accessToken: PRIVATE_TOKEN, accountId: WORKSPACE };
    const rows = getCodexProfileQuotaRows(['saved'], {
      readCodexNativeAuth: () => token,
      fetchCodexQuotaWithToken: () => pending,
      defaultCodexProfile: () => 'saved',
    });
    expect(getCodexProfileRetryBlocked('saved')).toBe(true);
    settle({
      success: false,
      windows: [],
      planType: null,
      lastUpdated: Date.now(),
      accountId: WORKSPACE,
      httpStatus: 429,
      errorDetail: 'retry-after:60',
    });
    await rows;
    expect(getCodexProfileRetryBlocked('saved')).toBe(true);
    const clock = spyOn(Date, 'now').mockReturnValue(Date.now() + 61_000);
    expect(getCodexProfileRetryBlocked('saved')).toBe(false);
    clock.mockRestore();
  });

  it('allows another source after a local expired token or network failure', async () => {
    for (const response of [
      { needsReauth: true, httpStatus: 401 },
      { retryable: true, httpStatus: 503 },
    ]) {
      resetNativeQuotaState();
      const fetchQuota = mock(async () => ({
        success: false,
        windows: [],
        planType: null,
        lastUpdated: Date.now(),
        accountId: WORKSPACE,
        ...response,
      }));
      const deps = {
        readCodexNativeAuth: () => ({ accessToken: PRIVATE_TOKEN, accountId: WORKSPACE }),
        fetchCodexQuotaWithToken: fetchQuota,
        defaultCodexProfile: () => 'saved',
      };
      await getCodexProfileQuotaRows(['saved'], deps);
      expect(getCodexProfileRetryBlocked('saved')).toBe(false);
      await getCodexProfileQuotaRows(['saved'], deps, { force: true });
      // The native credential keeps its own cooldown even though peers may recover.
      expect(fetchQuota).toHaveBeenCalledTimes(1);
    }
  });
});
