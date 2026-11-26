/** Fixture-only M3 read snapshots, compared against real TS readers in all service modes. */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { findSecret } from '../../src/shared/secret-policy';
import type { Request } from 'express';
const root = fileURLToPath(new URL('../..', import.meta.url));
const tsOnly = process.argv.includes('--ts-only');
const regenerate = process.argv.includes('--regenerate');
const binary = process.env.AAC_RUST_SERVICE_BINARY ?? path.join(root, 'dist/native/aac-http');
const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-read-parity-'));
const ccs = path.join(home, '.ccs');
const reports = path.join(home, 'agent-reports');
fs.mkdirSync(ccs, { mode: 0o700 });
for (const key of Object.keys(process.env)) if (key.startsWith('CCS_')) delete process.env[key];
Object.assign(process.env, { CCS_HOME: home, CCS_DIR: ccs, AAC_NO_LOCAL_RUST_BUILD: '1' });
assert.equal(
  findSecret('fixture text'),
  null,
  'build the native secret filter first: bun run status:build'
);
const { RustService } = await import('../../src/web-server/rust-service');
const { requestRust } = await import('../../src/web-server/rust-service-transport');
const { AgentReportsReader } = await import('../../src/web-server/status/agent-reports-reader');
const { buildStatusView } = await import('../../src/web-server/status/status-view');
const { CatalogReader } = await import('../../src/web-server/scoreboard/catalog');
const { RunsReader } = await import('../../src/web-server/scoreboard/runs-reader');
const { buildScoreboardView } = await import('../../src/web-server/scoreboard/scoreboard-view');
const { writeRun, runDoc } = await import('../unit/web-server/scoreboard/scoreboard-test-helpers');
const { writeScoreboardFixture } = await import('../../scripts/scoreboard-fixture.mjs');
const { NOW, writeReport, threadDoc, tasksDoc, item, writeCard, cardDoc } =
  await import('../unit/web-server/status/status-test-helpers');
const services = [new RustService('off'), new RustService('on'), new RustService('shadow')];
const none = () => ({ data: null, asOf: null, error: null });
const req = {
  method: 'GET',
  sessionID: 'fixture-session',
  session: { authenticated: true },
} as unknown as Request;
try {
  if (!tsOnly) {
    assert.ok(fs.existsSync(binary), 'copy the Nas1-built binary first');
    for (const service of services) {
      await service.start(binary);
      assert.equal(service.stats.fallbacks, 0, 'no accidental all-TS proof');
    }
  }
  writeReport(reports, 'vm.fixture', 'thread.json', threadDoc('vm.fixture'));
  writeReport(
    reports,
    'vm.fixture',
    'tasks.json',
    tasksDoc('vm.fixture', [item('A', 'landed'), item('B', 'blocked'), item('C', 'dropped')])
  );
  writeCard(reports, cardDoc('vm.fixture', 'question'));
  const reader = new AgentReportsReader({ root: () => reports, now: () => NOW });
  await reader.scan();
  const status = buildStatusView({
    now: NOW,
    timeZone: 'UTC',
    reports: reader.snapshot(),
    registry: none(),
    machines: { vm: none(), nas1: none(), mac: none(), windows: none(), alexis: none(), odroid: none(), '1070ti': none() },
    browsers: none(),
    simlock: none(),
    usage: null,
    answers: [],
  });
  assert.equal(status.headline.counts.questions, 1);
  assert.equal(status.headline.counts.blockedItems, 1);
  const scoreboardRoot = path.join(home, 'scoreboard');
  writeScoreboardFixture(scoreboardRoot, { now: NOW });
  fs.rmSync(path.join(scoreboardRoot, 'v1/writers'), { recursive: true });
  writeRun(scoreboardRoot, runDoc('vm.fixture', 'read'));
  for (const [name, key, select] of [
    ['models.json', 'models', (entry: any) => entry.model === 'anthropic/claude-opus-5-5'],
    ['task-types.json', 'taskTypes', (entry: any) => entry.id === 'build'],
    ['uses.json', 'uses', () => true],
    ['benchmarks.json', 'benchmarks', () => false],
  ] as const) {
    const file = path.join(scoreboardRoot, 'v1/catalog', name);
    const doc = JSON.parse(fs.readFileSync(file, 'utf8'));
    doc[key] = doc[key].filter(select);
    fs.writeFileSync(file, JSON.stringify(doc));
  }
  const options = { root: () => scoreboardRoot, now: () => NOW };
  const catalog = new CatalogReader(options);
  const runs = new RunsReader(options);
  await Promise.all([catalog.scan(), runs.scan()]);
  const score = buildScoreboardView({
    now: NOW,
    timeZone: 'UTC',
    root: scoreboardRoot,
    catalog: catalog.snapshot(),
    runs: runs.snapshot(),
  });
  assert.ok(score.uses.length > 0, 'real runs/catalog fixture, not a response stub');
  const rows = [
    { name: 'status-populated', route: '/api/status', body: status, owner: 'rust' },
    { name: 'scoreboard-populated', route: '/api/scoreboard', body: score, owner: 'rust' },
    {
      name: 'store-disabled',
      route: '/api/store/status',
      body: { state: 'unavailable', reason: 'disabled' },
      owner: 'rust',
    },
    {
      name: 'store-unsupported-stub',
      route: '/api/store/status',
      body: { state: 'ready', fixture: true },
      owner: 'ts-fallback',
    },
  ];
  const { openCardFile } = await import('../../src/web-server/status/status-files');
  const mediaDir = path.join(reports, 'v1/writers/vm.fixture/media');
  fs.mkdirSync(mediaDir, { recursive: true });
  fs.writeFileSync(
    path.join(mediaDir, 'image.png'),
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3, 4])
  );
  const media = await openCardFile(reports, 'vm.fixture', 'media/image.png');
  assert.ok(media.ok);
  if (!media.ok) throw new Error('fixture media unavailable');
  const buffer = Buffer.alloc(media.size);
  await media.handle.read(buffer, 0, buffer.length, 0);
  await media.handle.close();
  rows.push({
    name: 'status-media',
    route: '/api/status/files/{*path}',
    body: { size: buffer.length, bytes: buffer.toString('base64') },
    owner: 'rust',
    input: {
      root: reports,
      writer: 'vm.fixture',
      path: 'media/image.png',
      start: 0,
      length: buffer.length,
      head: false,
    },
  } as any);
  const machine = path.join(home, 'machine');
  fs.mkdirSync(path.join(machine, 'share'), { recursive: true });
  const concept = Buffer.from('<h1>Fixture deliverable</h1>');
  fs.writeFileSync(path.join(machine, 'share/concept.html'), concept);
  rows.push({
    name: 'deliverable',
    route: '/api/status/deliverable',
    body: { size: concept.length, bytes: concept.toString('base64') },
    owner: 'rust',
    input: {
      root: machine,
      writer: null,
      path: 'share/concept.html',
      start: 0,
      length: concept.length,
      head: false,
    },
  } as any);
  rows.push({
    name: 'deliverable-range',
    route: '/api/status/deliverable',
    body: { size: concept.length, bytes: concept.subarray(1, 7).toString('base64') },
    owner: 'rust',
    input: {
      root: machine,
      writer: null,
      path: 'share/concept.html',
      start: 1,
      length: 6,
      head: false,
    },
  } as any);
  // Windows serializes `\` as `\\` inside JSON text, so the escaped form is the one that appears there.
  const escapedHome = JSON.stringify(home).slice(1, -1);
  // The snapshot pins fixture paths, not the host's separator: compare them in one form.
  const portable = (value: unknown): unknown => {
    if (typeof value === 'string') return value.split('\\').join('/');
    if (Array.isArray(value)) return value.map(portable);
    if (value && typeof value === 'object')
      return Object.fromEntries(Object.entries(value).map(([key, entry]) => [key, portable(entry)]));
    return value;
  };
  const normalize = (value: unknown) => {
    const parsed = JSON.parse(
      JSON.stringify(value).split(escapedHome).join('<fixture>').split(home).join('<fixture>')
    );
    return path.sep === '\\' ? portable(parsed) : parsed;
  };
  const golden = path.join(root, 'tests/spec/contract/m3-read-snapshots.json');
  const bytes = JSON.stringify(normalize(rows), null, 2) + '\n';
  if (regenerate) fs.writeFileSync(golden, bytes);
  else assert.equal(fs.readFileSync(golden, 'utf8'), bytes, 'real-reader golden bytes changed');
  if (process.env.AAC_STATUS_MODULE) {
    const { openStatusHandle } = await import('../../src/status-native/pinned');
    const { composeByModel, composeGroups } =
      await import('../../src/web-server/scoreboard/by-model');
    const handle = openStatusHandle({ file: process.env.AAC_STATUS_MODULE });
    assert.ok(!('unavailable' in handle), 'the optional native fixture must actually load');
    if ('unavailable' in handle) throw new Error(handle.unavailable);
    const byModel = await composeByModel(score, { handle });
    const groups = await composeGroups(score, { handle });
    assert.ok(byModel);
    assert.ok(groups);
    rows.push({
      name: 'scoreboard-native-fields',
      route: '/api/scoreboard',
      body: { ...score, byModel, groups },
      owner: 'rust',
    } as any);
  }
  for (const row of rows) {
    for (const service of tsOnly ? services.slice(0, 1) : services) {
      const before = { ...service.stats };
      const answer = await service.read(
        req,
        row.route,
        row.body,
        (row as any).input ?? { snapshot: row.body }
      );
      assert.equal(
        JSON.stringify(answer),
        JSON.stringify(row.body),
        `${row.name}/${service.mode}: raw serialized bytes`
      );
      if (service.mode === 'on') {
        assert.equal(service.stats.served - before.served, row.owner === 'rust' ? 1 : 0, row.name);
        assert.equal(
          service.stats.fallbacks - before.fallbacks,
          row.owner === 'rust' ? 0 : 1,
          row.name
        );
      }
      if (service.mode === 'shadow') {
        for (let i = 0; i < 200 && service.stats.shadowed === before.shadowed; i++)
          await new Promise((resolve) => setTimeout(resolve, 5));
        assert.equal(service.stats.shadowed - before.shadowed, 1, row.name);
        assert.equal(service.stats.differences - before.differences, 0, row.name);
      }
    }
  }
  if (!tsOnly) {
    const service = services[1];
    const before = { ...service.stats };
    const wrong = structuredClone(status);
    wrong.headline.counts.questions = 999;
    assert.deepEqual(await service.read(req, '/api/status', wrong, { snapshot: status }), wrong);
    assert.equal(
      service.stats.differences - before.differences,
      1,
      'candidate mismatch must refuse'
    );
    assert.equal(service.stats.fallbacks - before.fallbacks, 1);
    const excessive = { snapshot: { padding: 'x'.repeat(4 * 1024 * 1024) } };
    const boot = JSON.parse(fs.readFileSync(path.join(ccs, 'run/aac-http.json'), 'utf8'));
    await assert.rejects(
      requestRust(
        boot,
        '/api/store/status',
        { kind: 'session', id: 'fixture-session', role: 'admin' },
        new Set(),
        excessive
      ),
      /Rust service request cap/
    );
    const count = service.stats.fallbacks;
    assert.deepEqual(
      await service.read(
        req,
        '/api/store/status',
        { state: 'unavailable', reason: 'disabled' },
        excessive
      ),
      { state: 'unavailable', reason: 'disabled' }
    );
    assert.equal(service.stats.fallbacks, count + 1, 'input cap is an explicit fallback');
  }
  console.log(
    `M3 real-reader snapshots: ${rows.length} cases, ${tsOnly ? 'TS only' : 'off/on/shadow'} passed`
  );
} finally {
  for (const service of services.reverse()) service.stop();
  fs.rmSync(home, { recursive: true, force: true });
}
