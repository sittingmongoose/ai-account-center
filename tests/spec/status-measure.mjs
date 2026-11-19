/**
 * R2 measurement under Node (the installed runtime): GET /api/status end to end on the large synthetic home
 * (large-home.mjs: 200 agent reports of 50 writers), today's TS path (off), today's M3 envelope path (on) and the
 * fed path (R2), in alternating rounds (ABAB: 10 rounds of 40 requests per server by default) so drift on a loaded
 * host cancels. Each server is its own process (status-measure-server.cjs) on its own copy of the home, with the
 * Status owner running its scan timers. Records p50/p95/p99, the load average before and after each round, RSS of
 * TS and of the Rust child after the rounds, the event-loop p99, the owner's `readInputs` time and the fed path's
 * Rust part (the hop and the service's answer), and evaluates the design's R2 targets. Every fed reply in the
 * rounds must be the fed answer (the route's fed-only `fedServed` counter), and the fed and off answers must
 * agree (ETag at the same second, checked before the rounds).
 *
 *   node tests/spec/status-measure.mjs --dist <dist> --binary <aac-http> --out <file.json>
 *        [--rounds 10] [--samples 40] [--clients 16] [--seconds 20] [--base <aac-http>]
 * `--base` adds today's envelope path on another build's service (`on:base`, the before of the composer).
 * `--owned` adds R4's owned server (store host, Status and Scoreboard groups on) to the rounds, with its own Status
 * targets, and then measures its Scoreboard route over the large home's runs once ingested: cached (one URL
 * repeated inside the view cache's 60 s) and uncached (a new filter each request) against DESIGN 1.4's bars.
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
const withOwned = args.includes('--owned');
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
      mode: label,
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
    child.on('exit', (code, signal) => {
      worker.exited = { code, signal, stderr: stderr.slice(-2000) };
      for (const resolve of worker.pending.values()) resolve({ exited: worker.exited });
      worker.pending.clear();
      if (!worker.port) reject(new Error(`${label} server exited ${code ?? signal}: ${stderr}`));
    });
    // A send to a server that has gone is an error event, not a throw: it is recorded by the exit above.
    child.on('error', () => undefined);
  });
}
let asked = 0;
function ask(worker, type, extra = {}) {
  const id = ++asked;
  return new Promise((resolve) => {
    if (worker.exited) return resolve({ exited: worker.exited });
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
  if (withOwned) home('owned');
  const servers = [
    await start('off', path.join(scratch, 'off')),
    await start('on', path.join(scratch, 'on')),
    await start('fed', path.join(scratch, 'fed')),
    ...(base ? [await start('on', path.join(scratch, 'base'), 'on:base', path.resolve(base))] : []),
    ...(withOwned ? [await start('owned', path.join(scratch, 'owned'))] : []),
  ];
  const ownedServer = servers.find((w) => w.mode === 'owned') ?? null;
  // Warm up, and check the fed server's answers: on one read, the fed path's bytes are today's builder's.
  for (const worker of servers)
    for (let i = 0; i < 20; i++) assert.equal((await call(worker)).status, 200);
  const verified = [];
  for (let i = 0; i < 20; i++) verified.push(await ask(servers[2], 'verify'));
  assert.ok(
    verified.every((v) => v.served && v.equal),
    `the fed path serves today's bytes: ${JSON.stringify(verified.slice(0, 3))}`
  );
  if (ownedServer) {
    const ownedVerified = [];
    for (let i = 0; i < 20; i++) ownedVerified.push(await ask(ownedServer, 'verify'));
    assert.ok(
      ownedVerified.every((v) => v.served && v.equal),
      `the owned path serves the builder's bytes: ${JSON.stringify(ownedVerified.slice(0, 3))}`
    );
  }
  const sizes = Object.fromEntries(
    await Promise.all(servers.map(async (w) => [w.mode, (await call(w)).body.length]))
  );
  for (const worker of servers) await stats(worker, true);
  const ownedBefore = ownedServer ? ((await stats(ownedServer)).route?.fedServed ?? 0) : 0;
  const fedStatsBefore = await stats(servers[2]);
  const fedBefore = fedStatsBefore.route?.fedServed ?? 0;
  const routeBefore = fedStatsBefore.route?.served ?? 0;

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
  const fedServed = (after.fed.route?.fedServed ?? 0) - fedBefore;
  assert.equal(
    fedServed,
    rounds * samples,
    `every fed request in the rounds was served by the fed path (${fedServed})`
  );
  const ownedServed = ownedServer ? (after.owned.route?.fedServed ?? 0) - ownedBefore : null;
  if (ownedServer)
    assert.equal(ownedServed, rounds * samples, `every owned request was served (${ownedServed})`);
  // The envelope fallback's successes on the same route, apart from the fed path's (R2b L2).
  const envelopeServed = (after.fed.route?.served ?? 0) - routeBefore - fedServed;

  let concurrency = null;
  if (clients > 0) {
    concurrency = {};
    for (const worker of servers) {
      const before = await stats(worker, true);
      const latencies = [];
      let errors = 0;
      const end = Date.now() + seconds * 1000;
      await Promise.all(
        Array.from({ length: clients }, async () => {
          while (Date.now() < end && !worker.exited) {
            try {
              const reply = await call(worker);
              if (reply.status === 200) latencies.push(reply.ms);
              else errors++;
            } catch {
              errors++;
            }
          }
        })
      );
      const s = await stats(worker);
      concurrency[worker.mode] = {
        requests: latencies.length,
        errors,
        ...(latencies.length ? quantiles(latencies) : {}),
        eventLoopP99Ms: s.eventLoopP99Ms ?? null,
        // The fed server: fed-only serves, the envelope fallback's serves, and the fed path's own
        // named declines (Rust's `busy` when its pool is full).
        ...(worker.mode === 'fed'
          ? {
              fedServed: (s.route?.fedServed ?? 0) - (before.route?.fedServed ?? 0),
              envelopeServed:
                (s.route?.served ?? 0) -
                (before.route?.served ?? 0) -
                ((s.route?.fedServed ?? 0) - (before.route?.fedServed ?? 0)),
              fedDeclined: s.route?.fedDeclined ?? {},
            }
          : {}),
      };
    }
  }

  const latency = Object.fromEntries(servers.map((w) => [w.mode, quantiles(times[w.mode])]));

  // R4 Scoreboard on the owned server: the large home's runs ingested first (data-aware), then cached and
  // uncached reads; every read must be the owned answer (x-aac-view) and none may decline.
  let scoreboard = null;
  if (ownedServer) {
    const ready = await ask(ownedServer, 'scoreboard-ready', { runs: fixture.runs ?? 1 });
    assert.ok(!ready.error, ready.error);
    const get = (route) =>
      new Promise((resolve, reject) => {
        const started = process.hrtime.bigint();
        http
          .get({ host: '127.0.0.1', port: ownedServer.port, path: route, agent: ownedServer.agent }, (response) => {
            const chunks = [];
            response.on('data', (c) => chunks.push(c));
            response.on('end', () =>
              resolve({
                status: response.statusCode,
                view: response.headers['x-aac-view'] ?? null,
                bytes: Buffer.concat(chunks).length,
                ms: Number(process.hrtime.bigint() - started) / 1e6,
              })
            );
          })
          .on('error', reject);
      });
    const day = (offset) => new Date(Date.now() - offset * 86400000).toISOString().slice(0, 10);
    const cached = [];
    const uncached = [];
    let notOwned = 0;
    let bytes = 0;
    await get('/api/scoreboard');
    for (let round = 0; round < rounds; round++) {
      for (let i = 0; i < samples; i++) {
        const reply = await get('/api/scoreboard');
        assert.equal(reply.status, 200);
        if (reply.view !== 'scoreboard@1') notOwned++;
        cached.push(reply.ms);
        bytes = reply.bytes;
      }
      for (let i = 0; i < Math.max(1, Math.floor(samples / 4)); i++) {
        // A filter not asked before: a new view-cache key, so the view is composed from the store.
        const n = round * samples + i;
        const reply = await get(`/api/scoreboard?from=${day(40 + (n % 30))}&to=${day(n % 10)}&source=${['all', 'real', 'benchmark'][Math.floor(n / 30) % 3]}`);
        assert.equal(reply.status, 200);
        if (reply.view !== 'scoreboard@1') notOwned++;
        uncached.push(reply.ms);
      }
    }
    const s = await stats(ownedServer);
    scoreboard = {
      runsIngested: ready.runs,
      viewBytes: bytes,
      cached: quantiles(cached),
      uncached: quantiles(uncached),
      notOwned,
      declined: s.scoreboardRoute?.declined ?? {},
      targets: {
        cachedP95: { bar: 2, measured: quantiles(cached).p95Ms, met: quantiles(cached).p95Ms <= 2 },
        uncachedP95: { bar: 15, measured: quantiles(uncached).p95Ms, met: quantiles(uncached).p95Ms <= 15 },
      },
    };
  }
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
    fedServed,
    ownedServed,
    envelopeServed,
    scoreboard,
    fedDeclines: after.fed.route?.fedDeclined ?? {},
    serverExits: Object.fromEntries(workers.filter((w) => w.exited).map((w) => [w.mode, w.exited])),
    targets: {
      p50: { bar: 0.6, measured: latency.fed.p50Ms, met: latency.fed.p50Ms <= 0.6 },
      p95: { bar: 1.0, measured: latency.fed.p95Ms, met: latency.fed.p95Ms <= 1.0 },
      rss: { barMiB: 25, overOffMiB: overOff, met: overOff <= 25 },
      ...(latency.owned
        ? {
            ownedP50: { bar: 0.6, measured: latency.owned.p50Ms, met: latency.owned.p50Ms <= 0.6 },
            ownedP95: { bar: 1.0, measured: latency.owned.p95Ms, met: latency.owned.p95Ms <= 1.0 },
          }
        : {}),
    },
  };
  fs.writeFileSync(out, JSON.stringify(result, null, 2) + '\n');
  console.log(
    `R2 Status under Node: p50/p95 ms off ${latency.off.p50Ms}/${latency.off.p95Ms}, on ${latency.on.p50Ms}/${latency.on.p95Ms}, fed ${latency.fed.p50Ms}/${latency.fed.p95Ms}${latency.owned ? `, owned ${latency.owned.p50Ms}/${latency.owned.p95Ms}` : ''}${scoreboard ? `; owned Scoreboard p95 cached ${scoreboard.cached.p95Ms} uncached ${scoreboard.uncached.p95Ms} ms over ${scoreboard.runsIngested} runs` : ''}; combined RSS fed ${overOff >= 0 ? '+' : ''}${overOff} MiB over off`
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
