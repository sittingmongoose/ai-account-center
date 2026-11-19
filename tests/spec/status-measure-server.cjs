/**
 * R2 measurement worker: GET /api/status from a built dist/ under Node, the way read-http-parity serves it (the
 * Status router behind a signed-in session, no other middleware), over the real StatusService owner on a large
 * fixture share. The owner runs as installed (its 10 s report scans, 30 s registry scans and the collection
 * timer), with the process collectors, the pm-mail registry and the pins stubbed, so nothing outside the fixture
 * is read or started. Forked by status-measure.mjs; answers IPC `stats` and `stop`.
 *
 *   node tests/spec/status-measure-server.cjs <dist> <binary> <off|on|fed|owned> <home>
 *   - off: the service off (today's TS path, the reference);
 *   - on: mode on with the Status group off (today's M3 envelope path);
 *   - fed: mode on with the Status group on (R2);
 *   - owned: R4, the store host and the Status and Scoreboard groups on: the service reads the share, ingests
 *     the runs and answers both routes (the Scoreboard router is mounted too).
 */
'use strict';
const fs = require('fs');
const path = require('path');
const { monitorEventLoopDelay, performance } = require('perf_hooks');

const [dist, binary, mode, home] = process.argv.slice(2);
const owned = mode === 'owned';
const repo = path.resolve(__dirname, '../..');
const ccs = path.join(home, '.ccs');
fs.mkdirSync(ccs, { recursive: true, mode: 0o700 });
for (const key of Object.keys(process.env))
  if (key.startsWith('CCS_') || key.startsWith('AAC_RUST')) delete process.env[key];
Object.assign(process.env, {
  CCS_HOME: home,
  CCS_DIR: ccs,
  CCS_DASHBOARD_AUTH_ENABLED: 'true',
  AAC_NO_LOCAL_RUST_BUILD: '1',
  AAC_AGENT_REPORTS_DIR: path.join(home, 'share'),
  AAC_PM_MAIL_REGISTRY_DIR: path.join(home, 'registry'),
  AAC_RUST_GROUP_STATUS: mode === 'fed' || owned ? 'on' : 'off',
  AAC_RUST_STORE: owned ? 'on' : 'off',
  AAC_RUST_GROUP_SCOREBOARD: owned ? 'on' : 'off',
  NO_COLOR: '1',
});
const load = (file) => require(path.join(dist, file));
const express = require(path.join(repo, 'node_modules/express'));
const { RustService } = load('web-server/rust-service.js');
const { createStatusRouter } = load('web-server/routes/status-routes.js');
const { StatusService, setStatusRustService } = load('web-server/status/status-service.js');

const sample = {
  cores: 56,
  load1: 21.25,
  busyPct: 38,
  memAvailableBytes: 16 * 1024 ** 3,
  memTotalBytes: 38 * 1024 ** 3,
  disk: { mount: '/', freeBytes: 200 * 1024 ** 3, totalBytes: 500 * 1024 ** 3 },
  cpuTimes: null,
};
const owner = new StatusService({
  registryReader: {
    scan: async (now) => ({
      threads: Array.from({ length: 40 }, (_, i) => ({
        name: `fixture-thread-${i}`,
        kind: i % 2 ? 'codex' : 'claude',
        machine: ['vm', 'mac', 'nas1'][i % 3],
        about: `Fixture work item ${i}`,
        lastSeen: new Date(now - i * 60_000).toISOString(),
        readAt: now - 5_000,
        problem: null,
      })),
      problems: [],
      changed: false,
    }),
  },
  historyStore: () => null,
  run: async () => ({ code: 1, stdout: '', timedOut: false, missing: true }),
  localMachine: () => 'vm',
  readLocalMachine: async () => structuredClone(sample),
  readRemoteMachine: async () => structuredClone(sample),
  readRemotePins: async () => '[]',
  timeZone: () => 'America/New_York',
  notify: () => undefined,
  subscribeUsage: () => () => undefined,
  refreshSeconds: () => 60,
  pinned: {
    view: () => ({ unavailable: 'the pinned threads have not been read yet' }),
    scanStatusDocs: async () => undefined,
    compose: async () => undefined,
    collect: async () => undefined,
    docs: { snapshot: () => ({ docs: [] }) },
  },
  spendNative: null,
});

const timing = { readInputs: [], rust: [] };
const readInputs = owner.readInputs.bind(owner);
owner.readInputs = () => {
  const started = performance.now();
  const value = readInputs();
  timing.readInputs.push(performance.now() - started);
  return value;
};

async function verify(service) {
  const { answerStatusFed } = load('web-server/status/status-feed.js');
  const { buildStatusView } = load('web-server/status/status-view.js');
  const { getCcsDir } = load('utils/config-manager.js');
  const req = {
    method: 'GET',
    sessionID: 'fixture-measure-session',
    session: { authenticated: true },
    get: () => undefined,
  };
  let bytes = null;
  const res = {
    req,
    setHeader: () => undefined,
    vary: () => undefined,
    status: () => res,
    end: () => undefined,
    send: (body) => {
      bytes = body;
    },
  };
  const input = readInputs();
  const served = await answerStatusFed(req, res, service, input, getCcsDir());
  const expected = JSON.stringify(buildStatusView(input));
  return {
    served,
    equal: served ? bytes?.toString('utf8') === expected : null,
    bytes: expected.length,
  };
}

async function main() {
  const service = new RustService(mode === 'off' ? 'off' : 'on');
  if (mode !== 'off') {
    await service.start(binary);
    if (!service.lease) throw new Error('the Rust service did not start');
    const fedView = service.fedView.bind(service);
    service.fedView = async (...args) => {
      const started = performance.now();
      const reply = await fedView(...args);
      timing.rust.push(performance.now() - started);
      return reply;
    };
  }
  let stopOwner;
  if (owned) {
    // R4: the owner forwards the service's owned snapshots; wait for the first owned scan of the share.
    setStatusRustService(service);
    stopOwner = owner.start();
    const deadline = Date.now() + 120000;
    for (;;) {
      const inputs = await service.internalJson('/internal/status/inputs');
      if (inputs && typeof inputs.generation === 'number' && inputs.generation > 0) break;
      if (Date.now() > deadline) throw new Error('the owned Status readers never produced a generation');
      await new Promise((r) => setTimeout(r, 200));
    }
    await owner.collect();
  } else {
    await owner.scanReports();
    await owner.scanRegistry();
    await owner.collect();
    stopOwner = owner.start();
  }
  const app = express();
  app.use((req, res, next) => {
    Object.assign(req, { session: { authenticated: true }, sessionID: 'fixture-measure-session' });
    res.locals.rustService = service;
    next();
  });
  app.use(
    '/api/status',
    createStatusRouter(() => owner)
  );
  if (owned) {
    const { createScoreboardRouter } = load('web-server/routes/scoreboard-routes.js');
    const { getScoreboardService } = load('web-server/scoreboard/scoreboard-service.js');
    const scoreboard = getScoreboardService();
    app.use(
      '/api/scoreboard',
      createScoreboardRouter(() => scoreboard)
    );
  }
  const server = app.listen(0, '127.0.0.1', () =>
    process.send({ type: 'ready', port: server.address().port })
  );
  const loop = monitorEventLoopDelay({ resolution: 1 });
  loop.enable();
  const rss = (pid) => {
    try {
      const match = /^VmRSS:\s+(\d+) kB/m.exec(fs.readFileSync(`/proc/${pid}/status`, 'utf8'));
      return match ? Number(match[1]) * 1024 : null;
    } catch {
      return null;
    }
  };
  const quantile = (values, q) => {
    if (!values.length) return null;
    const sorted = [...values].sort((a, b) => a - b);
    return Number(sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))].toFixed(3));
  };
  process.on('message', (message) => {
    if (message.type === 'stats') {
      global.gc?.();
      const diag = service.diagnostics();
      process.send({
        type: 'stats',
        id: message.id,
        tsRssBytes: rss(process.pid),
        rustRssBytes: service.lease?.pid ? rss(service.lease.pid) : 0,
        eventLoopP99Ms: Number((loop.percentile(99) / 1e6).toFixed(3)),
        readInputs: {
          p50Ms: quantile(timing.readInputs, 0.5),
          p95Ms: quantile(timing.readInputs, 0.95),
        },
        rustPart: { p50Ms: quantile(timing.rust, 0.5), p95Ms: quantile(timing.rust, 0.95) },
        route: diag.routes?.['/api/status'] ?? null,
        scoreboardRoute: diag.routes?.['/api/scoreboard'] ?? null,
        stats: diag.stats,
      });
      if (message.reset) {
        loop.reset();
        timing.readInputs = [];
        timing.rust = [];
      }
    } else if (message.type === 'scoreboard-ready') {
      // Data-aware: the owned store holds the fixture's runs (the ingest backfills in passes).
      const deadline = Date.now() + 600000;
      // Done at the fixture's count, or when the count holds for 5 polls (a refused run never lands).
      const poll = async () => {
        let last = -1;
        let held = 0;
        for (;;) {
          const inputs = await service.internalJson('/internal/scoreboard/inputs');
          const runs = Array.isArray(inputs?.runs?.runs) ? inputs.runs.runs.length : 0;
          held = runs > 0 && runs === last ? held + 1 : 0;
          last = runs;
          if (inputs?.catalog && (runs >= message.runs || held >= 5)) return runs;
          if (Date.now() > deadline) throw new Error(`the owned ingest reached ${runs} runs`);
          await new Promise((r) => setTimeout(r, 1000));
        }
      };
      poll().then(
        (runs) => process.send({ type: 'stats', id: message.id, runs }),
        (error) => process.send({ type: 'stats', id: message.id, error: String(error) })
      );
    } else if (message.type === 'verify') {
      // The fed path against today's builder on one read: the same bytes, or a decline (today's answer).
      verify(service).then(
        (result) => process.send({ type: 'stats', id: message.id, ...result }),
        (error) => process.send({ type: 'stats', id: message.id, error: String(error) })
      );
    } else if (message.type === 'stop') {
      stopOwner();
      server.close();
      service.stop();
      setTimeout(() => process.exit(0), 200);
    }
  });
}
main().catch((error) => {
  process.send({ type: 'error', message: String(error?.stack ?? error) });
  process.exit(1);
});
