/**
 * R2 measurement under Node (the installed runtime): GET /api/status end to end on the large synthetic home
 * (large-home.mjs: 200 agent reports of 50 writers), today's TS path (off), today's M3 envelope path (on) and the
 * fed path (R2), in alternating rounds (ABAB: 10 rounds of 40 requests per server by default) so drift on a loaded
 * host cancels. Each server is its own process (status-measure-server.cjs) on its own copy of the home, with the
 * Status owner running its scan timers. Records p50/p95/p99, the load average before and after each round, RSS of
 * TS and of the Rust child after the rounds, the event-loop p99, the owner's `readInputs` time and the fed path's
 * Rust part (the hop and the service's answer), and evaluates the design's R2 targets. Every fed reply in the
 * rounds must be the fed answer (the route's served counter), and the fed and off answers must agree (ETag at the
 * same second, checked before the rounds).
 *
 *   node tests/spec/status-measure.mjs --dist <dist> --binary <aac-http> --out <file.json>
 *        [--rounds 10] [--samples 40] [--clients 16] [--seconds 20] [--base <aac-http>]
 * `--base` adds today's envelope path on another build's service (`on:base`, the before of the composer).
 * Called by `read-http-parity.ts --measure`. Fixture data only; the processes it starts are its own.
 */
import assert from 'node:assert/strict';
import { fork, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const option = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : fallback;
};
const dist = path.resolve(option('dist', path.join(here, '../../dist')));
const binary = path.resolve(option('binary', path.join(dist, 'native/aac-http')));
const out = option('out', null);
const rounds = Number(option('rounds', 10));
const samples = Number(option('samples', 40));
const clients = Number(option('clients', 16));
const seconds = Number(option('seconds', 20));
const base = option('base', process.env.AAC_R2_BASE_BINARY ?? null);
assert.ok(out, '--out <file.json> is required');
assert.ok(fs.existsSync(binary), `no aac-http at ${binary}`);
const scratch = fs.mkdtempSync(path.join(process.env.TMPDIR ?? os.tmpdir(), 'aac-r2-measure-'));
const workers = [];

function home(name) {
  const root = path.join(scratch, name);
  const made = spawnSync(process.execPath, [path.join(here, 'large-home.mjs'), root], {
    encoding: 'utf8',
  });
  assert.equal(made.status, 0, made.stderr);
  return { root, summary: JSON.parse(made.stdout) };
}

function start(mode, root, label = mode, bin = binary) {
  return new Promise((resolve, reject) => {
    const child = fork(path.join(here, 'status-measure-server.cjs'), [dist, bin, mode, root], {
      execArgv: ['--expose-gc'],
      stdio: ['ignore', 'ignore', 'pipe', 'ipc'],
    });
    let stderr = '';
    child.stderr.on('data', (d) => (stderr += d));
    const worker = {
      mode,
      child,
      pending: new Map(),
      agent: new http.Agent({ keepAlive: true, maxSockets: 32 }),
    };
    workers.push(worker);
    child.on('message', (message) => {
      if (message.type === 'ready') {
        worker.port = message.port;
        resolve(worker);
      } else if (message.type === 'error') reject(new Error(message.message));
      else if (message.type === 'stats') {
        worker.pending.get(message.id)?.(message);
        worker.pending.delete(message.id);
      }
    });
    child.on('exit', (code) => {
      if (!worker.port) reject(new Error(`${mode} server exited ${code}: ${stderr}`));
    });
  });
}
let asked = 0;
function ask(worker, type, extra = {}) {
  const id = ++asked;
  return new Promise((resolve) => {
    worker.pending.set(id, resolve);
    worker.child.send({ type, id, ...extra });
  });
}
const stats = (worker, reset = false) => ask(worker, 'stats', { reset });

function call(worker) {
  return new Promise((resolve, reject) => {
    const started = process.hrtime.bigint();
    const request = http.get(
      { host: '127.0.0.1', port: worker.port, path: '/api/status', agent: worker.agent },
      (response) => {
        const chunks = [];
        response.on('data', (c) => chunks.push(c));
        response.on('end', () =>
          resolve({
            status: response.statusCode,
            etag: response.headers.etag,
            body: Buffer.concat(chunks),
            ms: Number(process.hrtime.bigint() - started) / 1e6,
          })
        );
      }
    );
    request.on('error', reject);
  });
}

const quantiles = (values) => {
  const sorted = [...values].sort((a, b) => a - b);
  const at = (q) =>
    Number(sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))].toFixed(3));
  return { samples: sorted.length, p50Ms: at(0.5), p95Ms: at(0.95), p99Ms: at(0.99) };
};

try {
  const fixture = home('off').summary;
  home('on');
  home('fed');
  if (base) home('base');
  const servers = [
    await start('off', path.join(scratch, 'off')),
    await start('on', path.join(scratch, 'on')),
    await start('fed', path.join(scratch, 'fed')),
    ...(base ? [await start('on', path.join(scratch, 'base'), 'on:base', path.resolve(base))] : []),
  ];
  // Warm up, and check the fed server's answers: on one read, the fed path's bytes are today's builder's.
  for (const worker of servers)
    for (let i = 0; i < 20; i++) assert.equal((await call(worker)).status, 200);
  const verified = [];
  for (let i = 0; i < 20; i++) verified.push(await ask(servers[2], 'verify'));
  assert.ok(
    verified.every((v) => v.served && v.equal),
    `the fed path serves today's bytes: ${JSON.stringify(verified.slice(0, 3))}`
  );
  const sizes = Object.fromEntries(
    await Promise.all(servers.map(async (w) => [w.mode, (await call(w)).body.length]))
  );
  for (const worker of servers) await stats(worker, true);
  const fedBefore = (await stats(servers[2])).route?.served ?? 0;

  const times = Object.fromEntries(servers.map((w) => [w.mode, []]));
  const loads = [];
  for (let round = 0; round < rounds; round++) {
    const order = round % 2 === 0 ? servers : [...servers].reverse();
    for (const worker of order) {
      const before = os.loadavg()[0];
      for (let i = 0; i < samples; i++) {
        const reply = await call(worker);
        assert.equal(reply.status, 200);
        times[worker.mode].push(reply.ms);
      }
      loads.push({ round, mode: worker.mode, before, after: os.loadavg()[0] });
    }
  }
  const after = Object.fromEntries(
    await Promise.all(servers.map(async (w) => [w.mode, await stats(w)]))
  );
  const fedServed = (after.fed.route?.served ?? 0) - fedBefore;
  assert.equal(
    fedServed,
    rounds * samples,
    `every fed request in the rounds was served by the fed path (${fedServed})`
  );

  let concurrency = null;
  if (clients > 0) {
    concurrency = {};
    for (const worker of servers) {
      await stats(worker, true);
      const latencies = [];
      const end = Date.now() + seconds * 1000;
      await Promise.all(
        Array.from({ length: clients }, async () => {
          while (Date.now() < end) latencies.push((await call(worker)).ms);
        })
      );
      const s = await stats(worker);
      concurrency[worker.mode] = {
        requests: latencies.length,
        ...quantiles(latencies),
        eventLoopP99Ms: s.eventLoopP99Ms,
      };
    }
  }

  const latency = Object.fromEntries(servers.map((w) => [w.mode, quantiles(times[w.mode])]));
  const memory = Object.fromEntries(
    servers.map((w) => {
      const s = after[w.mode];
      return [
        w.mode,
        {
          tsRssMiB: Number((s.tsRssBytes / 2 ** 20).toFixed(1)),
          rustRssMiB: Number((s.rustRssBytes / 2 ** 20).toFixed(1)),
          combinedMiB: Number(((s.tsRssBytes + s.rustRssBytes) / 2 ** 20).toFixed(1)),
          eventLoopP99Ms: s.eventLoopP99Ms,
        },
      ];
    })
  );
  const parts = Object.fromEntries(
    servers.map((w) => [
      w.mode,
      { readInputs: after[w.mode].readInputs, rustPart: after[w.mode].rustPart },
    ])
  );
  const overOff = Number((memory.fed.combinedMiB - memory.off.combinedMiB).toFixed(1));
  const result = {
    label: option('label', 'r2-status'),
    node: process.version,
    host: os.hostname(),
    cpus: os.cpus().length,
    binarySha256: spawnSync('sha256sum', [binary], { encoding: 'utf8' }).stdout.split(' ')[0],
    ...(base
      ? {
          baseBinarySha256: spawnSync('sha256sum', [base], { encoding: 'utf8' }).stdout.split(
            ' '
          )[0],
        }
      : {}),
    fixture,
    viewBytes: sizes,
    fedVerified: verified.length,
    roundCount: rounds,
    samplesPerRound: samples,
    latency,
    memory,
    parts,
    concurrency,
    loadAverage: {
      first: loads[0]?.before,
      last: loads.at(-1)?.after,
      min: Math.min(...loads.flatMap((l) => [l.before, l.after])),
      max: Math.max(...loads.flatMap((l) => [l.before, l.after])),
    },
    rounds: loads,
    fedDeclines: after.fed.route?.declined ?? {},
    targets: {
      p50: { bar: 0.6, measured: latency.fed.p50Ms, met: latency.fed.p50Ms <= 0.6 },
      p95: { bar: 1.0, measured: latency.fed.p95Ms, met: latency.fed.p95Ms <= 1.0 },
      rss: { barMiB: 25, overOffMiB: overOff, met: overOff <= 25 },
    },
  };
  fs.writeFileSync(out, JSON.stringify(result, null, 2) + '\n');
  console.log(
    `R2 Status under Node: p50/p95 ms off ${latency.off.p50Ms}/${latency.off.p95Ms}, on ${latency.on.p50Ms}/${latency.on.p95Ms}, fed ${latency.fed.p50Ms}/${latency.fed.p95Ms}; combined RSS fed ${overOff >= 0 ? '+' : ''}${overOff} MiB over off`
  );
} finally {
  for (const worker of workers) {
    try {
      worker.child.send({ type: 'stop' });
    } catch {
      /* Already gone. */
    }
  }
  await new Promise((r) => setTimeout(r, 800));
  for (const worker of workers) if (worker.child.exitCode === null) worker.child.kill('SIGKILL');
  fs.rmSync(scratch, { recursive: true, force: true });
}
