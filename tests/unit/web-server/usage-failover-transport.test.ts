import { afterEach, describe, expect, it } from 'bun:test';
import { execFile } from 'child_process';
import fs from 'fs/promises';
import os from 'os';
import path from 'path';
import {
  localUsageHost,
  parseUsageHelperResponse,
  runUsageHelper,
  usageHelperBundle,
} from '../../../src/web-server/services/usage-failover-transport';
import type {
  UsageHelperRequest,
  UsageHelperResponse,
} from '../../../src/web-server/services/usage-failover-contract';

const temporary: string[] = [];
afterEach(async () => {
  await Promise.all(
    temporary.splice(0).map((directory) => fs.rm(directory, { recursive: true, force: true }))
  );
});

async function fixture() {
  const home = await fs.mkdtemp(path.join(os.tmpdir(), 'aac-failover-bundle-'));
  temporary.push(home);
  const config = path.join(home, 'config');
  await fs.mkdir(path.join(config, 'muse'), { recursive: true });
  const auth = path.join(config, 'muse', 'auth.json');
  const writeAuth = async (token: string) =>
    fs.writeFile(
      auth,
      JSON.stringify({
        providers: { meta: { access_token: token, user_email: 'fixture@example.test' } },
      })
    );
  const helperSource = await usageHelperBundle();
  const request = async (request: UsageHelperRequest): Promise<UsageHelperResponse> => {
    // This subprocess receives only explicit temporary paths. Even a collector
    // regression cannot contact a provider: every urllib opener is blocked.
    const bootstrap = [
      'import sys,json,io,urllib.request',
      "def _blocked(*args,**kwargs): raise AssertionError('network forbidden in bundle fixture')",
      'urllib.request.OpenerDirector.open=_blocked',
      "_p=json.loads(sys.stdin.buffer.read().decode('utf-8'))",
      "sys.stdin=io.StringIO(json.dumps(_p['request']))",
      "exec(compile(_p['helperSource'],'test-bundle','exec'))",
    ].join('\n');
    const text = await new Promise<string>((resolve, reject) => {
      const child = execFile(
        process.platform === 'win32' ? 'python' : '/usr/bin/python3',
        ['-B', '-c', bootstrap],
        {
          timeout: 10_000,
          maxBuffer: 2 * 1024 * 1024,
          windowsHide: true,
          encoding: 'utf8',
          env: {
            ...process.env,
            XDG_CONFIG_HOME: config,
            APPDATA: path.join(home, 'appdata'),
            PYTHONIOENCODING: 'utf-8',
          },
        },
        (error, stdout) => (error ? reject(error) : resolve(stdout))
      );
      child.stdin?.on('error', reject);
      child.stdin?.end(
        JSON.stringify({
          helperSource,
          request: { ...request, homeDir: home, ccsDir: path.join(home, 'ccs') },
        })
      );
    });
    expect(text).not.toContain('dca:');
    expect(text).not.toContain('access_token');
    return parseUsageHelperResponse(text);
  };
  return { home, writeAuth, request };
}

describe('packaged usage failover transport', () => {
  it('runs the in-memory bundle against an empty synthetic home without installed host helpers', async () => {
    const h = await fixture();
    expect(await h.request({ version: 1, action: 'inventory', provider: 'muse' })).toEqual({
      version: 1,
      action: 'inventory',
      candidates: [],
    });
    expect(await fs.readdir(h.home)).toEqual(['config']);
  });

  it('inventories the synthetic login and rejects its stale revision before provider access', async () => {
    const h = await fixture();
    await h.writeAuth('dca:bundle-first');
    const first = await h.request({ version: 1, action: 'inventory', provider: 'muse' });
    expect(first.action).toBe('inventory');
    if (first.action !== 'inventory') throw new Error('Expected inventory');
    expect(first.candidates).toHaveLength(1);
    await h.writeAuth('dca:bundle-rotated');
    const collected = await h.request({
      version: 1,
      action: 'collect',
      provider: 'muse',
      candidate: first.candidates[0],
    });
    expect(collected).toMatchObject({
      action: 'collect',
      sample: null,
      failureCode: 'source_changed',
    });
  });

  it('refuses a nonlocal OS with no SSH destination before launching a helper', async () => {
    const local = localUsageHost();
    const platform = local.platform === 'mac' ? 'windows' : 'mac';
    await expect(
      runUsageHelper(
        { host: platform, platform, sshHost: null },
        {
          version: 1,
          action: 'inventory',
          provider: 'muse',
        }
      )
    ).rejects.toThrow('not local');
  });

  it('rejects secret-bearing or malformed candidate metadata', () => {
    for (const candidate of [
      { locator: 'private-path' },
      {
        locator: '1'.repeat(64),
        revision: '2'.repeat(64),
        selectors: ['discover'],
        identity: { kind: 'muse-team', key: '3'.repeat(64) },
        email: 'fixture@example.test',
        plan: 'Bearer private-token',
      },
      {
        locator: '1'.repeat(64),
        revision: '2'.repeat(64),
        selectors: ['../auth.json'],
        identity: { kind: 'muse-team', key: '3'.repeat(64) },
        email: 'fixture@example.test',
        plan: null,
      },
    ]) {
      expect(() =>
        parseUsageHelperResponse(
          JSON.stringify({ version: 1, action: 'inventory', candidates: [candidate] })
        )
      ).toThrow();
    }
  });

  it('drops unrelated helper fields and bounds provider retry metadata', () => {
    const parsed = parseUsageHelperResponse(
      JSON.stringify({
        version: 1,
        action: 'collect',
        candidate: null,
        sample: null,
        failureCode: 'rate_limited',
        retryAfterSeconds: 999999999,
        secret: 'fixture-secret',
      })
    );
    expect(parsed).toEqual({
      version: 1,
      action: 'collect',
      candidate: null,
      sample: null,
      failureCode: 'rate_limited',
      retryAfterSeconds: 86400,
    });
  });

  it('preserves both bounded aliases reported for a signed-out desktop profile', () => {
    const selectors = ['claude-profile:default', 'claude-desktop-profile:default'];
    expect(
      parseUsageHelperResponse(
        JSON.stringify({
          version: 1,
          action: 'inventory',
          candidates: [],
          signInNeededSelectors: selectors,
        })
      )
    ).toEqual({
      version: 1,
      action: 'inventory',
      candidates: [],
      signInNeededSelectors: selectors,
    });
    for (const signInNeededSelectors of [
      ['../private'],
      ['claude-profile:bad\nname'],
      Array.from({ length: 129 }, () => 'claude-profile:default'),
    ]) {
      expect(() =>
        parseUsageHelperResponse(
          JSON.stringify({ version: 1, action: 'inventory', candidates: [], signInNeededSelectors })
        )
      ).toThrow();
    }
  });
});
