import { afterEach, expect, test } from 'bun:test';
import { spawnSync } from 'child_process';
import { mkdtempSync, rmSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';
import { fileURLToPath } from 'url';

const roots: string[] = [];
afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});
for (const mode of ['lost-reply', 'entry-throw']) {
  test(`real Claude adapter preserves uncertainty after helper ${mode}`, () => {
    const root = mkdtempSync(join(tmpdir(), 'aac-adapter-boundary-'));
    roots.push(root);
    const fixture = fileURLToPath(
      new URL('../../fixtures/reset-adapter/host-failure.ts', import.meta.url)
    );
    const child = spawnSync(process.execPath, [fixture, mode], {
      env: {
        ...process.env,
        CCS_HOME: root,
        CCS_DIR: join(root, '.ccs'),
        AAC_STATUS_NATIVE: 'off',
      },
      encoding: 'utf8',
      timeout: 10_000,
    });
    expect(child.status).toBe(0);
    const result = JSON.parse(child.stdout);
    expect(result.entered).toBe(2);
    expect(result.rejected).toBe(true);
    expect(result.directOutcome).not.toBe('not-sent');
    expect(result.status).toBe(200);
    expect(result.finishOutcome).toBe('ambiguous');
    expect(result.reads).toBe(2);
    expect(result.receipt).toEqual({ outcome: 'claimed-unconfirmed', pending: true });
  });
}
