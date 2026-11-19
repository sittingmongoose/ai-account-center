#!/usr/bin/env node
/**
 * R4 exit gate, end to end: the owned Status and Scoreboard paths against today's handlers,
 * over identical fixture shares in two homes (two worker processes, one per mode, each with
 * its own store). At every step the owned answer must equal today's answer up to the scan
 * clocks (the two readers scan at different instants; the reader corpus proves the scan logic
 * itself), and the owned path must have served (x-aac-view). Then the ingest audit, the
 * readers-check file, and the kill drill: with the service stopped, the TypeScript share
 * readers return and the page still answers.
 *
 *   node tests/spec/owned-read-parity.mjs [--dist <dist>] [--binary <aac-http>] [--ts-only]
 * Needs the Nas1-built binary and native modules in dist/native. Fixture data only.
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import { fork } from 'node:child_process';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, '../..');
const args = process.argv.slice(2);
const option = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : fallback;
};
const dist = path.resolve(option('dist', path.join(repo, 'dist')));
const binary = path.resolve(option('binary', path.join(dist, 'native/aac-http')));

if (args.includes('--ts-only')) {
  // Off mode: the owned switches stay off without config, and the workers are not started.
  const { rustOwnedStatusOn, rustOwnedScoreboardOn } = await import(
    path.join(dist, 'web-server/rust-service-groups.js')
  );
  assert.equal(rustOwnedStatusOn({}), false);
  assert.equal(rustOwnedScoreboardOn({}), false);
  console.log('owned-read-parity: the owned groups are off by default');
  process.exit(0);
}
assert.ok(fs.existsSync(binary), `needs the Nas1-built aac-http at ${binary}`);

const NOW = Date.parse('2026-11-18T16:30:00.000Z');
const iso = (ms) => new Date(ms).toISOString();
const scratch = fs.mkdtempSync(path.join(process.env.TMPDIR ?? os.tmpdir(), 'aac-r4-parity-'));
const homes = { today: path.join(scratch, 'today'), owned: path.join(scratch, 'owned') };

// ---- identical fixture shares -------------------------------------------------------------
const catalogDir = path.join(repo, 'docs/agent-reports/catalog');
const models = JSON.parse(fs.readFileSync(path.join(catalogDir, 'models.json'), 'utf8')).models;
const taskTypes = JSON.parse(
  fs.readFileSync(path.join(catalogDir, 'task-types.json'), 'utf8')
).taskTypes;
const modelId = models[0].model;
const taskType = taskTypes[0].id;

function writeFixtures(home) {
  const share = path.join(home, 'share');
  const writers = path.join(share, 'v1', 'writers');
  const put = (rel, value) => {
    const file = path.join(writers, rel);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
  };
  const thread = (writer, fields = {}) => ({
    schema: 'agent-reports/thread@1',
    writer,
    seq: 1,
    writtenAt: iso(NOW - 60000),
    ttlSeconds: 1800,
    agent: { name: writer.slice(writer.indexOf('.') + 1), model: modelId, effort: 'high' },
    project: 'proj',
    stream: 'S',
    state: 'working',
    doing: 'Building',
    blockedOn: null,
    progress: null,
    links: [],
    ...fields,
  });
  put('vm.fixture/thread.json', thread('vm.fixture'));
  put('vm.fixture/tasks.json', {
    schema: 'agent-reports/tasks@1',
    writer: 'vm.fixture',
    seq: 1,
    writtenAt: iso(NOW - 60000),
    project: 'proj',
    stream: 'S',
    items: [{ id: 'A', title: 'Item A', state: 'landed', updatedAt: iso(NOW - 120000) }],
  });
  put('vm.fixture/status.json', {
    schema: 'agent-reports/status@1',
    writer: 'vm.fixture',
    seq: 1,
    writtenAt: iso(NOW - 30000),
    t3Thread: 'thread-1',
    summary: 'Half done.',
    percent: 50,
    done: [{ id: 'd1', title: 'Done one' }],
    left: [],
    blockers: [],
  });
  put('vm.fixture/cards/vm.fixture--q1.json', {
    schema: 'agent-reports/card@1',
    id: 'vm.fixture--q1',
    writer: 'vm.fixture',
    seq: 1,
    writtenAt: iso(NOW - 60000),
    askedBy: 'fixture',
    replyTo: { pmMail: 'fixture', t3Thread: null },
    state: 'open',
    title: 'Question one',
    now: 'Plain words.',
    options: [
      { key: 'A', label: 'First' },
      { key: 'B', label: 'Second' },
    ],
    multiple: false,
    urgency: 'today',
    askedAt: iso(NOW - 600000),
    kind: 'question',
  });
  for (const [index, day] of ['2026-11-17', '2026-11-18'].entries()) {
    for (const n of [1, 2]) {
      put(`vm.fixture/runs/${day}/run-${index}-${n}.json`, {
        schema: 'agent-reports/run@1',
        id: `vm.fixture--run-${index}-${n}`,
        writer: 'vm.fixture',
        seq: 1,
        writtenAt: iso(NOW - 3600000),
        source: 'observed',
        task: { id: `t-${index}-${n}`, type: taskType, title: 'A review' },
        model: { id: modelId, provider: null, harness: 'codex', effort: 'high' },
        endedAt: iso(NOW - 3600000 + n * 1000),
        outcome: 'done',
        wallSeconds: 60 + n,
      });
    }
  }
  const quota = path.join(share, 'v1', 'quota');
  fs.mkdirSync(quota, { recursive: true });
  fs.writeFileSync(
    path.join(quota, 'report.json'),
    JSON.stringify({
      schema: 'quota-report@1',
      writer: 'vm.fixture',
      seq: 1,
      writtenAt: iso(NOW - 120000),
      estimate: true,
      streamEstimates: {
        day: {
          period: 'day',
          start: iso(NOW - 86400000),
          end: iso(NOW),
          coverage: 0.5,
          rows: [
            {
              stream: 'S',
              account: 'codex:account-a',
              meter: 'credits_balance',
              unit: 'credits',
              value: 1.5,
              estimate: true,
            },
          ],
        },
      },
      budgetRows: [],
      unattributedRows: [],
      machinesNotScanned: [],
    })
  );
  fs.mkdirSync(path.join(share, 'v1', 'catalog'), { recursive: true });
  for (const name of fs.readdirSync(catalogDir))
    if (name.endsWith('.json'))
      fs.copyFileSync(path.join(catalogDir, name), path.join(share, 'v1', 'catalog', name));
  const registry = path.join(home, 'registry');
  fs.mkdirSync(registry, { recursive: true });
  fs.writeFileSync(
    path.join(registry, 'lead.json'),
    JSON.stringify({
      name: 'lead',
      kind: 'impl',
      machine: 'ubuntuserver',
      cwd: '/home/x',
      about: 'Working',
      last_seen: iso(NOW - 30000),
    })
  );
  return share;
}
const shares = { today: writeFixtures(homes.today), owned: writeFixtures(homes.owned) };

// ---- the workers --------------------------------------------------------------------------
function startWorker(mode) {
  const child = fork(path.join(here, 'owned-read-worker.cjs'), [dist, binary, mode, homes[mode]], {
    stdio: ['ignore', 'inherit', 'inherit', 'ipc'],
  });
  const pending = new Map();
  let nextId = 1;
  child.on('message', (message) => {
    if (message.ready !== undefined) {
      readyResolve(message);
      return;
    }
    const held = pending.get(message.id);
    if (held) {
      pending.delete(message.id);
      held(message);
    }
  });
  let readyResolve;
  const ready = new Promise((resolve, reject) => {
    readyResolve = (message) =>
      message.ready ? resolve(message) : reject(new Error(message.error ?? 'worker failed'));
    child.once('exit', (code) => reject(new Error(`worker exited ${code}`)));
  });
  const command = (cmd, extra = {}) =>
    new Promise((resolve, reject) => {
      const id = nextId++;
      pending.set(id, resolve);
      child.send({ id, cmd, ...extra });
      setTimeout(() => {
        if (pending.has(id)) {
          pending.delete(id);
          reject(new Error(`${cmd} timed out`));
        }
      }, 60000);
    });
  return { child, ready, command };
}

function get(port, route) {
  return new Promise((resolve, reject) => {
    const request = http.get(`http://127.0.0.1:${port}${route}`, (response) => {
      const chunks = [];
      response.on('data', (c) => chunks.push(c));
      response.on('end', () =>
        resolve({
          status: response.statusCode,
          headers: response.headers,
          body: Buffer.concat(chunks).toString('utf8'),
        })
      );
    });
    request.on('error', reject);
  });
}

/** The scan clocks the two readers cannot share; the reader corpus proves the scan logic. */
const CLOCK_KEYS = new Set([
  'scannedAt',
  'asOf',
  'asOfMs',
  'firstSeenAt',
  'generatedAt',
  'ageSeconds',
  'cacheAgeSeconds',
  'readAt',
  'since',
  'cpuReadAt',
  'diskReadAt',
  'memoryReadAt',
  'boards',
]);
/** The two stores' own identities; the fixture homes differ by design. */
const ID_KEYS = new Set(['instanceId', 'openedAt']);
function normalize(value) {
  if (Array.isArray(value)) return value.map(normalize);
  if (value && typeof value === 'object') {
    const out = {};
    for (const [key, item] of Object.entries(value))
      out[key] = CLOCK_KEYS.has(key) || ID_KEYS.has(key) ? null : normalize(item);
    return out;
  }
  if (typeof value === 'string')
    return value.split(homes.today).join('<home>').split(homes.owned).join('<home>');
  return value;
}
const sameJson = (a, b) =>
  JSON.stringify(normalize(JSON.parse(a))) === JSON.stringify(normalize(JSON.parse(b)));

const report = { steps: [], failures: [] };
const workers = [];
try {
  const today = startWorker('today');
  const owned = startWorker('owned');
  workers.push(today, owned);
  const [todayReady, ownedReady] = await Promise.all([today.ready, owned.ready]);

  async function compare(name, route, { view = null } = {}) {
    const a = await get(todayReady.port, route);
    const b = await get(ownedReady.port, route);
    const served = view === null || b.headers['x-aac-view'] === view;
    report.steps.push({ name, status: b.status, bytes: b.body.length, served });
    if (a.status !== 200 || b.status !== 200)
      report.failures.push(`${name}: status ${a.status}/${b.status}`);
    else if (!sameJson(a.body, b.body)) {
      const pa = normalize(JSON.parse(a.body));
      const pb = normalize(JSON.parse(b.body));
      const keys = [...new Set([...Object.keys(pa), ...Object.keys(pb)])];
      const diff = keys.filter((k) => JSON.stringify(pa[k]) !== JSON.stringify(pb[k]));
      const k = diff[0];
      const sample = (v) => JSON.stringify(v ?? null).slice(0, 220);
      report.failures.push(
        `${name}: differs at [${diff.join(', ')}]; ${k}: today=${sample(pa[k])} owned=${sample(pb[k])}`
      );
      if (!report.diffDir) {
        const dump = path.join(process.env.TMPDIR ?? os.tmpdir(), 'aac-r4-diff');
        fs.mkdirSync(dump, { recursive: true });
        report.diffDir = dump;
        fs.writeFileSync(path.join(dump, 'diff-today.json'), a.body);
        fs.writeFileSync(path.join(dump, 'diff-owned.json'), b.body);
      }
    }
    if (!served) report.failures.push(`${name}: the owned path did not serve (x-aac-view)`);
    return b;
  }

  await today.command('scan');
  await owned.command('wait-owned');
  await owned.command('collect');
  // Let both owners' first collector tick (pinned, machines, simlock) land before comparing.
  await new Promise((resolve) => setTimeout(resolve, 1500));
  await compare('first status read', '/api/status', { view: 'status@1' });
  report.pinsProbe = await owned.command('pins');
  await compare('first scoreboard read', '/api/scoreboard', { view: 'scoreboard@1' });
  await compare(
    'filtered scoreboard',
    `/api/scoreboard?source=real&taskType=${encodeURIComponent(taskType)}`,
    { view: 'scoreboard@1' }
  );
  report.diag = await owned.command('diag');
  const inputsProbe = await owned.command('inputs');
  report.inputsProbe = {
    statusGeneration: inputsProbe.status?.generation ?? null,
    scoreboardGeneration: inputsProbe.scoreboard?.generation ?? null,
    scoreboardCatalog: inputsProbe.scoreboard?.catalog ? 'present' : 'null',
  };
  report.factsProbe = await owned.command('facts');

  // A new report on both shares; each side rescans in its own way.
  for (const mode of ['today', 'owned']) {
    const file = path.join(shares[mode], 'v1', 'writers', 'mac.second', 'thread.json');
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(
      file,
      JSON.stringify({
        schema: 'agent-reports/thread@1',
        writer: 'mac.second',
        seq: 1,
        writtenAt: iso(NOW - 5000),
        state: 'blocked',
        agent: { name: 'second' },
        links: [],
      })
    );
  }
  await today.command('scan');
  await owned.command('wait-owned', { marker: 'mac.second' });
  await compare('a new report scanned', '/api/status', { view: 'status@1' });

  // Ingest parity: the store's runs audit after each side's passes over the same share.
  const auditToday = await today.command('audit');
  const auditOwned = await owned.command('audit');
  report.steps.push({ name: 'runs audit', ok: auditToday.ok && auditOwned.ok });
  if (!auditToday.ok || !auditOwned.ok) report.failures.push('runs audit: a side refused');
  else if (
    JSON.stringify(normalize(auditToday.data)) !== JSON.stringify(normalize(auditOwned.data))
  )
    report.failures.push('runs audit: the owned ingest differs from the TS ingest');

  // The readers-check file: one file per writer, the same rows up to the read clocks.
  await new Promise((resolve) => setTimeout(resolve, 1500));
  const checkToday = await today.command('check');
  const checkOwned = await owned.command('check');
  const checkNames = Object.keys(checkToday.files).sort();
  report.steps.push({ name: 'readers check', writers: checkNames });
  const ownedNames = Object.keys(checkOwned.files).sort();
  if (JSON.stringify(ownedNames) !== JSON.stringify(checkNames))
    report.failures.push(
      `readers check: different writer files (today [${checkNames}] owned [${ownedNames}])`
    );
  for (const name of checkNames) {
    const a = normalize(JSON.parse(checkToday.files[name]));
    const b = normalize(JSON.parse(checkOwned.files[name] ?? 'null'));
    if (JSON.stringify(a) !== JSON.stringify(b))
      report.failures.push(`readers check: ${name} differs`);
  }

  // The kill drill: the service stops; the TS share readers return and the page answers.
  await owned.command('stop-rust');
  await new Promise((resolve) => setTimeout(resolve, 35000));
  const after = await get(ownedReady.port, '/api/status');
  const body = JSON.parse(after.body);
  const hasData = after.body.includes('vm.fixture');
  report.steps.push({ name: 'kill drill', status: after.status, hasData });
  if (after.status !== 200 || body.version !== 1 || !hasData)
    report.failures.push('kill drill: the TS readers did not return with data');
} catch (error) {
  report.failures.push(String(error && error.stack ? error.stack : error));
} finally {
  for (const worker of workers) {
    try {
      worker.child.send({ cmd: 'stop' });
    } catch {
      /* already gone */
    }
    worker.child.kill();
  }
  fs.rmSync(scratch, { recursive: true, force: true });
}

console.log(JSON.stringify(report, null, 2));
if (report.failures.length > 0) {
  console.error(`owned-read-parity: ${report.failures.length} failure(s)`);
  process.exit(1);
}
console.log(`owned-read-parity: ${report.steps.length} steps equal or served`);
