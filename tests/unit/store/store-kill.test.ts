/**
 * Kill -9 during sampling loses no acknowledged reading (docs/store.md phase 1). A child process
 * records one changed reading after another and prints each acknowledged sample time; it is
 * killed without warning; the store, reopened, holds every acknowledged reading. Needs the
 * native module in dist/native (bun run store:build).
 */
import { describe, expect, it } from 'bun:test';
import { spawn } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { loadNativeStore, nativeModuleName } from '../../../src/store/native-loader';

const MODULE = path.resolve(__dirname, '../../../dist/native', nativeModuleName());
const loaded = loadNativeStore({ env: {}, file: MODULE });

const CHILD = `
const { NativeStore } = require(process.argv[2]);
const dir = process.argv[3];
(async () => {
  const store = new NativeStore();
  await store.call('open', JSON.stringify({ dir }));
  const start = 1790000000000;
  for (let i = 0; ; i++) {
    const t = start + i * 60000;
    const observation = {
      account: { identity: 'a'.repeat(64), provider: 'codex', account_id: 'codex:first', plan: null,
                 platform: 'ubuntu', source: 'kill test', status: 'ok', is_active: true },
      sampled_at_ms: t, observed_at_ms: t, import_ref: null,
      windows: [{ identity: '["five_hour",null,null]', key: 'five_hour', label: '5 hour',
                  used_pct: i % 100, remaining_pct: 100 - (i % 100), reset_at_ms: t + 3600000 }],
    };
    await store.call('record', JSON.stringify({ observations: [observation] }));
    process.stdout.write('ok ' + t + '\\n');
  }
})();
`;

describe.skipIf(loaded.module === null)('a killed writer', () => {
  it('keeps every acknowledged reading', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-store-kill-'));
    const script = path.join(dir, 'child.js');
    fs.writeFileSync(script, CHILD);
    const store = path.join(dir, 'store');
    try {
      let acknowledged = 0;
      let buffer = '';
      await new Promise<void>((resolve, reject) => {
        const child = spawn(process.execPath, [script, MODULE, store], {
          stdio: ['ignore', 'pipe', 'pipe'],
        });
        child.stdout.on('data', (chunk: Buffer) => {
          buffer += chunk.toString();
          const lines = buffer.split('\n');
          buffer = lines.pop() ?? '';
          for (const line of lines)
            if (line.startsWith('ok ')) acknowledged = Number(line.slice(3));
          // Kill in the middle of a run of writes, without warning.
          if (acknowledged > 1790000000000 + 150 * 60000 && !child.killed) child.kill('SIGKILL');
        });
        child.on('exit', () => resolve());
        child.on('error', reject);
        setTimeout(() => child.kill('SIGKILL'), 20_000).unref();
      });
      expect(acknowledged).toBeGreaterThan(1790000000000 + 150 * 60000);
      if (!loaded.module) throw new Error('unreachable');
      const handle = new loaded.module.NativeStore();
      await handle.call('open', JSON.stringify({ dir: store }));
      const records = JSON.parse(
        await handle.call(
          'historyRecords',
          JSON.stringify({
            fromMs: 0,
            toMs: acknowledged + 3600000,
            nowMs: acknowledged,
            thinning: { kind: 'none' },
          })
        )
      ) as Array<{ sampledAt: string; windows: Array<{ usedPercent: number }> }>;
      const status = JSON.parse(await handle.call('status', '{}')) as {
        findings: Array<{ kind: string }>;
      };
      await handle.call('close', '{}');
      const times = new Set(records.map((record) => Date.parse(record.sampledAt)));
      // Every acknowledged minute is there (each one changed the reading).
      for (let t = 1790000000000; t <= acknowledged; t += 60000) expect(times.has(t)).toBe(true);
      // A torn tail may be found and cut; nothing acknowledged is reported lost.
      expect(
        status.findings.some((f) => f.kind.includes('degraded') || f.kind.includes('missing'))
      ).toBe(false);
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  }, 60_000);
});
