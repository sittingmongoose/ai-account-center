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
  /** Longest stretch inside the delay's window without a main-loop tick (edges included). */
  silentMs: number;
  /** The same for the worker-thread control loop in this process, over the same window. */
  controlSilentMs: number;
  /** The same for completed HTTP answers. */
  httpSilentMs: number;
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

/**
 * How much longer than the control loop the main loop, or HTTP, may go silent inside the
 * delay's window. Load starves both loops alike; a block on the main thread starves only the
 * main loop. HTTP needs a few loop turns per answer, so it gets a little more.
 */
const LOOP_MARGIN_MS = 1000;
const HTTP_MARGIN_MS = 1500;

function verdict(result: FixtureResult) {
  return {
    delayRecorded: (result.injection?.ms ?? 0) > 1000,
    ranOnEventLoop: result.injection?.onMainThread ?? null,
    loopTickedInsideDelay: result.windowTicks > 0,
    httpAnsweredInsideDelay: result.windowHttp > 0,
    loopSilenceWithinControl: result.silentMs < result.controlSilentMs + LOOP_MARGIN_MS,
    httpSilenceWithinControl: result.httpSilentMs < result.controlSilentMs + HTTP_MARGIN_MS,
  };
}

const LIVE = {
  delayRecorded: true,
  ranOnEventLoop: false,
  loopTickedInsideDelay: true,
  httpAnsweredInsideDelay: true,
  loopSilenceWithinControl: true,
  httpSilenceWithinControl: true,
};

/** The loop and HTTP kept pace with the control loop while the delay ran, off the loop. */
function live(result: FixtureResult): boolean {
  return Object.entries(LIVE).every(
    ([key, value]) => verdict(result)[key as keyof typeof LIVE] === value
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
        // read from the delay's own window and against a control loop in the same process, not
        // from a wall-clock budget: a loaded machine may starve the whole process for seconds
        // without any block, and then it starves the control too. Uneven starvation can still
        // hide the loop for one window, so a miss earns one retry with a named reason; a delay
        // paid on the loop itself is deterministic (the marker names the payer) and never
        // retries, and a real main-thread block fails the retry as well.
        let shadow = await runFixture(root, kind, 'shadow', shim);
        if (!live(shadow) && !shadow.injection?.onMainThread)
          console.warn(
            `slow lifecycle ${kind}: main loop silent ${shadow.silentMs} ms, HTTP ${shadow.httpSilentMs} ms, control ${shadow.controlSilentMs} ms inside the ${shadow.injection?.ms ?? 0} ms delay (uneven load or a block?), retrying once`
          );
        if (!live(shadow) && !shadow.injection?.onMainThread)
          shadow = await runFixture(root, kind, 'shadow', shim);

        expect(shadow.delayed).toBe(true);
        expect(shadow.responses).toBeGreaterThan(5);
        if (!live(shadow)) console.error(`slow lifecycle ${kind}: ${JSON.stringify(shadow)}`);
        // A slow lifecycle read on the event loop cannot leave a tick, or an HTTP answer, in
        // its own window, and the shim records that it ran there. A shorter main-thread block
        // inside the window (a sync hash, parse or exec) leaves the main loop silent far longer
        // than the control loop.
        expect(verdict(shadow)).toEqual(LIVE);
      } finally {
        fs.rmSync(root, { recursive: true, force: true });
      }
    },
    300_000
  );
