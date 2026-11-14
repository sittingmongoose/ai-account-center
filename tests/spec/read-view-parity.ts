/** Real raw-reader view composition parity; all data and locale facts are fixture-owned. */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
const root = fileURLToPath(new URL('../..', import.meta.url));
const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-raw-views-'));
const ccs = path.join(home, '.ccs');
fs.mkdirSync(ccs, { mode: 0o700 });
for (const key of Object.keys(process.env)) if (key.startsWith('CCS_')) delete process.env[key];
Object.assign(process.env, { CCS_HOME: home, CCS_DIR: ccs, AAC_NO_LOCAL_RUST_BUILD: '1' });
const { RustService } = await import('../../src/web-server/rust-service');
const { requestRust } = await import('../../src/web-server/rust-service-transport');
const { rustReadInput, rustScoreboardInput, rustStatusInput } =
  await import('../../src/web-server/rust-read-input');
const { writeScoreboardFixture } = await import('../../scripts/scoreboard-fixture.mjs');
const { CatalogReader } = await import('../../src/web-server/scoreboard/catalog');
const { RunsReader } = await import('../../src/web-server/scoreboard/runs-reader');
const { buildScoreboardView } = await import('../../src/web-server/scoreboard/scoreboard-view');
const tsOnly = process.argv.includes('--ts-only');
const captured: unknown[] = [];
const service = new RustService('on');
const { quotaDocument } = await import('./read-view-fixtures');
const { QuotaReportReader } = await import('../../src/web-server/status/quota-report');
const { AgentReportsReader } = await import('../../src/web-server/status/agent-reports-reader');
const { buildStatusView } = await import('../../src/web-server/status/status-view');
const { writeReport, threadDoc, tasksDoc, item, writeCard, cardDoc } =
  await import('../unit/web-server/status/status-test-helpers');
try {
  const collation = rustReadInput(['é', 'e\u0301'], 'UTC') as any;
  assert.equal(collation.presentation.order['é'], collation.presentation.order['e\u0301']);
  const dir = path.join(home, 'reports');
  const now = Date.parse('2026-11-06T16:30:00Z');
  writeScoreboardFixture(dir, { now });
  const opts = { root: () => dir, now: () => now };
  const catalog = new CatalogReader(opts);
  const runs = new RunsReader(opts);
  await Promise.all([catalog.scan(), runs.scan()]);
  if (!tsOnly)
    await service.start(
      process.env.AAC_RUST_SERVICE_BINARY ?? path.join(root, 'dist/native/aac-http')
    );
  assert.equal(service.stats.fallbacks, 0);
  const boot = tsOnly
    ? null
    : JSON.parse(fs.readFileSync(path.join(ccs, 'run/aac-http.json'), 'utf8'));
  const active = new Set<import('node:http').ClientRequest>();
  const scoreBase: any = {
    now,
    timeZone: 'UTC',
    root: dir,
    catalog: catalog.snapshot(),
    runs: runs.snapshot(),
  };
  const filtered = structuredClone(scoreBase);
  filtered.runs.runs = filtered.runs.runs.filter(
    (row: any) => row.doc.task.type === 'worknode-build'
  );
  filtered.joinReviews = scoreBase.runs.runs
    .filter((row: any) => row.doc.task.type === 'review')
    .map((row: any) => row.doc);
  const missing = structuredClone(scoreBase);
  missing.catalog.scannedAt = null;
  missing.catalog.state = 'missing';
  const stale = structuredClone(scoreBase);
  stale.now += 1000000;
  const empty = structuredClone(scoreBase);
  empty.runs.runs = [];
  empty.runs.problems = [];
  const unknown = structuredClone(scoreBase);
  for (const [n, row] of unknown.runs.runs.entries())
    row.doc.outcome = ['partial', '\uFEFFdone', '\u0085done', 'done\uFEFF', 'timed_out'][n % 5];
  const scoreCases = [scoreBase, filtered, missing, stale, empty, unknown];
  for (const input of scoreCases)
    for (const zone of ['UTC', 'America/New_York', 'invalid-fixture-zone']) {
      input.timeZone = zone;
      const expected = buildScoreboardView(input);
      captured.push({
        route: '/api/scoreboard',
        case: `${scoreCases.indexOf(input)}:${zone}`,
        body: expected,
      });
      if (tsOnly) continue;
      const reply = await requestRust(
        boot,
        '/api/scoreboard',
        { kind: 'session', id: 'fixture-admin', role: 'admin' },
        active,
        rustScoreboardInput(input)
      );
      assert.equal(reply.status, 200, 'raw Scoreboard must not silently fall back');
      assert.deepEqual(
        reply.body,
        JSON.parse(JSON.stringify(expected)),
        `Scoreboard case ${scoreCases.indexOf(input)} in ${zone}`
      );
      assert.equal(
        JSON.stringify(reply.body),
        JSON.stringify(expected),
        `Scoreboard order case ${scoreCases.indexOf(input)} in ${zone}`
      );
    }
  if (!tsOnly && process.env.AAC_STATUS_MODULE) {
    const { openStatusHandle } = await import('../../src/status-native/pinned');
    const { composeByModel, composeGroups } =
      await import('../../src/web-server/scoreboard/by-model');
    const handle = openStatusHandle({ file: process.env.AAC_STATUS_MODULE });
    assert.ok(!('unavailable' in handle));
    if ('unavailable' in handle) throw new Error(handle.unavailable);
    const finish = {
      filters: {
        from: null,
        to: null,
        source: 'all',
        taskType: null,
        available: false,
        taskTypes: scoreBase.catalog.taskTypes.map((t: any) => ({ id: t.id, label: t.label })),
      },
      identity: null,
      history: { store: 'off', runs: null, importing: false },
      native: true,
    };
    const base = {
      ...buildScoreboardView(scoreBase),
      filters: finish.filters,
      identity: null,
      history: finish.history,
    };
    const byModel = await composeByModel(base, { handle });
    const groups = byModel ? await composeGroups(base, { handle }) : null;
    const expected = {
      ...base,
      ...(byModel ? { byModel } : {}),
      ...(groups ? { groups } : {}),
      cached: true,
      cacheAgeSeconds: 0,
      refreshing: false,
    };
    const reply = await requestRust(
      boot,
      '/api/scoreboard',
      { kind: 'session', id: 'fixture-admin', role: 'admin' },
      active,
      rustScoreboardInput(scoreBase, finish)
    );
    assert.equal(reply.status, 200);
    assert.deepEqual(reply.body, JSON.parse(JSON.stringify(expected)));
    assert.equal(JSON.stringify(reply.body), JSON.stringify(expected));
  }
  const statusDir = path.join(home, 'status');
  writeReport(statusDir, 'vm.fixture', 'thread.json', threadDoc('vm.fixture'));
  writeReport(
    statusDir,
    'vm.fixture',
    'tasks.json',
    tasksDoc('vm.fixture', [item('A', 'landed'), item('B', 'blocked'), item('C', 'dropped')])
  );
  writeCard(statusDir, cardDoc('vm.fixture', 'question'));
  const reader = new AgentReportsReader({ root: () => statusDir, now: () => now });
  await reader.scan();
  const none = () => ({ data: null, asOf: null, error: null });
  const sample = {
    cores: 8,
    load1: 1.234,
    busyPct: 12,
    memAvailableBytes: 16 * 1024 ** 3,
    memTotalBytes: 32 * 1024 ** 3,
    disk: { mount: '/', freeBytes: 200 * 1024 ** 3, totalBytes: 500 * 1024 ** 3 },
    cpuTimes: null,
  };
  const base: any = {
    now,
    timeZone: 'UTC',
    reports: reader.snapshot(),
    registry: none(),
    machines: Object.fromEntries(
      ['vm', 'mac', 'windows', 'nas1', 'alexis'].map((id) => [
        id,
        { data: structuredClone(sample), asOf: now - 30000, error: null },
      ])
    ),
    browsers: none(),
    simlock: none(),
    usage: null,
    answers: [],
  };
  fs.mkdirSync(path.join(statusDir, 'v1/quota'), { recursive: true });
  fs.writeFileSync(path.join(statusDir, 'v1/quota/report.json'), JSON.stringify(quotaDocument()));
  const quota = new QuotaReportReader({ root: () => statusDir, now: () => now });
  await quota.scan();
  const period = 60000;
  const complex = structuredClone(base);
  complex.quota = quota.snapshot();
  complex.registry = {
    data: [
      {
        name: 'mail-worker',
        kind: 'codex',
        machine: 'vm',
        about: 'Fixture task',
        lastSeen: new Date(now - period).toISOString(),
        readAt: now - 30000,
        problem: null,
      },
    ],
    asOf: now,
    error: null,
  };
  complex.machines.vm.data.memAvailableBytes = 2 * 1024 ** 3;
  complex.machines.mac = {
    data: structuredClone(sample),
    asOf: now - 30000,
    error: { reason: 'Fixture host unavailable.', since: now - 50000 },
  };
  complex.browsers = {
    data: [
      {
        machine: 'vm',
        browsers: 1,
        recordings: 0,
        gpuPct: 12,
        sessions: [],
        readAt: now - 30000,
        problem: null,
      },
    ],
    asOf: now,
    error: null,
  };
  complex.usage = {
    sampledAt: now - 30000,
    staleAfterSeconds: 120,
    accounts: [
      {
        id: 'fixture-account',
        provider: 'codex',
        label: 'Fixture account',
        host: 'vm',
        active: true,
        hidden: false,
        status: 'ok',
        sampledAt: new Date(now - 30000).toISOString(),
        windows: [
          {
            label: 'Weekly',
            kind: 'rate_limit',
            usedPercent: 100,
            remaining: null,
            limit: null,
            unit: 'percent',
            resetAt: new Date(now + 3600000).toISOString(),
            resetPassed: false,
            cached: false,
            burn: null,
            sampledAt: new Date(now - 30000).toISOString(),
          },
        ],
      },
    ],
  };
  complex.pinned = {
    asOfMs: now,
    head: 'Fixture pins',
    counts: { pinned: 2, looping: 1, waitingOnYou: 1 },
    machineProblems: [],
    updates: [
      {
        id: 'loop',
        title: 'Looping fixture',
        looping: { answer: 'yes', text: 'Fixture loop.', task: 'A', rounds: 3 },
        blocked: { waitingOnYou: false, text: 'None.' },
      },
      {
        id: 'wait',
        title: 'Waiting fixture',
        looping: { answer: 'no', text: 'No.', task: null, rounds: 0 },
        blocked: { waitingOnYou: true, text: 'Pick the fixture.' },
      },
    ],
  };
  const row = base.reports.cards[0];
  const done = {
    state: 'done',
    at: new Date(now - 1000).toISOString(),
    detail: 'Fixture complete',
    attempts: 1,
  };
  const history = structuredClone(complex);
  history.answers = [
    {
      id: 'fixture-answer',
      sendId: 'fixture-send',
      answer: {
        cardId: row.doc.id,
        cardSeq: row.doc.seq,
        cardSha256: row.sha256,
        kind: 'answer',
        choice: ['A'],
        note: 'Fixture note',
        via: 'deck',
        answeredAt: new Date(now - 1000).toISOString(),
        device: 'Fixture',
      },
      card: row.doc,
      share: done,
      doorbell: done,
    },
  ];
  history.sends = [
    {
      sendId: 'fixture-send',
      summary: {
        state: 'failed',
        at: new Date(now - 1000).toISOString(),
        detail: 'Fixture share unavailable',
        attempts: 2,
      },
    },
  ];
  const problems = structuredClone(complex);
  problems.reports.problems = [
    { file: 'writers/vm.fixture/thread.json', reason: 'Fixture malformed.', since: now - 60000 },
    { file: 'limits/writers', reason: 'Fixture scan limit.', since: now - 60000 },
  ];
  problems.registry.problems = [
    { item: 'vm.fixture', reason: 'Fixture file unavailable.', since: now - 60000 },
  ];
  problems.machines.vm.problems = [
    { item: 'disk', reason: 'Fixture disk unavailable.', since: now - 60000 },
  ];
  problems.quota.problem = { reason: 'Fixture quota malformed.', since: now - 60000 };
  const sourceStates = ['missing', 'error', 'ok'].map((state) => {
    const value = structuredClone(base);
    value.reports.state = state;
    if (state === 'ok') value.now += 2000000;
    return value;
  });
  const cases = [base, complex, history, problems, ...sourceStates];
  for (const input of cases)
    for (const zone of ['UTC', 'America/New_York', 'invalid-fixture-zone']) {
      input.timeZone = zone;
      const expected = buildStatusView(input);
      captured.push({
        route: '/api/status',
        case: `${cases.indexOf(input)}:${zone}`,
        body: expected,
      });
      if (tsOnly) continue;
      const reply = await requestRust(
        boot,
        '/api/status',
        { kind: 'session', id: 'fixture-admin', role: 'admin' },
        active,
        rustStatusInput(input)
      );
      assert.equal(reply.status, 200, 'raw Status must not silently fall back');
      assert.deepEqual(
        reply.body,
        JSON.parse(JSON.stringify(expected)),
        `raw Status parity case ${cases.indexOf(input)} in ${zone}`
      );
      assert.equal(
        JSON.stringify(reply.body),
        JSON.stringify(expected),
        `raw Status order case ${cases.indexOf(input)} in ${zone}`
      );
    }
  const file = path.join(root, 'tests/spec/contract/m3-read-views.json');
  const golden =
    JSON.stringify(JSON.parse(JSON.stringify(captured).split(home).join('<fixture>')), null, 2) +
    '\n';
  if (process.argv.includes('--regenerate')) fs.writeFileSync(file, golden);
  else assert.equal(fs.readFileSync(file, 'utf8'), golden, 'raw-reader view golden bytes changed');
  console.log('18 Scoreboard and 21 Status raw-reader states: parity and bytes passed');
} finally {
  service.stop();
  fs.rmSync(home, { recursive: true, force: true });
}
