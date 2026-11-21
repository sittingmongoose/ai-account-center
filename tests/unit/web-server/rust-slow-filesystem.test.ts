/** Max pair 2 M2: the reviewers' delayed stat and regular boot-descriptor read schedules. */
import { expect, it } from 'bun:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFile, execFileSync } from 'node:child_process';

for (const kind of ['metadata', 'descriptor'])
  it.skipIf(process.platform !== 'linux')(
    `keeps public HTTP and timers live during slow lifecycle ${kind}`,
    async () => {
      const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-slow-fs-'));
      try {
        const shim = path.join(root, 'slow.so');
        execFileSync('cc', [
          '-shared',
          '-fPIC',
          path.join(import.meta.dir, 'fixtures/slow-lifecycle.c'),
          '-o',
          shim,
          '-ldl',
        ]);
        for (const mode of ['off', 'shadow']) {
          const home = path.join(root, mode);
          fs.mkdirSync(home);
          const text = await new Promise<string>((resolve, reject) =>
            execFile(
              'node',
              [path.join(import.meta.dir, 'fixtures/slow-lifecycle.cjs'), home, kind, mode],
              { env: { ...process.env, LD_PRELOAD: shim }, timeout: 16000 },
              (error, stdout, stderr) =>
                error ? reject(new Error(`${error}\n${stderr}`)) : resolve(stdout)
            )
          );
          const value = JSON.parse(text.trim().split('\n').at(-1)!);
          expect(value.delayed).toBe(mode === 'shadow');
          // A loaded Nas1 can miss the 700 ms heartbeat budget. The stat shim
          // sleeps for 6200 ms, so 1200 ms still detects a main-thread stat
          // with room for scheduling jitter. Keep the tighter read budget for
          // the descriptor shim, whose sleep is only 1300 ms.
          expect(value.maxGapMs).toBeLessThan(kind === 'metadata' ? 1200 : 700);
          expect(value.responses).toBeGreaterThan(5);
          expect(value.maxHttpMs).toBeLessThan(700);
        }
      } finally {
        fs.rmSync(root, { recursive: true, force: true });
      }
    },
    25000
  );
