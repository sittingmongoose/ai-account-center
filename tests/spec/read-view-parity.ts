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
  // sb-R3: reviews whose own runs are unscored (a model the catalog lacks) count nowhere —
  // in view, or joining from outside a filtered view — so TS and Rust must agree they move
  // no blind-review measure.
  const unscoredReviews = structuredClone(scoreBase);
  {
    const reviewed = new Set(
      unscoredReviews.runs.runs
        .filter((row: any) => row.doc.review)
        .map((row: any) => row.doc.review.of)
    );
    const target = unscoredReviews.runs.runs.find(
      (row: any) =>
        row.doc.task.type === 'build' && row.doc.outcome === 'done' && !reviewed.has(row.doc.id)
    );
    assert.ok(target, 'a scored, unreviewed build run to review');
    const at = (minutes: number) =>
      new Date(Date.parse(target.doc.endedAt) + minutes * 60_000).toISOString();
    const UNSCORED_MODEL = { provider: 'mystery', id: 'mystery-9', harness: null, effort: null };
    const scored = unscoredReviews.runs.runs.find((row: any) => row.doc.task.type === 'review');
    assert.ok(scored, 'a review run to copy field shapes from');
    const reviewRow = (local: string, minutes: number, fields: Record<string, unknown>) => ({
      file: `writers/mac.reviews/runs/2026-11-06/mac.reviews--${local}.json`,
      firstSeenAt: Date.parse(at(minutes)),
      sha256: 'r2'.repeat(32),
      doc: {
        ...structuredClone(scored.doc),
        id: `mac.reviews--${local}`,
        endedAt: at(minutes),
        writtenAt: at(minutes),
        task: { id: local, type: 'review', title: `Review ${local}`, size: null },
        review: {
          of: target.doc.id,
          verdict: 'pass',
          findings: { high: 0, medium: 0, low: 0 },
          overturned: false,
          disputed: false,
          by: null,
        },
        ...fields,
      },
    });
    unscoredReviews.runs.runs.push(
      // An unscored reviewer's later pass, over a scored review's earlier fail with an
      // overturn: before sb-R3 the pass rescued the item on one side of the parity only.
      reviewRow('sb3-unscored-pass', 90, { model: UNSCORED_MODEL }),
      reviewRow('sb3-scored-fail', 60, {
        review: {
          of: target.doc.id,
          verdict: 'fail',
          findings: { high: 0, medium: 0, low: 0 },
          overturned: true,
          disputed: false,
          by: null,
        },
      })
    );
    unscoredReviews.joinReviews = [
      reviewRow('sb3-join-unscored', 120, { model: UNSCORED_MODEL }).doc,
    ];
  }
  const scoreCases = [scoreBase, filtered, missing, stale, empty, unknown, unscoredReviews];
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
      ['vm', 'mac', 'windows', 'nas1', 'alexis', 'odroid', '1070ti'].map((id) => [
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
  if (!tsOnly) {
    const { checkRoundingParity } = await import('./read-rounding-parity');
    await checkRoundingParity(service, complex);
  }
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
  const cachedAtLimit = structuredClone(complex);
  cachedAtLimit.usage.accounts[0].windows[0].cached = true;
  cachedAtLimit.usage.accounts[0].windows[0].sampledAt = null;
  {
    const expected = buildStatusView({ ...cachedAtLimit, timeZone: 'UTC' });
    const row = expected.usage.accounts.find((a: { id: string }) => a.id === 'fixture-account');
    assert.equal(row?.stale, true, 'cached 100% with no sampledAt stays visible as stale');
    assert.equal(row?.atLimit, false, 'cached 100% with no sampledAt is not an at-limit claim');
  }
  const cases = [base, complex, history, problems, ...sourceStates, cachedAtLimit];
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
  // Big review (Fable M1): a board time is fs.Stats.mtimeMs, which carries a fraction on ext4, ZFS, NFS and SMB.
  // Rust must age it as TS does (or decline), never read it as no time. One real file's stat, Fable's synthetic
  // fraction and an integer control; not part of the golden (a real stat changes every run).
  const boardFile = path.join(statusDir, 'fixture-board.md');
  fs.writeFileSync(boardFile, 'board');
  const stamp = fs.statSync(boardFile).mtimeMs;
  let fractional = 0;
  for (const [mtime, at] of [
    [now - 3600000 + 0.4567, now],
    [stamp, Math.floor(stamp) + 5000],
    [stamp, Math.floor(stamp) + 499],
    [now - 3600000, now],
  ] as const) {
    if (!Number.isInteger(mtime)) fractional++;
    const input = structuredClone(base);
    input.now = at;
    input.timeZone = 'UTC';
    input.reports.tasks[0].doc.board = 'share/fixture/board.md';
    input.reports.boards = new Map([['share/fixture/board.md', mtime]]);
    const expected = buildStatusView(input);
    const age = expected.projects.flatMap((p: any) => p.streams).find((stream: any) => stream.board)
      ?.board.ageSeconds;
    assert.equal(typeof age, 'number', 'the TS view ages the board');
    if (tsOnly) continue;
    const reply = await requestRust(
      boot,
      '/api/status',
      { kind: 'session', id: 'fixture-admin', role: 'admin' },
      active,
      rustStatusInput(input)
    );
    assert.equal(reply.status, 200, `board mtime ${mtime}: Rust declined`);
    assert.equal(JSON.stringify(reply.body), JSON.stringify(expected), `board mtime ${mtime}`);
  }
  assert.ok(fractional >= 1, 'at least one board time carries a fraction');
  // Big review (Fable L1): TS reads a name from a plain object (a quota meter label, a catalog metric) as the inherited
  // Object.prototype member when the name is one, which no JSON lookup reproduces. Rust must decline those inputs.
  const rename = (value: any, edit: (row: any) => boolean): number => {
    if (value instanceof Map) return [...value.values()].reduce((n, v) => n + rename(v, edit), 0);
    if (Array.isArray(value)) return value.reduce((n, v) => n + rename(v, edit), 0);
    if (!value || typeof value !== 'object') return 0;
    return (
      (edit(value) ? 1 : 0) + Object.values(value).reduce((n: number, v) => n + rename(v, edit), 0)
    );
  };
  const protoStatus = structuredClone(complex);
  protoStatus.timeZone = 'UTC';
  const meters = rename(
    protoStatus.quota,
    (row) => row.meter === 'seven_day' && !!(row.meter = 'constructor')
  );
  const protoScore = structuredClone(scoreBase);
  const metrics = rename(protoScore.catalog, (row) =>
    typeof row.metric?.of === 'string' ? !!(row.metric.of = 'constructor') : false
  );
  assert.ok(meters > 0 && metrics > 0, 'the prototype-name inputs reach a meter and a metric');
  for (const [route, input, build, envelope] of [
    ['/api/status', protoStatus, buildStatusView, rustStatusInput],
    ['/api/scoreboard', protoScore, buildScoreboardView, rustScoreboardInput],
  ] as const) {
    const expected = JSON.stringify((build as (i: any) => unknown)(input));
    if (tsOnly) continue;
    const reply = await requestRust(
      boot,
      route,
      { kind: 'session', id: 'fixture-admin', role: 'admin' },
      active,
      (envelope as (i: any) => unknown)(input)
    );
    assert.ok(
      reply.status === 503 || (reply.status === 200 && JSON.stringify(reply.body) === expected),
      `${route}: a prototype-member name must decline or match TS (got ${reply.status})`
    );
  }
  const file = path.join(root, 'tests/spec/contract/m3-read-views.json');
  // Windows serializes `\` as `\\` inside JSON text, so the escaped form is the one that appears there.
  const escapedHome = JSON.stringify(home).slice(1, -1);
  // The views pin fixture paths, not the host's separator: compare them in one form.
  const portable = (value: unknown): unknown => {
    if (typeof value === 'string') return value.split('\\').join('/');
    if (Array.isArray(value)) return value.map(portable);
    if (value && typeof value === 'object')
      return Object.fromEntries(
        Object.entries(value).map(([key, entry]) => [key, portable(entry)])
      );
    return value;
  };
  const normalized = JSON.parse(
    JSON.stringify(captured).split(escapedHome).join('<fixture>').split(home).join('<fixture>')
  );
  const golden =
    JSON.stringify(path.sep === '\\' ? portable(normalized) : normalized, null, 2) + '\n';
  if (process.argv.includes('--regenerate')) fs.writeFileSync(file, golden);
  else assert.equal(fs.readFileSync(file, 'utf8'), golden, 'raw-reader view golden bytes changed');
  console.log(
    '18 Scoreboard and 24 Status raw-reader states, 4 board ages and 2 prototype names: parity and bytes passed'
  );
} finally {
  service.stop();
  fs.rmSync(home, { recursive: true, force: true });
}
