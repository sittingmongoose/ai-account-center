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
  complete: boolean;
}

interface FixtureResult {
  windowTicks: number;
  windowHttp: number;
  /** Longest gap between main-loop ticks that overlaps the delay's window, measured whole. */
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
 * Fixed bounds, not budgets relative to the control: a main-loop gap of 1,000 ms or more inside
 * the delay's window fails, and so does an HTTP gap of 1,500 ms or more (an answer needs a few
 * loop turns). They are read only while the control loop stayed responsive, its own longest gap
 * under 250 ms; a starved control makes the sample inconclusive, never a pass.
 */
const LOOP_STALL_MS = 1000;
const HTTP_STALL_MS = 1500;
const CONTROL_RESPONSIVE_MS = 250;

/** A sample says something only if the delay ran its course and the control kept ticking. */
function conclusive(result: FixtureResult): boolean {
  return (
    !!result.injection?.complete &&
    result.injection.ms > 1000 &&
    result.controlSilentMs < CONTROL_RESPONSIVE_MS
  );
}

function verdict(result: FixtureResult) {
  return {
    delayRecorded: !!result.injection?.complete && result.injection.ms > 1000,
    controlResponsive: result.controlSilentMs < CONTROL_RESPONSIVE_MS,
    ranOnEventLoop: result.injection?.onMainThread ?? null,
    loopTickedInsideDelay: result.windowTicks > 0,
    httpAnsweredInsideDelay: result.windowHttp > 0,
    loopStallUnderBound: result.silentMs < LOOP_STALL_MS,
    httpStallUnderBound: result.httpSilentMs < HTTP_STALL_MS,
  };
}

const LIVE = {
  delayRecorded: true,
  controlResponsive: true,
  ranOnEventLoop: false,
  loopTickedInsideDelay: true,
  httpAnsweredInsideDelay: true,
  loopStallUnderBound: true,
  httpStallUnderBound: true,
};

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
        // read from the delay's own window under fixed bounds, and only while a control loop in
        // the same process kept ticking: a loaded machine may starve the whole process for
        // seconds without any block, and then it starves the control too. Only such an
        // inconclusive sample (starved control, or no complete delay) earns one retry, with a
        // named reason. A conclusive sample stands, pass or fail: a main-loop stall seen while
        // the control ticked is never retried away.
        let shadow = await runFixture(root, kind, 'shadow', shim);
        if (!conclusive(shadow)) {
          console.warn(
            `slow lifecycle ${kind}: inconclusive sample (control silent ${shadow.controlSilentMs} ms, delay ${shadow.injection?.ms ?? 'missing'} ms${shadow.injection?.complete ? '' : ', incomplete'}), retrying once`
          );
          shadow = await runFixture(root, kind, 'shadow', shim);
        }

        expect(shadow.delayed).toBe(true);
        expect(shadow.responses).toBeGreaterThan(5);
        const result = verdict(shadow);
        if (JSON.stringify(result) !== JSON.stringify(LIVE))
          console.error(`slow lifecycle ${kind}: ${JSON.stringify(shadow)}`);
        // A slow lifecycle read on the event loop cannot leave a tick, or an HTTP answer, in
        // its own window, and the shim records that it ran there. A shorter main-thread block
        // inside the window (a sync hash, parse or exec) of 1,000 ms or more leaves a main-loop
        // gap at or over the bound while the control keeps ticking.
        expect(result).toEqual(LIVE);
      } finally {
        fs.rmSync(root, { recursive: true, force: true });
      }
    },
    300_000
  );
