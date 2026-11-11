import { expect, it } from 'bun:test';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { AccountAnalyticsActivityService } from '../../../src/web-server/services/account-analytics-activity';
import { loadAnalyticsRemoteSources } from '../../../src/web-server/services/analytics-remote-sources';

it('explicit Analytics refresh bypasses a recent successful remote scan', async () => {
  const home = mkdtempSync(join(tmpdir(), 'aac-explicit-refresh-'));
  const previous = process.env.CCS_HOME;
  process.env.CCS_HOME = home;
  const now = Date.parse('2026-10-01T16:30:00Z');
  let tokens = 100;
  let calls = 0;
  try {
    const service = new AccountAnalyticsActivityService({
      scope: () => home,
      now: () => now,
      requests: () => [],
      remote: (ms, options) =>
        loadAnalyticsRemoteSources(ms, {
          ...options,
          cacheDir: join(home, 'remote'),
          now: () => now,
          hosts: async () => ({ mac: 'fixture', windows: null, nas1: null }),
          runHelper: async (_alias, _host, request) => {
            calls++;
            return {
              version: 2,
              truncated: false,
              kinds: Object.fromEntries(
                request.kinds.map((k) => [
                  k,
                  {
                    state: k === 'omp' ? 'ok' : 'not_installed',
                    fingerprints:
                      k === 'omp' ? { ['a1'.repeat(32)]: { size: tokens, mtimeMs: tokens } } : {},
                  },
                ])
              ),
              rows: request.kinds.includes('omp')
                ? [
                    {
                      k: 'omp',
                      f: 'a1'.repeat(32),
                      m: 'deepseek-v4.1-flash',
                      h: '2026-10-01 15:00',
                      i: tokens,
                      o: 10,
                      cr: 0,
                      cw: 0,
                      c: 0.01,
                      n: 1,
                    },
                  ]
                : [],
              srows: [],
            };
          },
        }),
    });
    const query = { platform: 'mac', range: '24h', provider: 'all', account: 'all' } as const;
    const settled = async () => {
      for (let i = 0; i < 100; i++) {
        const answer = await service.get(query, now - 86_400_000, now);
        if (!answer.refreshing) return answer;
        await new Promise((resolve) => setTimeout(resolve, 5));
      }
      throw new Error('Fixture collection did not settle');
    };
    expect((await settled()).totals?.inputTokens).toBe(100);
    calls = 0;
    tokens = 200;
    await service.get({ ...query, refresh: true }, now - 86_400_000, now);
    expect((await settled()).totals?.inputTokens).toBe(200);
    expect(calls).toBeGreaterThan(0);
  } finally {
    if (previous === undefined) delete process.env.CCS_HOME;
    else process.env.CCS_HOME = previous;
    rmSync(home, { recursive: true, force: true });
  }
});
