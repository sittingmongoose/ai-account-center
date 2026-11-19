/**
 * The runtime refresh command over fakes: the refresh verdict is injected,
 * output is a list of lines, and no descriptor is ever touched.
 */
import { describe, expect, it } from 'bun:test';
import { runAntigravityTerminalRefresh } from '../../../src/antigravity/terminal-refresh';
import type { RuntimeRefreshResult } from '../../../src/antigravity/runtime-refresh';

function run(
  result: RuntimeRefreshResult,
  ownsState = true
): Promise<{ code: number; out: string }> {
  const lines: string[] = [];
  return runAntigravityTerminalRefresh({
    ccsDir: '/fixture-only/.ccs',
    home: '/fixture-only',
    uid: 1000,
    io: { write: (text) => lines.push(text) },
    ownsState: () => ownsState,
    refresh: async () => result,
  }).then((code) => ({ code, out: lines.join('') }));
}

describe('runtime refresh command', () => {
  it('refuses foreign state before refreshing', async () => {
    let calls = 0;
    const lines: string[] = [];
    const code = await runAntigravityTerminalRefresh({
      ccsDir: '/fixture-only/.ccs',
      home: '/fixture-only',
      uid: 1000,
      io: { write: (text) => lines.push(text) },
      ownsState: () => false,
      refresh: async () => {
        calls++;
        return { status: 'current', version: '1.2.16', sha256: 'c'.repeat(64) };
      },
    });
    expect(code).toBe(1);
    expect(calls).toBe(0);
    expect(lines.join('')).toContain('only as the user that owns');
  });

  it('reports each refresh verdict with its own headline', async () => {
    for (const [result, code, headline] of [
      [{ status: 'current', version: '1.2.16', sha256: 'c'.repeat(64) }, 0, 'is current'],
      [
        { status: 'refreshed', version: '1.2.16', sha256: 'c'.repeat(64), previousSha256: 'a'.repeat(64) },
        0,
        'refreshed to the reviewed 1.2.16 build',
      ],
      [{ status: 'unreviewed', installedVersion: '1.2.99' }, 1, 'updated to 1.2.99; switching paused until reviewed'],
      [{ status: 'unreviewed', installedVersion: null }, 1, 'updated; switching paused until reviewed'],
      [{ status: 'no-installation' }, 1, 'No Antigravity runtime is installed'],
      [{ status: 'gate-closed' }, 1, 'not released in this build'],
      [{ status: 'failed' }, 1, 'could not complete its checks'],
    ] as const) {
      const runResult = await run(result as RuntimeRefreshResult);
      expect(runResult.code).toBe(code);
      expect(runResult.out).toContain(headline);
    }
  });

  it('scrubs a hostile installed version from the paused message', async () => {
    const runResult = await run({ status: 'unreviewed', installedVersion: '1.2.99; id' });
    expect(runResult.code).toBe(1);
    expect(runResult.out).toContain('updated; switching paused until reviewed');
    expect(runResult.out).not.toContain('1.2.99; id');
  });
});
