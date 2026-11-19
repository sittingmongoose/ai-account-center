/**
 * R4 owned-path parity worker: one front door (built dist/) over a fixture share, in one of two
 * modes — `today` (the TypeScript readers and the envelope Scoreboard) or `owned` (the Rust
 * service reads the share, ingests the runs and answers both views). Forked by
 * owned-read-parity.mjs with its own CCS home; answers IPC commands and serves HTTP.
 *
 *   node tests/spec/owned-read-worker.cjs <dist> <binary> <today|owned> <home>
 * Fixture data only; the processes it starts are its own.
 */
'use strict';
const fs = require('fs');
const path = require('path');

const [dist, binary, mode, home] = process.argv.slice(2);
const repo = path.resolve(__dirname, '../..');
const ccs = path.join(home, '.ccs');
const share = path.join(home, 'share');
for (const key of Object.keys(process.env))
  if (key.startsWith('CCS_') || key.startsWith('AAC_RUST') || key.startsWith('AAC_STORE')) delete process.env[key];
Object.assign(process.env, {
  CCS_HOME: home,
  CCS_DIR: ccs,
  CCS_DASHBOARD_AUTH_ENABLED: 'true',
  AAC_NO_LOCAL_RUST_BUILD: '1',
  AAC_AGENT_REPORTS_DIR: share,
  AAC_PM_MAIL_REGISTRY_DIR: path.join(home, 'registry'),
  NO_COLOR: '1',
  ...(mode === 'owned'
    ? {
        AAC_RUST_SERVICE: 'on',
        AAC_RUST_SERVICE_BINARY: binary,
        AAC_RUST_STORE: 'on',
        AAC_RUST_GROUP_STATUS: 'on',
        AAC_RUST_GROUP_SCOREBOARD: 'on',
      }
    : { AAC_RUST_SERVICE: 'off' }),
});
fs.mkdirSync(ccs, { recursive: true, mode: 0o700 });

const require2 = (file) => require(path.join(dist, file));
const express = require(path.join(repo, 'node_modules/express'));
const { RustService } = require2('web-server/rust-service.js');
const { setStatusRustService } = require2('web-server/status/status-service.js');
const { StatusService } = require2('web-server/status/status-service.js');
const { createStatusRouter } = require2('web-server/routes/status-routes.js');
const { createScoreboardRouter } = require2('web-server/routes/scoreboard-routes.js');
const {
  getScoreboardService,
  ALL_RUNS,
} = require2('web-server/scoreboard/scoreboard-service.js');
const { getAacStore, openAacStore } = require2('store/store-service.js');
const { requestRust, FRONT_DOOR_PRINCIPAL } = require2('web-server/rust-service-transport.js');

const localSample = {
  cores: 8,
  load1: 1.25,
  busyPct: 12,
  memAvailableBytes: 16 * 1024 ** 3,
  memTotalBytes: 32 * 1024 ** 3,
  disk: { mount: '/', freeBytes: 200 * 1024 ** 3, totalBytes: 500 * 1024 ** 3 },
  cpuTimes: null,
};
const owner = new StatusService({
  historyStore: () => null,
  run: async () => ({ code: null, stdout: '', timedOut: false, missing: true }),
  localMachine: () => 'vm',
  readLocalMachine: async () => structuredClone(localSample),
  readRemoteMachine: async () => {
    throw new Error('Fixture host unavailable.');
  },
  readRemotePins: async () => '[]',
  notify: () => undefined,
  subscribeUsage: () => () => undefined,
  refreshSeconds: () => 60,
  spendNative: null,
});
const scoreboard = getScoreboardService();

const rust = new RustService(mode === 'owned' ? 'on' : 'off');
setStatusRustService(rust);

const app = express();
app.use((req, res, next) => {
  Object.assign(req, { session: { authenticated: true }, sessionID: 'fixture-owned-parity' });
  res.locals.rustService = rust;
  next();
});
app.use('/api/status', createStatusRouter(() => owner));
app.use('/api/scoreboard', createScoreboardRouter(() => scoreboard));

const send = (message) => process.send && process.send(message);

async function waitOwned(timeoutMs = 20000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const inputs = await rust.internalJson('/internal/status/inputs');
    if (inputs && typeof inputs.generation === 'number' && inputs.generation > 0) return inputs.generation;
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  throw new Error('the owned Status readers never produced a generation');
}

async function main() {
  if (mode === 'owned') {
    await rust.start(binary);
    if (!rust.lease) throw new Error('the Rust service did not start');
    owner.start();
    await waitOwned();
  } else {
    await openAacStore();
    await owner.scanReports();
    await owner.scanRegistry();
    await owner.collect();
    owner.start();
    // One ingest pass and a first view, so the store path is warm.
    await scoreboard.view(ALL_RUNS);
  }
  const server = app.listen(0, '127.0.0.1', () => {
    send({ ready: true, port: server.address().port, mode });
  });
  process.on('message', async (message) => {
    const send = (m) => process.send && process.send({ id: message.id, ...m });
    try {
      if (message.cmd === 'scan') {
        await owner.scanReports();
        await owner.scanRegistry();
        await owner.collect();
        await scoreboard.view(ALL_RUNS);
        send({ ok: true });
      } else if (message.cmd === 'wait-owned') {
        send({ ok: true, generation: await waitOwned() });
      } else if (message.cmd === 'audit') {
        if (mode === 'owned') {
          const lease = rust.lease;
          const reply = await requestRust(
            lease.connection,
            '/internal/store/runsAudit',
            FRONT_DOOR_PRINCIPAL,
            new Set(),
            {},
            'POST'
          );
          send({ ok: reply.status === 200, data: reply.body });
        } else {
          const store = getAacStore();
          send({ ok: true, data: store ? await store.runs('runsAudit') : null });
        }
      } else if (message.cmd === 'check') {
        const dir = path.join(share, 'v1', 'readers', 'aac');
        const files = {};
        if (fs.existsSync(dir))
          for (const name of fs.readdirSync(dir))
            if (name.endsWith('.json')) files[name] = fs.readFileSync(path.join(dir, name), 'utf8');
        send({ ok: true, files });
      } else if (message.cmd === 'diag') {
        send({ ok: true, data: rust.diagnostics(), lease: !!rust.lease });
      } else if (message.cmd === 'inputs') {
        send({
          ok: true,
          status: await rust.internalJson('/internal/status/inputs'),
          scoreboard: await rust.internalJson('/internal/scoreboard/inputs'),
        });
      } else if (message.cmd === 'stop-rust') {
        rust.stop();
        send({ ok: true });
      } else if (message.cmd === 'stop') {
        server.close();
        rust.stop();
        process.exit(0);
      }
    } catch (error) {
      send({ ok: false, error: String(error && error.message ? error.message : error) });
    }
  });
}
main().catch((error) => {
  send({ ready: false, error: String(error && error.stack ? error.stack : error) });
  process.exit(1);
});
