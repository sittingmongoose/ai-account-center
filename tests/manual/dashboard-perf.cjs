/* Offline dashboard regression fixture. Node only; no native build, browser or provider access.
 * node --expose-gc tests/manual/dashboard-perf.cjs <checkout> <seconds> <result.json>
 * Uses the checkout's compiled server services and routes, injected synthetic history/collectors.
 * Control endpoints are probed every 250 ms; data views every 5 s (the settled UI uses 60 s).
 * AAC_PERF_DATA_POLL_MS=250 repeats the deliberately excessive full-body stress workload.
 */
const fs = require('fs');
const os = require('os');
const path = require('path');
const http = require('http');
const { fork } = require('child_process');
const { performance, monitorEventLoopDelay } = require('perf_hooks');
const [checkout, duration = '180', output] = process.argv.slice(2);
const load = (name) => require(path.join(checkout, 'dist', name));
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const percentile = (values, p) =>
  [...values].sort((a, b) => a - b)[
    Math.min(values.length - 1, Math.ceil(values.length * p) - 1)
  ] ?? 0;

async function server() {
  const root = process.env.CCS_HOME;
  const now = Date.now();
  const stamp = new Date(now).toISOString();
  const timelines = [];
  const memory = (phase) => {
    const row = { phase, elapsedSeconds: process.uptime(), ...process.memoryUsage() };
    timelines.push(row);
    return row;
  };
  const { AccountAnalyticsActivityService } = load(
    'web-server/services/account-analytics-activity'
  );
  const { AccountAnalyticsService } = load('web-server/services/account-analytics-service');
  const { accountAnalyticsIdentity } = load('web-server/services/account-analytics-history');
  const { ScoreboardService } = load('web-server/scoreboard/scoreboard-service');
  const { StatusService } = load('web-server/status/status-service');
  const { AgentReportsReader } = load('web-server/status/agent-reports-reader');
  const { runCommand } = load('web-server/status/status-collectors');
  const express = require(path.join(checkout, 'node_modules/express'));
  memory('imports');
  const accounts = Array.from({ length: 64 }, (_, i) => ({
    id: `codex:fixture-${i}`,
    provider: 'codex',
    providerLabel: 'Codex',
    label: `Fixture ${i}`,
    email: `fixture-${i}@example.test`,
    plan: 'paid',
    platform: 'ubuntu',
    source: 'fixture',
    status: 'ok',
    message: null,
    fetchedAt: stamp,
    sampledAt: stamp,
    isActive: i === 0,
    capabilities: { codexProfile: `fixture-${i}`, claudeProfileId: null, claudePlatforms: [] },
    windows: [300, 10080].map((minutes) => ({
      key: `window-${minutes}`,
      label: 'Fixture quota',
      usedPercent: 50,
      remainingPercent: 50,
      resetAt: stamp,
      windowMinutes: minutes,
      used: null,
      limit: null,
      remaining: null,
      unit: null,
    })),
  }));
  let history = {
    schemaVersion: 1,
    collectedSince: new Date(now - 30 * 86400000).toISOString(),
    records: [],
  };
  for (let hour = 720; hour >= 0; hour -= 3)
    for (const account of accounts) {
      const at = new Date(now - hour * 3600000).toISOString();
      history.records.push({
        identity: accountAnalyticsIdentity(account),
        accountId: account.id,
        sampledAt: at,
        observedAt: at,
        platform: account.platform,
        source: account.source,
        status: 'ok',
        isActive: account.isActive,
        windows: account.windows.map((w) => ({ ...w, usedPercent: hour % 100 })),
      });
    }
  const dashboard = {
    schemaVersion: 1,
    updatedAt: stamp,
    accounts,
    settings: { refreshIntervalSeconds: 60 },
  };
  const tools = ['claude', 'codex', 'omp', 'muse', 'zcode'];
  const sourceFiles = new Map();
  for (const tool of tools) {
    const models = [
      tool === 'claude' ? 'claude-sonnet-4-6' : 'gpt-5.4',
      'fixture-model-2',
      'fixture-model-3',
    ];
    const counts = {
      inputTokens: 1000,
      outputTokens: 500,
      cacheCreationTokens: 20,
      cacheReadTokens: 30,
      cost: 0.01,
      totalCost: 0.01,
    };
    const data = {
      daily: [],
      monthly: [],
      eventCount: 24000,
      hourly: Array.from({ length: 744 }, (_, i) => ({
        ...counts,
        hour: new Date(now - i * 3600000).toISOString().slice(0, 13).replace('T', ' ') + ':00',
        source: 'native',
        modelsUsed: models,
        requestCount: 32,
        modelBreakdowns: models.map((modelName) => ({ modelName, ...counts })),
      })),
      session: Array.from({ length: 2000 }, (_, i) => ({
        ...counts,
        sessionId: `${tool}-${i}`,
        projectPath: '/fixture/project',
        firstActivity: new Date(now - 29 * 86400000).toISOString(),
        lastActivity: stamp,
        versions: [],
        modelsUsed: models,
        modelBreakdowns: models.map((modelName) => ({ modelName, ...counts })),
        source: 'native',
      })),
    };
    const file = path.join(root, `${tool}-aggregate.json`);
    fs.writeFileSync(file, JSON.stringify(data));
    sourceFiles.set(tool, file);
  }
  const reportRoot = path.join(root, 'reports');
  for (let i = 0; i < 1702; i++) {
    const writer = `vm.fixture-${i % 38}`;
    const dir = path.join(reportRoot, 'v1', 'writers', writer, 'runs', stamp.slice(0, 10));
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(
      path.join(dir, `${i}.json`),
      JSON.stringify({
        schema: 'agent-reports/run@1',
        id: `${writer}--run-${i}`,
        writer,
        seq: 1,
        writtenAt: stamp,
        source: 'observed',
        project: 'fixture',
        stream: null,
        task: { id: `run-${i}`, type: 'build', title: 'Fixture run', size: { tasks: 1 } },
        model: { provider: 'openai', id: 'gpt-6.1-sol', harness: 'codex', effort: 'high' },
        startedAt: null,
        endedAt: stamp,
        wallSeconds: 100,
        outcome: 'done',
      })
    );
  }
  const activity = new AccountAnalyticsActivityService({
    scope: () => root,
    now: () => Date.now(),
    requests: () =>
      tools.map((provider) => ({ provider, request: { kind: provider, roots: ['/fixture'] } })),
    loadWorker: async (request) =>
      JSON.parse(fs.readFileSync(sourceFiles.get(request.kind), 'utf8')),
    remote: async () => ({ results: [], states: [] }),
    pricing: () => null,
  });
  const analytics = new AccountAnalyticsService({
    scope: () => root,
    usageStore: async () => null,
    createHistoryStore: () => ({
      read: async () => history,
      write: async (value) => {
        history = value;
      },
    }),
    getDashboard: async () => dashboard,
    getActivity: (...args) => activity.get(...args),
  });
  const scoreboard = new ScoreboardService({
    root: () => reportRoot,
    native: null,
    timeZone: () => 'UTC',
  });
  const status = new StatusService({
    reader: new AgentReportsReader({ root: () => reportRoot }),
    spendNative: null,
    pinned: {
      view: () => ({ unavailable: 'fixture' }),
      collect: async () => {},
      compose: async () => {},
      docs: { scan: async () => {}, snapshot: () => ({ docs: [] }) },
    },
    run: async () => ({ code: 1, stdout: '', timedOut: false, missing: true }),
    readLocalMachine: async () => ({
      cores: 4,
      load1: 0,
      busyPct: 0,
      memAvailableBytes: 1073741824,
      memTotalBytes: 2147483648,
      disk: null,
      cpuTimes: null,
    }),
    readRemoteMachine: async () => ({
      cores: 4,
      load1: 0,
      busyPct: 0,
      memAvailableBytes: 1073741824,
      memTotalBytes: 2147483648,
      disk: null,
      cpuTimes: null,
    }),
    subscribeUsage: () => () => {},
    notify: () => {},
    timeZone: () => 'UTC',
  });
  const baseQuery = { platform: 'mac', range: '30d', provider: 'all', account: 'all', tz: 'UTC' };
  memory('fixture-loaded');
  // Repeated minute/range heads exercise the wave 3 answer and projection retention bounds.
  for (let i = 0; i < 36; i++) {
    await analytics.get({
      ...baseQuery,
      range: 'custom',
      tz: i % 2 ? 'America/New_York' : 'UTC',
      from: new Date(now - (720 - i * 12) * 3600000).toISOString().slice(0, 10),
      to: new Date(now - 86400000).toISOString().slice(0, 10),
    });
    await sleep(1);
  }
  global.gc?.();
  const retained = memory('warm-cache-gc');
  const caches = {
    answers: analytics.answers?.size ?? 0,
    latestProjections: activity.latestProjections?.size ?? 0,
    answerBytes: analytics.answers?.bytes ?? null,
    projectionBytes: activity.latestProjections?.bytes ?? null,
  };
  await scoreboard.view();
  await status.scanReports();
  const app = express();
  app.use((req, _res, next) => {
    req.session = { authenticated: true };
    next();
  });
  app.use(
    '/api/scoreboard',
    load('web-server/routes/scoreboard-routes').createScoreboardRouter(() => scoreboard)
  );
  app.use(
    '/api/accounts',
    load('web-server/routes/account-analytics-routes').createAccountAnalyticsRouter({
      getAnalytics: (query) => analytics.get(query),
    })
  );
  app.use(
    '/api/status',
    load('web-server/routes/status-routes').createStatusRouter(() => status)
  );
  // Same trivial JSON response as the unauthenticated auth check, without a real session store.
  app.get('/api/auth/check', (_req, res) => res.status(401).json({ authenticated: false }));
  const listener = app.listen(0, '127.0.0.1');
  await new Promise((resolve) => listener.once('listening', resolve));
  const loop = monitorEventLoopDelay({ resolution: 10 });
  loop.enable();
  let spawns = 0;
  let completedCommands = 0;
  let polling = false;
  const poll = async () => {
    if (polling) return;
    polling = true;
    try {
      // Four harmless Python jobs/sec emulate the profiled helper paths. No real helper/provider runs.
      await Promise.all(
        Array.from({ length: 4 }, (_, i) => {
          spawns++;
          return runCommand('/usr/bin/python3', ['-I', '-c', `print(${i})`], {
            timeoutMs: 1000,
            maxBytes: 65536,
            env: { PATH: '/usr/bin:/bin' },
          }).then((result) => {
            if (result.code !== 0 || result.timedOut || result.stdout.trim() !== String(i))
              throw new Error('Synthetic polling command failed');
            completedCommands++;
          });
        })
      );
    } finally {
      polling = false;
    }
  };
  const pollTimer = setInterval(() => void poll(), 1000);
  const memoryTimer = setInterval(() => memory('polling'), 10000);
  process.send({ ready: true, port: listener.address().port });
  process.on('message', async (message) => {
    if (message !== 'finish') return;
    clearInterval(pollTimer);
    clearInterval(memoryTimer);
    while (polling) await sleep(10);
    loop.disable();
    const final = memory('final');
    global.gc?.();
    const finalGc = memory('final-gc');
    const retention = [];
    const release = (phase) => {
      global.gc?.();
      retention.push(memory(phase));
    };
    analytics.answers?.clear();
    release('released-answers');
    for (const state of activity.states?.values() ?? [])
      activity.projections?.delete(state.sources);
    activity.latestProjections?.clear();
    release('released-projections');
    activity.states?.clear();
    release('released-sources');
    await activity.shutdown();
    listener.close();
    let brokerRss = null;
    try {
      const managed = load('utils/managed-process');
      brokerRss = managed.processBrokerStats?.();
      if (brokerRss?.pid) {
        const status = fs.readFileSync(`/proc/${brokerRss.pid}/status`, 'utf8');
        brokerRss.rssBytes = Number(/VmRSS:\s+(\d+)/.exec(status)?.[1] ?? 0) * 1024;
      }
      await managed.stopProcessBroker();
    } catch {
      /* Baselines do not have a broker. */
    }
    process.send({
      done: true,
      retained,
      final,
      finalGc,
      timelines,
      retention,
      caches,
      spawns,
      completedCommands,
      brokerRss,
      fixture: {
        accounts: accounts.length,
        historyRecords: history.records.length,
        sessions: 10000,
        aggregateBytes: [...sourceFiles.values()].reduce(
          (n, file) => n + fs.statSync(file).size,
          0
        ),
        reports: 1702,
      },
      eventLoop: { p99Ms: loop.percentile(99) / 1e6, maxMs: loop.max / 1e6 },
    });
    process.disconnect();
  });
}

async function main() {
  if (!output) throw new Error('checkout, seconds and output path required');
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-perf-fixture-'));
  const child = fork(__filename, [checkout, duration, output], {
    execArgv: ['--expose-gc'],
    env: { ...process.env, CCS_HOME: root, CCS_DIR: root, AAC_PERF_SERVER: '1' },
    stdio: ['ignore', 'inherit', 'inherit', 'ipc'],
  });
  let done;
  const completed = new Promise((resolve, reject) => {
    child.on('message', (message) => {
      if (message.done) {
        done = message;
        resolve();
      }
    });
    child.on('error', reject);
    child.on('exit', (code) => {
      if (!done) reject(new Error(`Fixture exited ${code}`));
    });
  });
  void completed.catch(() => {});
  try {
    const ready = await new Promise((resolve, reject) => {
      child.on('message', (message) => {
        if (message.ready) resolve(message);
      });
      child.once('exit', () => reject(new Error('Fixture failed before ready')));
    });
    const paths = [
      '/api/scoreboard',
      '/api/accounts/analytics?range=30d&tz=UTC',
      '/api/status',
      '/api/auth/check',
    ];
    const latencies = Object.fromEntries(paths.map((value) => [value, []]));
    const statuses = {};
    const deadline = performance.now() + Number(duration) * 1000;
    await Promise.all(
      paths.map(async (endpoint) => {
        while (performance.now() < deadline) {
          const start = performance.now();
          const status = await new Promise((resolve, reject) => {
            http
              .get(`http://127.0.0.1:${ready.port}${endpoint}`, (res) => {
                res.resume();
                res.on('end', () => resolve(res.statusCode));
              })
              .on('error', reject);
          });
          latencies[endpoint].push(performance.now() - start);
          statuses[endpoint] = status;
          await sleep(
            endpoint.startsWith('/api/accounts/') || endpoint === '/api/scoreboard'
              ? Number(process.env.AAC_PERF_DATA_POLL_MS ?? 5000)
              : 250
          );
        }
      })
    );
    child.send('finish');
    await completed;
    const result = {
      checkout,
      seconds: Number(duration),
      cadence: {
        trivialMs: 250,
        dataMs: Number(process.env.AAC_PERF_DATA_POLL_MS ?? 5000),
        commandsPerSecond: 4,
      },
      node: process.version,
      ...done,
      endpoints: Object.fromEntries(
        paths.map((endpoint) => [
          endpoint,
          {
            status: statuses[endpoint],
            n: latencies[endpoint].length,
            p50Ms: percentile(latencies[endpoint], 0.5),
            p95Ms: percentile(latencies[endpoint], 0.95),
            maxMs: Math.max(...latencies[endpoint]),
          },
        ])
      ),
    };
    fs.writeFileSync(output, JSON.stringify(result, null, 2) + '\n');
    console.log(
      JSON.stringify({
        output,
        eventLoop: result.eventLoop,
        retainedRss: result.retained.rss,
        finalRss: result.final.rss,
        endpoints: result.endpoints,
      })
    );
  } finally {
    child.kill();
    fs.rmSync(root, { recursive: true, force: true });
  }
}
(process.env.AAC_PERF_SERVER === '1' ? server() : main()).catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
