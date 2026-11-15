/**
 * L1 service lifecycle against the Nas1-built aac-http, under Node (the installed runtime), through
 * the built bridge in dist/. Run after `bun run build:server` with the binary in
 * dist/native/aac-http (or AAC_RUST_SERVICE_BINARY). Fixture roots only; the only processes it
 * signals are the service children it started. Each check failed against 7bc46ca90:
 *   keep-alive      one connection carries the requests (it was a new socket per call);
 *   restart         a killed service is restarted after its 1 s backoff and serves again;
 *   upgrade         a front door does not attach to a boot of another build;
 *   long root       a root too long for a Unix socket path still starts;
 *   sweep           boot folders of killed owners are removed;
 *   declines        every decline is counted per route with its named reason;
 *   feeds, events   the front door pushes a generation and hears `facts-needed`;
 *   hung            a stopped service is retired after three timeouts and restarted (review L3);
 *   crash, shadow   a crash while shadow awaits the TS read keeps the TS answer (review M1).
 * The deadline under a stuck read is checked in crates/aac-http/tests/lifecycle.rs, where the
 * slow-read shim can be preloaded (the front door starts the service with a clean environment).
 *
 *   node tests/spec/rust-lifecycle.mjs [--ts-only]
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const repo = fileURLToPath(new URL('../..', import.meta.url));
const dist = process.env.AAC_SPEC_DIST ?? path.join(repo, 'dist');
const binary = process.env.AAC_RUST_SERVICE_BINARY ?? path.join(dist, 'native/aac-http');
const require = createRequire(import.meta.url);
const scratch = fs.mkdtempSync(path.join(process.env.TMPDIR ?? os.tmpdir(), 'aac-l1-spec-'));
const services = [];

function root(...parts) {
  const dir = path.join(scratch, ...parts, '.ccs');
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  const real = fs.realpathSync(dir);
  process.env.CCS_HOME = path.dirname(real);
  process.env.CCS_DIR = real;
  return real;
}

const { RustService } = require(path.join(dist, 'web-server/rust-service.js'));
const transport = require(path.join(dist, 'web-server/rust-service-transport.js'));

async function start(mode = 'on', file = binary) {
  const service = new RustService(mode);
  services.push(service);
  await service.start(file);
  return service;
}

const req = {
  method: 'GET',
  originalUrl: '/api/accounts/settings',
  sessionID: 'fixture-session',
  session: { authenticated: true },
  headers: {},
};

async function settings(service) {
  let body;
  await service.respond(req, { json: (v) => (body = v) }, '/api/accounts/settings', () => ({
    ts: true,
  }));
  return body;
}

async function until(check, ms = 10_000) {
  const end = Date.now() + ms;
  while (!check()) {
    if (Date.now() > end) throw new Error('timed out');
    await new Promise((r) => setTimeout(r, 25));
  }
}

/** Timings, sizes and the build vary per run; the counters and states are the golden. */
function masked(value, key = '') {
  if (Array.isArray(value)) return value.map((v) => masked(v));
  if (value && typeof value === 'object')
    return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, masked(v, k)]));
  if (['p50Ms', 'p95Ms'].includes(key) && value !== null) return '<ms>';
  if (key === 'rssBytes' && value !== null) return '<bytes>';
  if (key === 'sha256') return '<sha256>';
  if (key === 'commit') return '<commit>';
  return value;
}

const checks = {
  async keepAlive() {
    const ccs = root('keep-alive');
    fs.writeFileSync(path.join(ccs, 'account-refresh-settings.json'), '{"refreshIntervalSeconds":90}');
    const service = await start();
    const agent = service.lease?.connection.agent;
    assert.ok(agent, 'one keep-alive agent per boot');
    let connections = 0;
    const create = agent.createConnection.bind(agent);
    agent.createConnection = (...args) => {
      connections++;
      return create(...args);
    };
    for (let i = 0; i < 20; i++) assert.deepEqual(await settings(service), { refreshIntervalSeconds: 90 });
    assert.ok(connections <= 1, `20 requests opened ${connections} connections`);
    assert.equal(service.stats.served, 20);
  },

  async restart() {
    const ccs = root('restart');
    fs.writeFileSync(path.join(ccs, 'account-refresh-settings.json'), '{"refreshIntervalSeconds":90}');
    const service = await start();
    const first = service.lease;
    assert.ok(first?.owned && first.pid, 'the front door owns its boot');
    const began = Date.now();
    process.kill(first.pid, 'SIGKILL');
    await until(() => service.lease && service.lease !== first);
    const waited = Date.now() - began;
    assert.ok(waited >= 900, `restarted after the 1 s backoff (${waited} ms)`);
    assert.notEqual(service.lease.connection.bootId, first.connection.bootId);
    assert.deepEqual(await settings(service), { refreshIntervalSeconds: 90 });
    assert.equal(service.diagnostics().service.restarts, 1);
  },

  async upgrade() {
    const ccs = root('upgrade');
    fs.writeFileSync(path.join(ccs, 'account-refresh-settings.json'), '{"refreshIntervalSeconds":90}');
    const copy = path.join(scratch, 'aac-http-upgrade');
    fs.copyFileSync(binary, copy);
    fs.chmodSync(copy, 0o755);
    const owner = await start('on', copy);
    const same = await start('on', copy);
    assert.equal(same.lease?.owned, false, 'the same build attaches');
    assert.equal(same.lease.connection.bootId, owner.lease.connection.bootId);
    // An in-place upgrade renames new bytes over the path (a running image cannot be rewritten);
    // the running boot keeps the old ones.
    const next = `${copy}.next`;
    fs.copyFileSync(copy, next);
    fs.appendFileSync(next, Buffer.from([0]));
    fs.chmodSync(next, 0o755);
    fs.renameSync(next, copy);
    const upgraded = await start('on', copy);
    assert.equal(upgraded.lease?.owned, true, 'a boot of another build is never attached');
    assert.notEqual(upgraded.lease.connection.bootId, owner.lease.connection.bootId);
    assert.notEqual(upgraded.lease.connection.build.sha256, owner.lease.connection.build.sha256);
    assert.deepEqual(await settings(owner), { refreshIntervalSeconds: 90 });
    assert.deepEqual(await settings(upgraded), { refreshIntervalSeconds: 90 });
  },

  async longRoot() {
    const ccs = root('l'.repeat(70), 'm'.repeat(70));
    fs.writeFileSync(path.join(ccs, 'account-refresh-settings.json'), '{"refreshIntervalSeconds":120}');
    assert.ok(Buffer.byteLength(path.join(ccs, 'run/aac-http-' + 'a'.repeat(24) + '/http.sock')) > 108);
    const service = await start();
    assert.ok(service.lease, 'an over-long root still starts');
    assert.ok(Buffer.byteLength(service.lease.connection.socketPath) <= 100);
    assert.deepEqual(await settings(service), { refreshIntervalSeconds: 120 });
  },

  async sweep() {
    const ccs = root('sweep');
    const run = path.join(ccs, 'run');
    fs.mkdirSync(run, { mode: 0o700 });
    const stale = path.join(run, 'aac-http-' + 'f'.repeat(24));
    fs.mkdirSync(stale, { mode: 0o700 });
    const old = new Date(Date.now() - 10 * 60_000);
    fs.utimesSync(stale, old, old);
    await start();
    assert.equal(fs.existsSync(stale), false, 'the killed owner’s boot folder is swept');
  },

  async declines() {
    const ccs = root('declines');
    const file = path.join(ccs, 'account-refresh-settings.json');
    const service = await start();
    for (const [text, expected] of [
      ['{"refreshIntervalSeconds":5}', { ts: true }],
      ['{"refreshIntervalSeconds":60,"x":1}', { ts: true }],
      ['{"refreshIntervalSeconds":60}', { refreshIntervalSeconds: 60 }],
    ]) {
      fs.writeFileSync(file, text);
      assert.deepEqual(await settings(service), expected);
    }
    fs.rmSync(file);
    assert.deepEqual(await settings(service), { ts: true });
    const route = service.diagnostics().routes['/api/accounts/settings'];
    assert.deepEqual(route.declined, { number: 1, shape: 1, unsupported: 1 });
    assert.equal(route.served, 1);
    assert.equal(route.state, 'declined:unsupported');
    // The diag golden: /api/diag?bridge=1's block for this sequence, timings and build masked.
    const golden = path.join(repo, 'tests/spec/contract/l1-bridge-diag.json');
    const bytes = JSON.stringify(masked(service.diagnostics()), null, 2) + '\n';
    if (process.argv.includes('--regenerate')) fs.writeFileSync(golden, bytes);
    else assert.equal(bytes, fs.readFileSync(golden, 'utf8'), 'bridge diagnostics golden');
  },

  // Review L3: a process that accepts but never answers is retired and restarted.
  async hung() {
    const ccs = root('hung');
    fs.writeFileSync(path.join(ccs, 'account-refresh-settings.json'), '{"refreshIntervalSeconds":90}');
    const service = await start();
    const first = service.lease;
    assert.deepEqual(await settings(service), { refreshIntervalSeconds: 90 });
    process.kill(first.pid, 'SIGSTOP');
    try {
      for (let i = 0; i < 3; i++) assert.deepEqual(await settings(service), { ts: true });
      await until(() => service.lease && service.lease !== first);
      assert.deepEqual(await settings(service), { refreshIntervalSeconds: 90 });
      assert.equal(service.diagnostics().service.restarts, 1);
    } finally {
      for (const signal of ['SIGCONT', 'SIGKILL'])
        try {
          process.kill(first.pid, signal);
        } catch {
          /* Already gone. */
        }
    }
  },

  // Review M1: the service dies while shadow awaits the TS read; the TS answer still goes out.
  async crashDuringShadow() {
    const ccs = root('crash-shadow');
    const service = await start('shadow');
    const first = service.lease;
    let began;
    let finish;
    const legacyStarted = new Promise((resolve) => (began = resolve));
    const held = new Promise((resolve) => (finish = resolve));
    const route = '/api/claude/desktop-profiles';
    const answer = service.ownedValue(
      { ...req, originalUrl: route },
      route,
      ccs,
      (body) => ({ value: body }),
      () => {
        began();
        return held;
      }
    );
    await legacyStarted;
    process.kill(first.pid, 'SIGKILL');
    await until(() => service.lease !== first);
    finish({ ts: 'successful' });
    assert.deepEqual(await answer, { ts: 'successful' });
    await until(() => (service.shadowInFlight.get(route) ?? 0) === 0, 2000);
    assert.equal(service.inFlight, 0);
  },

  async feeds() {
    root('feeds');
    const service = await start();
    const events = [];
    const stream = transport.openRustEvents(service.lease.connection, (e) => events.push(e));
    await stream.ready;
    const reply = await transport.pushRustFeed(
      service.lease.connection,
      'home',
      { generation: 'g1', data: { rows: [1] }, facts: null },
      new Set()
    );
    assert.equal(reply.status, 200);
    await until(() => events.length > 0, 3000);
    assert.deepEqual(events[0], { event: 'facts-needed', source: 'home', generation: 'g1' });
    stream.close();
  },
};

try {
  if (process.argv.includes('--ts-only')) {
    root('off');
    const service = await start('off');
    assert.equal(service.lease, null);
    assert.deepEqual(service.diagnostics().service.state, 'off');
    console.log('rust-lifecycle: off mode starts no service');
  } else {
    assert.ok(fs.existsSync(binary), 'needs the Nas1-built aac-http in dist/native');
    const failed = [];
    for (const [name, check] of Object.entries(checks)) {
      try {
        await check();
        console.log(`rust-lifecycle: ${name} passed`);
      } catch (error) {
        failed.push(name);
        console.log(`rust-lifecycle: ${name} FAILED: ${String(error?.message ?? error).split('\n')[0]}`);
      }
      for (const service of services.splice(0)) service.stop();
      await new Promise((r) => setTimeout(r, 100));
    }
    assert.deepEqual(failed, [], 'every lifecycle check passes');
    console.log(`rust-lifecycle: ${Object.keys(checks).length} checks passed under Node ${process.version}`);
  }
} finally {
  for (const service of services.splice(0)) service.stop();
  await new Promise((r) => setTimeout(r, 300));
  // Short socket folders a long root made under the runtime folder.
  const { shortName } = require(path.join(dist, 'web-server/rust-service-lifecycle.js'));
  for (const base of [process.env.XDG_RUNTIME_DIR, `/tmp/aac-${process.getuid()}`])
    if (base && fs.existsSync(base))
      for (const name of fs.readdirSync(base))
        for (const dir of fs.readdirSync(scratch, { recursive: true }))
          if (String(dir).endsWith('.ccs') && name.includes(shortName(fs.realpathSync(path.join(scratch, dir)))))
            fs.rmSync(path.join(base, name), { recursive: true, force: true });
  fs.rmSync(scratch, { recursive: true, force: true });
}
