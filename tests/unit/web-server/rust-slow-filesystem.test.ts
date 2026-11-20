/** Max pair 2 M2: the reviewers' delayed stat and regular boot-descriptor read schedules. */
import { expect, it } from 'bun:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFile, execFileSync } from 'node:child_process';

/** The shim's marker: how long the injected delay ran, and whether it ran on the first thread. */
interface Injection {
  ms: number;
  onMainThread: boolean;
}

interface FixtureResult {
  windowTicks: number;
  windowHttp: number;
  injection: Injection | null;
  responses: number;
  delayed: boolean;
}

function runFixture(
  root: string,
  kind: string,
  mode: string,
  shim: string
): Promise<FixtureResult> {
  const home = path.join(root, `${kind}-${mode}-${Date.now()}`);
  fs.mkdirSync(home);
  const { promise, resolve, reject } = Promise.withResolvers<FixtureResult>();
  execFile(
    'node',
    [path.join(import.meta.dir, 'fixtures/slow-lifecycle.cjs'), home, kind, mode],
    { env: { ...process.env, LD_PRELOAD: shim }, timeout: 150_000 },
    (error, stdout, stderr) =>
      error
        ? reject(new Error(`${error}\n${stderr}`))
        : resolve(JSON.parse(stdout.trim().split('\n').at(-1)!))
  );
  return promise;
}

/** The loop showed its own progress while the delay was in progress, off that loop. */
function live(result: FixtureResult): boolean {
  return (
    !!result.injection &&
    result.injection.ms > 1000 &&
    !result.injection.onMainThread &&
    result.windowTicks > 0 &&
    result.windowHttp > 0
  );
}

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

        // Off mode never reads through the injected path, so the delay must not appear; the
        // shadow case below carries the liveness and throughput assertions.
        const off = await runFixture(root, kind, 'off', shim);
        if (off.delayed || off.responses < 1)
          console.error(`slow lifecycle ${kind} off: ${JSON.stringify(off)}`);
        expect({ delayed: off.delayed, serves: off.responses > 0 }).toEqual({
          delayed: false,
          serves: true,
        });

        // Shadow mode pays the delay somewhere else than the event loop. The assertions are
        // read from the delay's own window, not from a wall-clock budget: a loaded machine may
        // starve the whole process for seconds without any block. Starvation can still hide the
        // loop for one 3,000 ms window, so it earns one retry with a named reason; a real block
        // is deterministic (the marker records the loop itself as the payer) and never retries.
        let shadow = await runFixture(root, kind, 'shadow', shim);
        if (!live(shadow) && !shadow.injection?.onMainThread)
          console.warn(
            `slow lifecycle ${kind}: no event-loop tick inside the ${shadow.injection?.ms ?? 0} ms delay (overloaded machine?), retrying once`
          );
        if (!live(shadow) && !shadow.injection?.onMainThread)
          shadow = await runFixture(root, kind, 'shadow', shim);

        expect(shadow.delayed).toBe(true);
        expect(shadow.responses).toBeGreaterThan(5);
        if (!live(shadow)) console.error(`slow lifecycle ${kind}: ${JSON.stringify(shadow)}`);
        // A slow lifecycle read on the event loop cannot leave a tick, or an HTTP answer, in
        // its own window, and the shim records that it ran there.
        expect({
          delayRecorded: (shadow.injection?.ms ?? 0) > 1000,
          ranOnEventLoop: shadow.injection?.onMainThread ?? null,
          loopTickedInsideDelay: shadow.windowTicks > 0,
          httpAnsweredInsideDelay: shadow.windowHttp > 0,
        }).toEqual({
          delayRecorded: true,
          ranOnEventLoop: false,
          loopTickedInsideDelay: true,
          httpAnsweredInsideDelay: true,
        });
      } finally {
        fs.rmSync(root, { recursive: true, force: true });
      }
    },
    300_000
  );
