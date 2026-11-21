'use strict';
// Runs in a separate Node process with a syscall shim and one owned fixture root.
const fs = require('node:fs');
const path = require('node:path');
const root = process.argv[2];
const kind = process.argv[3];
const mode = process.argv[4];
const repo = path.resolve(__dirname, '../../../..');
const ccs = path.join(root, '.ccs');
fs.mkdirSync(path.join(ccs, 'run'), { recursive: true, mode: 0o700 });
fs.writeFileSync(path.join(ccs, 'run/aac-http.json'), '{}', { mode: 0o600 });
Object.assign(process.env, {
  CCS_HOME: root,
  CCS_DIR: ccs,
  CLAUDE_CONFIG_DIR: path.join(root, 'claude'),
  CODEX_HOME: path.join(root, 'codex'),
  AAC_AGENT_REPORTS_DIR: path.join(root, 'absent-reports'),
  AAC_PM_MAIL_REGISTRY_DIR: path.join(root, 'absent-registry'),
  AAC_STATUS_DOORBELL: 'off',
  AAC_AGENT_TRANSPORT: 'off',
  AAC_RUST_STORE: 'off',
  AAC_STORE: 'off',
});
require('node:os').homedir = () => root;
require(
  path.join(repo, 'dist/web-server/services/codex-profile-renewal-service')
).CodexProfileRenewalService.prototype.start = () => {};
// Public startup stays real. Unrelated background readers/producers have no work here.
require(
  path.join(repo, 'dist/web-server/services/account-lifecycle-runtime')
).startAccountLifecycleMaintenance = () => {};
global.fetch = () => Promise.reject(new Error('fixture refuses outbound fetch'));
const binary = path.join(root, 'aac-http');
fs.copyFileSync(path.join(__dirname, 'fake-aac-http.mjs'), binary);
fs.chmodSync(binary, 0o755);
const { startServer } = require(path.join(repo, 'dist/web-server'));
const http = require('node:http');
const { Worker } = require('node:worker_threads');
// Liveness control: a second event loop in this same process, behind the same syscall shim.
// A loaded machine delays both loops alike, so main minus control isolates a main-thread
// block. The control never touches the injected path, so the delay cannot reach it.
const RING = 8189;
const controlBuffer = new SharedArrayBuffer(8 * (RING + 2));
const controlMetrics = new Float64Array(controlBuffer);
const control = new Worker(
  `const { workerData } = require('node:worker_threads');
   const { performance } = require('node:perf_hooks');
   const metrics = new Float64Array(workerData);
   let previous = performance.now();
   setInterval(() => {
     const now = performance.now();
     const gap = now - previous;
     previous = now;
     if (gap > metrics[0]) metrics[0] = gap;
     const count = metrics[1];
     metrics[2 + (count % ${RING})] = Date.now();
     metrics[1] = count + 1;
   }, 20);`,
  { eval: true, workerData: controlBuffer }
);
control.unref();
(async () => {
  // Arm only after fixture and module preparation. The shim affects this exact path once.
  process.env[kind === 'metadata' ? 'AAC_SLOW_STAT_PATH' : 'AAC_SLOW_READ_PATH'] = path.join(
    ccs,
    kind === 'metadata' ? 'run' : 'run/aac-http.json'
  );
  process.env.AAC_SLOW_MARKER = path.join(root, 'delayed');
  const began = performance.now();
  let previous = began,
    maxGapMs = 0,
    maxHttpMs = 0,
    responses = 0;
  // Every main-loop gap, on the shim's clock, so the run can be read around the injection.
  const gaps = [];
  let lastTickAt = Date.now();
  const heartbeat = setInterval(() => {
    const now = performance.now();
    maxGapMs = Math.max(maxGapMs, now - previous);
    previous = now;
    const at = Date.now();
    gaps.push([lastTickAt, at]);
    lastTickAt = at;
  }, 20);
  const instance = await startServer({
    port: 0,
    host: '127.0.0.1',
    rustService: mode,
    rustServiceBinary: binary,
  });
  try {
    const port = instance.server.address().port;
    // Measured from listening, not from process start: a loaded machine can spend a whole
    // fixed window on startup, and then no request would test liveness at all.
    const listeningMs = performance.now() - began;
    const markerPath = path.join(root, 'delayed');
    const readMarker = () => {
      let text = '';
      try {
        text = fs.readFileSync(markerPath, 'utf8').trim();
      } catch {
        return null;
      }
      const [start, end, onMain, pid] = text.split(/\s+/).map(Number);
      if (!Number.isFinite(start) || !Number.isFinite(end)) return null;
      return {
        start,
        end,
        ms: end - start,
        onMainThread: onMain === 1,
        pid,
        complete: end > start,
      };
    };
    // In shadow mode the lifecycle read runs on the supervisor's schedule, sometimes seconds
    // after listening: keep the loop busy until that delay has finished, so the window below is
    // the real one. Off mode never touches the injected path and holds only the floor. Requests,
    // not milliseconds, set the floor: a loaded machine serves a handful per second at most, so
    // the cap is the time six of them may take there, not the time they take on an idle box.
    const floor = performance.now() + (kind === 'metadata' ? 2000 : 1200);
    const deadline =
      performance.now() + (mode === 'shadow' ? (kind === 'metadata' ? 40000 : 30000) : 30000);
    const measuredFrom = Date.now();
    const httpDone = [];
    let injection = null;
    let foreignInjection = null;
    do {
      const started = performance.now();
      await new Promise((resolve, reject) => {
        http
          .get(`http://127.0.0.1:${port}/api/health`, (res) => {
            if (res.statusCode !== 200) reject(new Error('health status'));
            res.resume();
            res.on('end', resolve);
          })
          .on('error', reject);
      });
      maxHttpMs = Math.max(maxHttpMs, performance.now() - started);
      httpDone.push(Date.now());
      responses++;
      const marker = readMarker();
      if (marker && marker.pid === process.pid) injection = marker;
      else if (marker) foreignInjection = marker;
      await new Promise((resolve) => setTimeout(resolve, 25));
    } while (
      performance.now() < deadline &&
      (responses < (mode === 'shadow' ? 6 : 2) ||
        performance.now() < floor ||
        (mode === 'shadow' && !injection?.complete))
    );
    // What the main loop did inside the injected delay: a tick means it ran there. A delay paid
    // on this thread leaves the loop silent for its whole length; one paid elsewhere does not.
    // Each clock's silence is its longest gap between progress marks that overlaps the window,
    // measured whole: a stall that began just before the shim opened the window counts in full.
    // A loaded machine silences the control worker as much as the main loop; a block on the
    // main thread silences only the main loop.
    let windowTicks = 0;
    let controlWindowTicks = 0;
    let silentMs = 0;
    let controlSilentMs = 0;
    let windowHttp = 0;
    let httpSilentMs = 0;
    if (injection) {
      // The main loop is running now, so now is its latest progress mark; the control's latest
      // mark is its own last tick, so a stalled control reads as silent up to now.
      const now = Date.now();
      const silence = (times, until) => {
        let previous = injection.start,
          count = 0,
          worst = 0;
        for (const at of [...times, until].sort((a, b) => a - b)) {
          if (at <= injection.start) {
            previous = at;
            continue;
          }
          worst = Math.max(worst, at - previous);
          if (at >= injection.end) break;
          count++;
          previous = at;
        }
        return { count, ms: worst };
      };
      const main = silence(
        gaps.map(([, end]) => end),
        now
      );
      windowTicks = main.count;
      silentMs = main.ms;
      const answers = silence(httpDone, now);
      windowHttp = answers.count;
      httpSilentMs = answers.ms;
      const controlTimes = [];
      for (let index = 0; index < Math.min(controlMetrics[1], RING); index++)
        controlTimes.push(controlMetrics[2 + index]);
      const controlLoop = silence(controlTimes, now);
      controlWindowTicks = controlLoop.count;
      controlSilentMs = controlLoop.ms;
    }
    console.log(
      JSON.stringify({
        maxHttpMs,
        maxGapMs,
        controlGapMs: controlMetrics[0],
        controlTicks: controlMetrics[1],
        injection,
        foreignInjection,
        windowTicks,
        controlWindowTicks,
        silentMs,
        controlSilentMs,
        windowHttp,
        httpSilentMs,
        responses,
        delayed: injection !== null,
        listeningMs,
        measuredMs: Date.now() - measuredFrom,
        elapsed: performance.now() - began,
        diagnostics: instance.rustService?.diagnostics?.(),
      })
    );
  } finally {
    clearInterval(heartbeat);
    await control.terminate();
    instance.cleanup();
    await new Promise((resolve) => instance.server.close(resolve));
  }
})().catch((error) => {
  void control.terminate();
  console.error(error);
  process.exitCode = 1;
});
