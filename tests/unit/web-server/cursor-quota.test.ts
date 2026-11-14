import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import { execFile } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  CursorQuotaReader,
  cursorAccountBinary,
  CURSOR_ACCOUNT_TTL_MS,
} from '../../../src/web-server/services/cursor-account-reader';
import { AdditionalAccountService } from '../../../src/web-server/services/additional-account-service';
import { projectAgentUsage } from '../../../src/web-server/agent-usage/agent-usage-projection';
import type { DashboardAccount } from '../../../src/web-server/services/account-dashboard-types';

// Opt-in packaged-reader integration. The only HTTP target is our loopback fake;
// every key is created in the disposable CCS scope through the reader's stdin.
const binary = process.env.AAC_CURSOR_QUOTA_TEST_READER ?? cursorAccountBinary();
const suite = binary ? describe : describe.skip;
let dir: string;
let server: ReturnType<typeof Bun.serve>;
let mode = 'read';
let calls = 0;
let now = Date.parse('2026-11-14T20:00:00Z');
const reset = Date.parse('2026-11-18T19:16:00Z');
const base = {
  id: 'cursor:usage',
  provider: 'cursor',
  providerLabel: 'Cursor',
  label: 'Cursor',
  email: null,
  plan: null,
  platform: 'windows',
  source: 'session',
  status: 'unavailable',
  message: null,
  fetchedAt: null,
  sampledAt: null,
  isActive: false,
  windows: [],
  capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
} as DashboardAccount;
function run(args: string[], input: string): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const child = execFile(
      binary!,
      args,
      {
        env: {
          ...process.env,
          CCS_HOME: dir,
          AAC_CURSOR_API_BASE: `http://127.0.0.1:${server.port}`,
        },
        timeout: 5000,
      },
      (error, stdout) => {
        if (error) reject(new Error('fixture_reader_failed'));
        else {
          try {
            resolve(JSON.parse(stdout));
          } catch {
            reject(new Error('fixture_bad_output'));
          }
        }
      }
    );
    child.stdin!.end(input);
  });
}
const put = (key = 'loopback-test-key-only') => run(['key', 'put', '--ccs-dir', dir], key);
const reader = () => new CursorQuotaReader({ ccsDir: dir, now: () => now, run });
suite('Cursor quota from the key (loopback only)', () => {
  beforeEach(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-cursor-quota-'));
    mode = 'read';
    calls = 0;
    now = Date.parse('2026-11-14T20:00:00Z');
    server = Bun.serve({
      hostname: '127.0.0.1',
      port: 0,
      fetch(request) {
        calls++;
        if (mode === 'rejected') return new Response('{}', { status: 401 });
        if (mode === 'failed') return new Response('{}', { status: 503 });
        if (new URL(request.url).pathname === '/auth/exchange_user_api_key')
          return Response.json({ accessToken: 'fake-token' });
        return Response.json(
          mode === 'malformed'
            ? { planUsage: {} }
            : {
                planUsage: { totalPercentUsed: 0, autoPercentUsed: 12.5 },
                billingCycleEnd: String(reset),
              }
        );
      },
    });
  });
  afterEach(() => {
    server.stop(true);
    fs.rmSync(dir, { recursive: true, force: true });
  });
  it('reads real zero and auto usage with reset, persists, throttles across restarts, and permits manual refresh', async () => {
    await put();
    const first = await reader().read(base);
    expect(first).toMatchObject({
      status: 'ok',
      source: 'Cursor API key',
      windows: [{ usedPercent: 0, resetAt: new Date(reset).toISOString() }, { usedPercent: 12.5 }],
    });
    const published = projectAgentUsage([first!], { now: Date.now() });
    expect(published.accounts[0]).toMatchObject({
      source: 'Cursor API key',
      windows: [{ usedPercent: 0, resetAt: new Date(reset).toISOString() }, { usedPercent: 12.5 }],
    });
    expect(calls).toBe(2);
    await reader().read(base);
    expect(calls).toBe(2);
    await reader().read(base, true);
    expect(calls).toBe(4);
  });
  it('shows rejected key without session fallback or stale quotas', async () => {
    await put();
    const quota = reader();
    await quota.read(base);
    mode = 'rejected';
    const row = await quota.read(base, true);
    expect(row).toMatchObject({
      status: 'needs_sign_in',
      message: 'The key was rejected.',
      sampledAt: null,
      windows: [],
    });
  });
  it('retains the dated last good sample on failed reads, including after restart', async () => {
    await put();
    const first = await reader().read(base);
    mode = 'failed';
    now += CURSOR_ACCOUNT_TTL_MS;
    const row = await reader().read(base);
    expect(row).toMatchObject({
      status: 'cached',
      sampledAt: first!.sampledAt,
      windows: first!.windows,
    });
    expect(row!.message).toContain(first!.sampledAt!);
    expect(projectAgentUsage([row!], { now: Date.now() }).accounts[0]).toMatchObject({
      status: 'cached',
      sampledAt: first!.sampledAt,
    });
    const before = calls;
    await reader().read(base);
    expect(calls).toBe(before);
  });
  it('never creates zero usage from a malformed or failed first read', async () => {
    await put();
    mode = 'malformed';
    expect(await reader().read(base)).toMatchObject({
      status: 'unavailable',
      sampledAt: null,
      windows: [],
    });
  });
  it('does not reuse another key quota and invalidates removed keys', async () => {
    await put();
    const quota = reader();
    await quota.read(base);
    await put('different-loopback-test-key');
    mode = 'failed';
    expect(await quota.read(base)).toMatchObject({ status: 'unavailable', windows: [] });
    await run(['key', 'delete', '--ccs-dir', dir], '');
    expect(await quota.read(base)).toBeNull();
    expect(quota.peek(base)).toBeNull();
  });
  it('uses the old session reader only with no key and feeds configured card rows from the key', async () => {
    let sessionCalls = 0;
    const service = new AdditionalAccountService({
      ccsDir: dir,
      cursorQuota: reader(),
      now: () => now,
      readManifest: async () =>
        JSON.stringify({ version: 1, sources: [{ provider: 'cursor', platform: 'windows' }] }),
      runSource: async (source) => {
        if (source.provider === 'cursor') sessionCalls++;
        return JSON.stringify({
          ...base,
          status: 'ok',
          windows: [{ key: 'old', label: 'Old session', usedPercent: 31 }],
        });
      },
    });
    expect((await service.get()).find((row) => row.provider === 'cursor')!.source).not.toBe(
      'Cursor API key'
    );
    expect(sessionCalls).toBe(1);
    await put();
    now += 6000;
    expect(
      (await service.get({ refresh: true })).find((row) => row.provider === 'cursor')!.source
    ).toBe('Cursor API key');
    expect(service.configured()!.accounts.find((row) => row.provider === 'cursor')!.source).toBe(
      'Cursor API key'
    );
    mode = 'rejected';
    now += 6000;
    await service.get({ refresh: true });
    expect(sessionCalls).toBe(1);
  });
});
